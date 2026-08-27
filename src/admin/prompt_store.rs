//! 原始请求体留存（默认关闭）。
//!
//! # 为什么单独一个库
//!
//! `traces.db` 是**单连接互斥锁**，读查询期间会顶住请求收尾的 insert。往里塞
//! 几十上百 KB 的请求体，会让管理台的分钟级聚合变慢，而慢查询直接占着写入路径
//! 的锁——TPM 面板已经因为类似原因踩过一次。所以请求体走独立文件独立连接，
//! 出问题也只影响"查得到原文"这一件事，不影响计费与路由。
//!
//! # 为什么压缩
//!
//! 实测 2026-08 月 11.9 万次请求、prompt 合计 59.5 亿 token，
//! 原样存约 **23.8 GB/月**，gzip 后约 **5.9 GB/月**。压缩是能不能开全量的分水岭。
//!
//! # 为什么原样存而不是入库就脱敏
//!
//! 脱敏要放在"转走"那一步。入库就脱敏的话原始证据就没了，
//! 真要追责时反而说不清。顺序是：原样存 → 保留期内可追责 → 转走时按需脱敏或删除。
//!
//! # 边界
//!
//! 只存**请求体**，不存任何请求头——`Authorization` / `x-api-key` 一个字节都不落盘。
//! 客户在正文里自己粘贴的密钥属于其自有数据，按"原样存"处理，由保留期与访问控制兜底。

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use parking_lot::Mutex;
use rusqlite::{Connection, OptionalExtension, params};

/// 单条请求体的大小上限（压缩前）。超过就不存。
///
/// 不设上限的话，一个异常的巨型请求能瞬间吃掉几百 MB，而它对排查的价值
/// 并不比截断版更高。
const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;

pub struct PromptStore {
    /// 延迟打开：关闭状态下不创建 prompts.db，磁盘上干干净净。
    conn: Mutex<Option<Connection>>,
    path: PathBuf,
    /// 运行时开关。**必须能热切** —— 让一个设置开关要人 SSH 上去重启服务，
    /// 是把实现的限制转嫁给了使用者。
    enabled: AtomicBool,
}

impl PromptStore {
    /// 建一个留存实例。`enabled=false` 时不碰磁盘，直到被打开并写入第一条。
    pub fn new(dir: &Path, enabled: bool) -> Self {
        Self {
            conn: Mutex::new(None),
            path: dir.join("prompts.db"),
            enabled: AtomicBool::new(enabled),
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    /// 热切开关，立即生效，不需要重启。
    pub fn set_enabled(&self, on: bool) {
        self.enabled.store(on, Ordering::Relaxed);
        tracing::info!(enabled = on, "原始请求体留存开关已切换（即时生效）");
    }

    /// 取到连接；首次调用时才真正建库建表。
    fn with_conn<T>(&self, f: impl FnOnce(&Connection) -> T) -> Option<T> {
        let mut slot = self.conn.lock();
        if slot.is_none() {
            match Self::open_db(&self.path) {
                Ok(c) => *slot = Some(c),
                Err(e) => {
                    tracing::warn!("打开 prompts.db 失败: {}", e);
                    return None;
                }
            }
        }
        slot.as_ref().map(f)
    }

    fn open_db(path: &Path) -> anyhow::Result<Connection> {
        let conn = Connection::open(path)?;
        // WAL：写入与读取不互相阻塞。这个库的写入在请求收尾路径上，不能被读查询顶住。
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS prompts (
                trace_id   TEXT PRIMARY KEY,
                ts_epoch   INTEGER NOT NULL,
                key_id     INTEGER NOT NULL,
                model      TEXT,
                raw_bytes  INTEGER NOT NULL,
                body_gz    BLOB NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_prompts_ts ON prompts(ts_epoch);",
        )?;
        Ok(conn)
    }

    /// 存一条请求体。失败只 warn 不影响请求——留存是辅助能力，不能让它拖垮主路径。
    pub fn put(&self, trace_id: &str, key_id: u64, model: &str, body: &[u8]) {
        if !self.is_enabled() {
            return;
        }
        if body.len() > MAX_BODY_BYTES {
            tracing::debug!(
                trace_id,
                bytes = body.len(),
                "请求体超过留存上限，跳过"
            );
            return;
        }
        let gz = match gzip(body) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(trace_id, "请求体压缩失败: {}", e);
                return;
            }
        };
        let ts = chrono::Utc::now().timestamp();
        self.with_conn(|conn| {
            if let Err(e) = conn.execute(
                "INSERT OR REPLACE INTO prompts (trace_id, ts_epoch, key_id, model, raw_bytes, body_gz)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![trace_id, ts, key_id as i64, model, body.len() as i64, gz],
            ) {
                tracing::warn!(trace_id, "请求体落盘失败: {}", e);
            }
        });
    }

    /// 取回一条请求体（已解压）。
    pub fn get(&self, trace_id: &str) -> Option<StoredPrompt> {
        // 库还没建过就直接没有记录，不必为一次查询把文件创建出来
        if !self.path.exists() {
            return None;
        }
        let row = self.with_conn(|conn| {
            conn
            .query_row(
                "SELECT ts_epoch, key_id, model, raw_bytes, body_gz FROM prompts WHERE trace_id = ?1",
                params![trace_id],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, Option<String>>(2)?,
                        r.get::<_, i64>(3)?,
                        r.get::<_, Vec<u8>>(4)?,
                    ))
                },
            )
            .optional()
            .ok()
            .flatten()
        })??;
        let body = gunzip(&row.4).ok()?;
        Some(StoredPrompt {
            trace_id: trace_id.to_string(),
            ts_epoch: row.0,
            key_id: row.1 as u64,
            model: row.2,
            raw_bytes: row.3 as u64,
            body: String::from_utf8_lossy(&body).into_owned(),
        })
    }

    /// 删除保留期外的记录。返回删掉多少条。
    ///
    /// 不在这里 VACUUM：那会重写整个文件，几 GB 的库上要几十秒，
    /// 期间占着锁。空间由 WAL 复用，真要回收让运维脚本单独做。
    pub fn cleanup(&self, retention_days: i64) -> usize {
        if !self.path.exists() {
            return 0;
        }
        let cutoff = chrono::Utc::now().timestamp() - retention_days.max(1) * 86400;
        self.with_conn(|conn| {
            match conn.execute("DELETE FROM prompts WHERE ts_epoch < ?1", params![cutoff]) {
                Ok(n) => {
                    if n > 0 {
                        tracing::info!(deleted = n, retention_days, "已清理过期请求体");
                    }
                    n
                }
                Err(e) => {
                    tracing::warn!("清理请求体失败: {}", e);
                    0
                }
            }
        })
        .unwrap_or(0)
    }

    /// 统计：条数、原始字节合计、压缩后库大小。
    pub fn stats(&self) -> PromptStoreStats {
        if !self.path.exists() {
            return PromptStoreStats::default();
        }
        let (count, raw, oldest) = self
            .with_conn(|conn| {
                let (c, r): (i64, i64) = conn
                    .query_row(
                        "SELECT COUNT(*), COALESCE(SUM(raw_bytes), 0) FROM prompts",
                        [],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .unwrap_or((0, 0));
                let o: Option<i64> = conn
                    .query_row("SELECT MIN(ts_epoch) FROM prompts", [], |r| r.get(0))
                    .optional()
                    .ok()
                    .flatten();
                (c, r, o)
            })
            .unwrap_or((0, 0, None));
        let file_bytes = std::fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0);
        PromptStoreStats {
            count: count as u64,
            raw_bytes: raw as u64,
            file_bytes,
            oldest_ts_epoch: oldest,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredPrompt {
    pub trace_id: String,
    pub ts_epoch: i64,
    pub key_id: u64,
    pub model: Option<String>,
    /// 压缩前字节数
    pub raw_bytes: u64,
    /// 原始请求体 JSON（未脱敏）
    pub body: String,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptStoreStats {
    pub count: u64,
    /// 压缩前合计
    pub raw_bytes: u64,
    /// 库文件实际占用
    pub file_bytes: u64,
    pub oldest_ts_epoch: Option<i64>,
}

fn gzip(data: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    enc.write_all(data)?;
    enc.finish()
}

fn gunzip(data: &[u8]) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let mut dec = flate2::read::GzDecoder::new(data);
    let mut out = Vec::new();
    dec.read_to_end(&mut out)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("kiro_prompt_test_{}_{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn round_trips_a_body() {
        let d = tmp("rt");
        let s = PromptStore::new(&d, true);
        let body = r#"{"model":"claude-opus-5","messages":[{"role":"user","content":"你好"}]}"#
            .as_bytes();
        s.put("t1", 7, "claude-opus-5", body);
        let got = s.get("t1").expect("应能取回");
        assert_eq!(got.body, String::from_utf8_lossy(body));
        assert_eq!(got.key_id, 7);
        assert_eq!(got.raw_bytes, body.len() as u64);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// 开关热切：关掉就不写，打开立刻写，不需要重启。
    /// 让一个设置开关要人 SSH 上去重启服务，是把实现的限制转嫁给使用者。
    #[test]
    fn toggling_takes_effect_without_a_restart() {
        let d = tmp("toggle");
        let s = PromptStore::new(&d, false);
        s.put("off", 1, "m", b"{}");
        assert!(s.get("off").is_none(), "关闭状态不该落盘");
        assert!(!d.join("prompts.db").exists(), "关闭时连库都不该建");

        s.set_enabled(true);
        s.put("on", 1, "m", b"{}");
        assert!(s.get("on").is_some(), "打开后应立即开始记录");

        s.set_enabled(false);
        s.put("off2", 1, "m", b"{}");
        assert!(s.get("off2").is_none(), "再关掉应立即停止");
        assert!(s.get("on").is_some(), "关掉不影响已存的记录");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// 超大请求体不存：一个异常的巨型请求能瞬间吃掉几百 MB，
    /// 而它对排查的价值并不比截断版高。
    #[test]
    fn oversized_bodies_are_skipped() {
        let d = tmp("big");
        let s = PromptStore::new(&d, true);
        let big = vec![b'x'; MAX_BODY_BYTES + 1];
        s.put("t2", 1, "m", &big);
        assert!(s.get("t2").is_none(), "超限的不该落盘");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// 保留期清理只删过期的，不碰新的
    #[test]
    fn cleanup_only_removes_expired() {
        let d = tmp("clean");
        let s = PromptStore::new(&d, true);
        s.put("new", 1, "m", b"{}");
        {
            // 手工塞一条 40 天前的
            let old = chrono::Utc::now().timestamp() - 40 * 86400;
            s.with_conn(|conn| {
                conn.execute(
                    "INSERT INTO prompts (trace_id, ts_epoch, key_id, model, raw_bytes, body_gz)
                     VALUES ('old', ?1, 1, 'm', 2, ?2)",
                    params![old, gzip(b"{}").unwrap()],
                )
                .unwrap();
            })
            .expect("应能拿到连接");
        }
        assert_eq!(s.cleanup(30), 1, "应只删掉 40 天前那条");
        assert!(s.get("old").is_none());
        assert!(s.get("new").is_some(), "保留期内的不能被删");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// 压缩确实省空间——这是能不能开全量的分水岭
    #[test]
    fn compression_actually_shrinks_realistic_bodies() {
        let d = tmp("gz");
        let s = PromptStore::new(&d, true);
        // 真实 prompt 高度重复（agent 每轮重发全量上下文），压缩比很高
        let body = "The quick brown fox jumps over the lazy dog. ".repeat(2000);
        s.put("t3", 1, "m", body.as_bytes());
        let st = s.stats();
        assert_eq!(st.count, 1);
        assert!(
            st.file_bytes < st.raw_bytes / 2,
            "压缩后应显著小于原始：库 {} vs 原始 {}",
            st.file_bytes,
            st.raw_bytes
        );
        let _ = std::fs::remove_dir_all(&d);
    }
}
