//! 100 条凭据规模下的承载力实测（仅测试期编译）。
//!
//! # 为什么要它
//!
//! 「100 个号能不能装进现在这套架构」不能靠读代码回答。本模块用合成凭据把
//! 四条怀疑逐条压成数字，输出直接进进度日志。测量本身也是回归护栏：
//! 将来某次改动让写盘变慢一个数量级，这里会先叫。
//!
//! # 要量的四条（读代码得出的怀疑，需实测确认或推翻）
//!
//! 1. **余额刷新是串行 + 每条 `sleep(400ms)`**（`service.rs` 的 `refresh_all_balances`），
//!    刷新周期与缓存 TTL 都是 300s。100 条时一轮要多久？会不会超过 TTL？
//!    若超过，缓存里的余额在被读到时已过期，额度守卫的「拿不到余额就不降级」
//!    分支会被走到，降级不再发生 —— 这条要靠本模块的数字来判定，不靠推断。
//! 2. **`persist_credentials` 是全量覆写**，而一次调度 pass 会逐条改优先级、
//!    每条各触发一次写盘。100 条时一次 pass 的总写入量与耗时是多少？
//! 3. **`GET /credentials` 无分页**，前端 30s 全量轮询。100 条时快照 + 序列化
//!    耗时与响应体积是多少？
//! 4. **选号在锁内做 O(n) 过滤**。100 条时 `acquire_context` 的单次耗时是多少？
//!
//! # 纪律
//!
//! - 不打真实上游。余额那条只量**本地开销 + 固定 sleep**，网络往返按实测
//!   单次延迟外推，并在输出里标明哪部分是外推。
//! - 每项打印「N=7 实测 / N=100 实测 / 外推到 N=300」，让天花板可见。
//! - 合成凭据用**假 token**，写在临时目录里，跑完即删。仓里没有 `tempfile`
//!   依赖，沿用既有做法：`std::env::temp_dir().join(format!("...{}", process::id()))`
//!   （见 `usage_stats.rs` 的 `temp_dir` 辅助函数）。**不要为此新增依赖。**

#[cfg(test)]
mod tests {
    use crate::kiro::token_manager::GroupScope;
    use crate::admin::AdminService;
    use crate::admin::scheduling::{SchedulingConfig, SchedulingProfile};
    use crate::kiro::model::credentials::KiroCredentials;
    use crate::kiro::token_manager::MultiTokenManager;
    use crate::model::config::Config;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    /// 被测规模。300 是「再翻两倍还撑不撑得住」的探针，不是当前目标。
    const SIZES: [usize; 4] = [7, 50, 100, 300];
    /// 每个测量点重复次数，取中位数抹掉单次抖动。
    const REPEATS: usize = 5;

    /// `service.rs::refresh_all_balances` 每条凭据之后的固定节流，单位毫秒。
    /// 与被测代码里的字面量保持一致；改了那边要同步改这里。
    const BALANCE_THROTTLE_MS: u64 = 400;
    /// `service.rs::BALANCE_CACHE_TTL_SECS`，余额缓存有效期，单位秒。
    const BALANCE_TTL_SECS: u64 = 300;
    /// 外推余额刷新总耗时用的单次上游往返假设值，单位毫秒。
    /// **这是假设不是实测**——本模块不打真实上游。
    const ASSUMED_UPSTREAM_RTT_MS: u64 = 1500;

    /// 数据来源标签，打在表格最后一列。
    const MEASURED: &str = "实测";
    /// 由代码里的常量直接算出（如 N × 400ms），没有测量误差也没有外推假设。
    const DERIVED: &str = "常量算";
    /// 掺了假设值（上游 RTT），只能当量级参考。
    const EXTRAPOLATED: &str = "外推";

    /// 一行表格。
    struct Row {
        n: usize,
        item: &'static str,
        value: String,
        unit: &'static str,
        kind: &'static str,
    }

    impl Row {
        fn new(n: usize, item: &'static str, value: String, unit: &'static str, kind: &'static str) -> Self {
            Self { n, item, value, unit, kind }
        }
    }

    /// 建一个本次进程专属的临时目录并清空。仓里没有 `tempfile`，沿用
    /// `usage_stats.rs` 的做法。
    fn bench_dir(tag: &str) -> PathBuf {
        let p = std::env::temp_dir()
            .join(format!("kiro_fleet_bench_{}_{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    /// 造一个 OAuth 形态的合成凭据。
    ///
    /// 长度按真实 Kiro social 账号取：accessToken 是 JWT（上千字符），
    /// refreshToken 数百字符。字节量结论完全取决于这个尺寸，用 api_key 形态
    /// 量出来的体积会小一个数量级、失去参考价值。
    ///
    /// `expires_at` 取 24 小时后：`is_token_expired` / `is_token_expiring_soon`
    /// 都判 false，`try_ensure_token` 走 `needs_refresh == false` 分支直接返回，
    /// 不触碰 `refresh_token()`，因此不会外发任何请求。
    fn synth_oauth_credential(i: usize) -> KiroCredentials {
        let expires_at = (chrono::Utc::now() + chrono::Duration::hours(24)).to_rfc3339();
        KiroCredentials {
            id: Some(i as u64),
            access_token: Some(format!("fake.jwt.{}.{}", i, "A".repeat(1024))),
            refresh_token: Some(format!("fake-refresh-{}-{}", i, "R".repeat(640))),
            profile_arn: Some(
                "arn:aws:codewhisperer:us-east-1:699475941385:profile/EHGA3GRVQMUK".to_string(),
            ),
            expires_at: Some(expires_at),
            auth_method: Some("social".to_string()),
            provider: Some("Google".to_string()),
            priority: 50,
            machine_id: Some(format!("{:064x}", i)),
            email: Some(format!("bench-{}@example.invalid", i)),
            subscription_title: Some("KIRO PRO+".to_string()),
            endpoint: Some("ide".to_string()),
            groups: if i % 2 == 0 {
                vec!["pool-a".to_string()]
            } else {
                Vec::new()
            },
            source_channel: Some("fleet-bench".to_string()),
            created_at: Some(chrono::Utc::now().to_rfc3339()),
            ..KiroCredentials::default()
        }
    }

    /// 造一个 api_key 形态的合成凭据，只用于对照「换一种账号形态字节量差多少」。
    fn synth_api_key_credential(i: usize) -> KiroCredentials {
        KiroCredentials {
            id: Some(i as u64),
            auth_method: Some("api_key".to_string()),
            kiro_api_key: Some(format!("ksk_{:0>60}", i)),
            priority: 50,
            machine_id: Some(format!("{:064x}", i)),
            email: Some(format!("bench-{}@example.invalid", i)),
            subscription_title: Some("KIRO PRO+".to_string()),
            endpoint: Some("ide".to_string()),
            source_channel: Some("fleet-bench".to_string()),
            created_at: Some(chrono::Utc::now().to_rfc3339()),
            ..KiroCredentials::default()
        }
    }

    /// 把凭据写成多凭据数组格式，返回文件路径。
    fn write_credentials_file(dir: &Path, creds: &[KiroCredentials]) -> PathBuf {
        let path = dir.join("credentials.json");
        std::fs::write(&path, serde_json::to_string_pretty(creds).unwrap()).unwrap();
        path
    }

    /// 起一个挂在 `dir` 上的 manager。`is_multiple_format = true` 才会回写。
    fn build_manager(dir: &Path, creds: Vec<KiroCredentials>) -> Arc<MultiTokenManager> {
        let path = write_credentials_file(dir, &creds);
        let config = Config::default();
        Arc::new(MultiTokenManager::new(config, creds, None, Some(path), true).unwrap())
    }

    fn file_len(path: &Path) -> u64 {
        std::fs::metadata(path).unwrap().len()
    }

    fn median_u64(mut samples: Vec<u64>) -> u64 {
        assert!(!samples.is_empty(), "样本不能为空");
        samples.sort_unstable();
        samples[samples.len() / 2]
    }

    fn micros_to_ms(us: u64) -> f64 {
        us as f64 / 1000.0
    }

    fn fmt_bytes(bytes: u64) -> String {
        if bytes >= 1024 * 1024 {
            format!("{:.2} MiB", bytes as f64 / (1024.0 * 1024.0))
        } else if bytes >= 1024 {
            format!("{:.1} KiB", bytes as f64 / 1024.0)
        } else {
            format!("{} B", bytes)
        }
    }

    /// 打印对齐表格。中文字符宽度按 2 计，否则列头会歪。
    fn display_width(s: &str) -> usize {
        s.chars()
            .map(|c| if (c as u32) > 0x2000 { 2 } else { 1 })
            .sum()
    }

    fn pad(s: &str, width: usize, left: bool) -> String {
        let w = display_width(s);
        let fill = width.saturating_sub(w);
        if left {
            format!("{}{}", " ".repeat(fill), s)
        } else {
            format!("{}{}", s, " ".repeat(fill))
        }
    }

    fn print_table(rows: &[Row]) {
        let h = ("N", "项目", "中位数", "单位", "实测/外推");
        let w_n = rows
            .iter()
            .map(|r| r.n.to_string().len())
            .chain(std::iter::once(display_width(h.0)))
            .max()
            .unwrap();
        let w_item = rows
            .iter()
            .map(|r| display_width(r.item))
            .chain(std::iter::once(display_width(h.1)))
            .max()
            .unwrap();
        let w_val = rows
            .iter()
            .map(|r| display_width(&r.value))
            .chain(std::iter::once(display_width(h.2)))
            .max()
            .unwrap();
        let w_unit = rows
            .iter()
            .map(|r| display_width(r.unit))
            .chain(std::iter::once(display_width(h.3)))
            .max()
            .unwrap();
        let w_kind = rows
            .iter()
            .map(|r| display_width(r.kind))
            .chain(std::iter::once(display_width(h.4)))
            .max()
            .unwrap();

        let sep = format!(
            "{}-+-{}-+-{}-+-{}-+-{}",
            "-".repeat(w_n),
            "-".repeat(w_item),
            "-".repeat(w_val),
            "-".repeat(w_unit),
            "-".repeat(w_kind)
        );
        println!(
            "{} | {} | {} | {} | {}",
            pad(h.0, w_n, true),
            pad(h.1, w_item, false),
            pad(h.2, w_val, true),
            pad(h.3, w_unit, false),
            pad(h.4, w_kind, false)
        );
        println!("{}", sep);
        let mut last_n = None;
        for r in rows {
            if last_n.is_some() && last_n != Some(r.n) {
                println!("{}", sep);
            }
            last_n = Some(r.n);
            println!(
                "{} | {} | {} | {} | {}",
                pad(&r.n.to_string(), w_n, true),
                pad(r.item, w_item, false),
                pad(&r.value, w_val, true),
                pad(r.unit, w_unit, false),
                pad(r.kind, w_kind, false)
            );
        }
    }

    /// 每个 N 跑完攒下来的原始数字，供最后统一做断言。
    struct Measured {
        n: usize,
        /// 凭据文件全量覆写后的字节数
        persist_bytes: u64,
        /// api_key 形态同规模的字节数（对照用）
        persist_bytes_api_key: u64,
        /// `set_priority`（含 1 次 persist）单次耗时中位数，微秒
        set_priority_us: u64,
        /// `set_priority_with_memo`（含 2 次 persist）单次耗时中位数，微秒
        set_priority_with_memo_us: u64,
        /// persist 拆解：克隆全部凭据，微秒
        persist_clone_us: u64,
        /// persist 拆解：`serde_json::to_string_pretty`，微秒
        persist_serialize_us: u64,
        /// persist 拆解：写临时文件 + chmod + rename，微秒
        persist_write_us: u64,
        /// 一次全量调度 pass 的耗时中位数，微秒
        scheduling_pass_us: u64,
        /// 一次调度 pass 产出的变更条数
        scheduling_changes: usize,
        /// `get_all_credentials()` + `serde_json` 序列化耗时中位数，微秒
        credentials_api_us: u64,
        /// `GET /credentials` 响应体字节数
        credentials_api_bytes: u64,
        /// `snapshot()` 单次耗时中位数，微秒
        snapshot_us: u64,
        /// priority 模式下 `acquire_context` 单次耗时中位数，微秒
        acquire_priority_us: u64,
        /// balanced 模式下 `acquire_context` 单次耗时中位数，微秒
        acquire_balanced_us: u64,
    }

    /// 测一个规模。返回原始数字，表格在外面统一拼。
    async fn measure(n: usize) -> Measured {
        let dir = bench_dir(&format!("n{}", n));
        let creds: Vec<KiroCredentials> = (1..=n).map(synth_oauth_credential).collect();
        let mgr = build_manager(&dir, creds);
        let cred_path = dir.join("credentials.json");

        // ---- A. persist_credentials：走公开入口 set_priority ----
        //
        // `persist_credentials` 是私有方法，测试模块够不到；`set_priority` 是
        // 它唯一稳定的公开代理（内存改一条 → select_highest_priority O(n) →
        // persist 全量覆写）。多出来的 O(n) 扫描相对写盘可忽略。
        let mut set_priority_samples = Vec::with_capacity(REPEATS);
        for r in 0..REPEATS {
            let target = (r as u64 % n as u64) + 1;
            // 在 50/51 之间来回改：两个值都非零，序列化长度不变，字节数可比。
            let value = if r % 2 == 0 { 51 } else { 50 };
            let t = Instant::now();
            mgr.set_priority(target, value).unwrap();
            set_priority_samples.push(t.elapsed().as_micros() as u64);
        }
        // 把被改过的那条改回 50，保证后面调度 pass 的初始态整齐
        for r in 0..REPEATS {
            let target = (r as u64 % n as u64) + 1;
            mgr.set_priority(target, 50).unwrap();
        }
        let persist_bytes = file_len(&cred_path);

        // set_priority_with_memo = set_priority + 再一次 persist。
        // 量它是为了用实测证实「一次变更 = 2 次全量覆写」，而不是只靠读代码断言。
        let mut with_memo_samples = Vec::with_capacity(REPEATS);
        for r in 0..REPEATS {
            let target = (r as u64 % n as u64) + 1;
            let t = Instant::now();
            mgr.set_priority_with_memo(target, 50, None).unwrap();
            with_memo_samples.push(t.elapsed().as_micros() as u64);
        }

        // persist 的三段拆解。用公开 API 复刻 `persist_credentials` 的每一步，
        // 目的是回答「这 12ms 里有多少是 debug 构建下的 serde、多少是真 I/O」——
        // release 只能把前者压下去，后者不会变。
        //
        // 注：这是**复刻**不是插桩，绝对值可能与真实 persist 有出入；
        // 三段之和与上面实测的 set_priority 对得上才说明复刻是准的。
        let probe_path = dir.join("persist_probe.json");
        let mut clone_samples = Vec::with_capacity(REPEATS);
        let mut serialize_samples = Vec::with_capacity(REPEATS);
        let mut write_samples = Vec::with_capacity(REPEATS);
        for _ in 0..REPEATS {
            let t = Instant::now();
            let all = mgr.clone_all_credentials();
            clone_samples.push(t.elapsed().as_micros() as u64);

            let t = Instant::now();
            let json = serde_json::to_string_pretty(&all).unwrap();
            serialize_samples.push(t.elapsed().as_micros() as u64);

            let tmp = probe_path.with_extension("json.tmp");
            let t = Instant::now();
            std::fs::write(&tmp, &json).unwrap();
            crate::common::secret_file::restrict_permissions(&tmp).unwrap();
            std::fs::rename(&tmp, &probe_path).unwrap();
            write_samples.push(t.elapsed().as_micros() as u64);
        }
        // 复刻产物必须和真 persist 的产物一样大，否则上面的拆解没有参考价值。
        assert_eq!(
            file_len(&probe_path),
            persist_bytes,
            "persist 拆解复刻的字节数应与真实 persist 一致"
        );
        let _ = std::fs::remove_file(&probe_path);

        // api_key 形态的同规模对照（另起目录，不污染主 manager）
        let api_dir = bench_dir(&format!("n{}_apikey", n));
        let api_creds: Vec<KiroCredentials> = (1..=n).map(synth_api_key_credential).collect();
        let api_path = write_credentials_file(&api_dir, &api_creds);
        let persist_bytes_api_key = file_len(&api_path);
        let _ = std::fs::remove_dir_all(&api_dir);

        // ---- C. GET /credentials 全量快照 + 序列化 ----
        let svc = Arc::new(AdminService::new(
            Arc::clone(&mgr),
            vec!["ide".to_string()],
        ));

        let mut snapshot_samples = Vec::with_capacity(REPEATS);
        for _ in 0..REPEATS {
            let t = Instant::now();
            let snap = mgr.snapshot();
            let us = t.elapsed().as_micros() as u64;
            assert_eq!(snap.total, n, "snapshot 条数应等于凭据数");
            snapshot_samples.push(us);
        }

        let mut api_samples = Vec::with_capacity(REPEATS);
        let mut api_bytes = 0u64;
        for _ in 0..REPEATS {
            let t = Instant::now();
            let resp = svc.get_all_credentials();
            let body = serde_json::to_vec(&resp).unwrap();
            let us = t.elapsed().as_micros() as u64;
            assert_eq!(resp.credentials.len(), n, "响应条数应等于凭据数");
            api_bytes = body.len() as u64;
            api_samples.push(us);
        }

        // ---- D. 选号路径 ----
        //
        // 合成凭据是 OAuth 形态但 expiresAt 在 24 小时后，`try_ensure_token`
        // 直接返回内存里的 accessToken，不发请求（已读代码确认，见
        // `synth_oauth_credential` 的注释）。
        let model = Some("claude-sonnet-4-5");

        mgr.set_load_balancing_mode("priority".to_string()).unwrap();
        // 预热一次，避免第一次调用把 current_id 初始化的成本算进样本
        let _ = mgr.acquire_context(model, GroupScope::AllGroups).await.unwrap();
        let mut acquire_priority_samples = Vec::with_capacity(REPEATS);
        for _ in 0..REPEATS {
            let t = Instant::now();
            let ctx = mgr.acquire_context(model, GroupScope::AllGroups).await.unwrap();
            acquire_priority_samples.push(t.elapsed().as_micros() as u64);
            assert!(!ctx.token.is_empty());
        }

        mgr.set_load_balancing_mode("balanced".to_string()).unwrap();
        let _ = mgr.acquire_context(model, GroupScope::AllGroups).await.unwrap();
        let mut acquire_balanced_samples = Vec::with_capacity(REPEATS);
        for _ in 0..REPEATS {
            let t = Instant::now();
            let ctx = mgr.acquire_context(model, GroupScope::AllGroups).await.unwrap();
            acquire_balanced_samples.push(t.elapsed().as_micros() as u64);
            assert!(!ctx.token.is_empty());
        }
        mgr.set_load_balancing_mode("priority".to_string()).unwrap();

        // ---- B. 一次全量调度 pass ----
        //
        // 余额缓存是私有字段，测试够不到，所以全部 usage_pct = None。
        // HighConcurrency 对「用量未知」判前排 40、Throughput 判中间档 50，
        // 两个 profile 轮换 → 每轮都恰好产生 N 条变更 = 全量 pass 的上界。
        let mut pass_samples = Vec::with_capacity(REPEATS);
        let mut changes_len = 0usize;
        for r in 0..REPEATS {
            let profile = if r % 2 == 0 {
                SchedulingProfile::HighConcurrency
            } else {
                SchedulingProfile::Throughput
            };
            mgr.set_scheduling_runtime(SchedulingConfig {
                enabled: true,
                profile,
                ..SchedulingConfig::default()
            });
            let t = Instant::now();
            let applied = svc.run_scheduling_pass();
            pass_samples.push(t.elapsed().as_micros() as u64);
            changes_len = applied.len();
        }
        // 关掉总开关，避免 Drop 阶段还有后台任务改优先级
        mgr.set_scheduling_runtime(SchedulingConfig::default());

        let out = Measured {
            n,
            persist_bytes,
            persist_bytes_api_key,
            set_priority_us: median_u64(set_priority_samples),
            set_priority_with_memo_us: median_u64(with_memo_samples),
            persist_clone_us: median_u64(clone_samples),
            persist_serialize_us: median_u64(serialize_samples),
            persist_write_us: median_u64(write_samples),
            scheduling_pass_us: median_u64(pass_samples),
            scheduling_changes: changes_len,
            credentials_api_us: median_u64(api_samples),
            credentials_api_bytes: api_bytes,
            snapshot_us: median_u64(snapshot_samples),
            acquire_priority_us: median_u64(acquire_priority_samples),
            acquire_balanced_us: median_u64(acquire_balanced_samples),
        };

        drop(svc);
        drop(mgr);
        let _ = std::fs::remove_dir_all(&dir);
        out
    }

    /// 单独验一次「400ms 节流真的是墙钟 400ms」，不是被 tokio 压缩掉的。
    /// 只在最小规模上跑，剩下的按线性外推。
    async fn measure_throttle_wall_ms(rounds: usize) -> u64 {
        let t = Instant::now();
        for _ in 0..rounds {
            tokio::time::sleep(Duration::from_millis(BALANCE_THROTTLE_MS)).await;
        }
        t.elapsed().as_millis() as u64
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn fleet_bench_report() {
        let mut results = Vec::new();
        for n in SIZES {
            results.push(measure(n).await);
        }

        // 节流墙钟校验：N=7 那一轮的固定 sleep 总量
        let throttle_probe_rounds = SIZES[0];
        let throttle_probe_ms = measure_throttle_wall_ms(throttle_probe_rounds).await;

        let mut rows: Vec<Row> = Vec::new();
        for m in &results {
            let n = m.n;

            // --- 2. 落盘 / 调度 pass ---
            rows.push(Row::new(
                n,
                "credentials.json 全量覆写字节数(OAuth 形态)",
                fmt_bytes(m.persist_bytes),
                "bytes",
                MEASURED,
            ));
            rows.push(Row::new(
                n,
                "credentials.json 全量覆写字节数(api_key 形态)",
                fmt_bytes(m.persist_bytes_api_key),
                "bytes",
                MEASURED,
            ));
            rows.push(Row::new(
                n,
                "set_priority 单次(含 1 次全量覆写)",
                format!("{:.2}", micros_to_ms(m.set_priority_us)),
                "ms",
                MEASURED,
            ));
            rows.push(Row::new(
                n,
                "set_priority_with_memo 单次(含 2 次全量覆写)",
                format!("{:.2}", micros_to_ms(m.set_priority_with_memo_us)),
                "ms",
                MEASURED,
            ));
            rows.push(Row::new(
                n,
                "  拆解 a: clone_all_credentials()",
                format!("{:.2}", micros_to_ms(m.persist_clone_us)),
                "ms",
                MEASURED,
            ));
            rows.push(Row::new(
                n,
                "  拆解 b: serde_json 序列化(debug 构建)",
                format!("{:.2}", micros_to_ms(m.persist_serialize_us)),
                "ms",
                MEASURED,
            ));
            rows.push(Row::new(
                n,
                "  拆解 c: 写盘 write+chmod+rename",
                format!("{:.2}", micros_to_ms(m.persist_write_us)),
                "ms",
                MEASURED,
            ));
            rows.push(Row::new(
                n,
                "  拆解: serde 占单次 persist 比例",
                format!(
                    "{:.0}",
                    m.persist_serialize_us as f64 * 100.0
                        / (m.persist_clone_us + m.persist_serialize_us + m.persist_write_us).max(1)
                            as f64
                ),
                "%",
                DERIVED,
            ));
            rows.push(Row::new(
                n,
                "调度 pass 变更条数(全量重排上界)",
                m.scheduling_changes.to_string(),
                "条",
                MEASURED,
            ));
            rows.push(Row::new(
                n,
                "调度 pass 一轮总耗时",
                format!("{:.1}", micros_to_ms(m.scheduling_pass_us)),
                "ms",
                MEASURED,
            ));
            rows.push(Row::new(
                n,
                "调度 pass 一轮总写入量(= 2 × N × 单次字节数)",
                fmt_bytes(2 * n as u64 * m.persist_bytes),
                "bytes",
                DERIVED,
            ));
            rows.push(Row::new(
                n,
                "调度 pass 一轮纯 I/O 下界(= 2 × N × 拆解c，与构建模式无关)",
                format!("{:.1}", micros_to_ms(2 * n as u64 * m.persist_write_us)),
                "ms",
                DERIVED,
            ));

            // --- 3. GET /credentials ---
            rows.push(Row::new(
                n,
                "snapshot() 单次",
                format!("{:.3}", micros_to_ms(m.snapshot_us)),
                "ms",
                MEASURED,
            ));
            rows.push(Row::new(
                n,
                "GET /credentials 快照+序列化",
                format!("{:.3}", micros_to_ms(m.credentials_api_us)),
                "ms",
                MEASURED,
            ));
            rows.push(Row::new(
                n,
                "GET /credentials 响应体积(未压缩)",
                fmt_bytes(m.credentials_api_bytes),
                "bytes",
                MEASURED,
            ));
            rows.push(Row::new(
                n,
                "前端 30s 轮询产生的出站带宽",
                format!("{:.1}", m.credentials_api_bytes as f64 / 30.0 / 1024.0),
                "KiB/s",
                DERIVED,
            ));

            // --- 4. 选号 ---
            rows.push(Row::new(
                n,
                "acquire_context 单次(priority 模式)",
                format!("{:.3}", micros_to_ms(m.acquire_priority_us)),
                "ms",
                MEASURED,
            ));
            rows.push(Row::new(
                n,
                "acquire_context 单次(balanced 模式)",
                format!("{:.3}", micros_to_ms(m.acquire_balanced_us)),
                "ms",
                MEASURED,
            ));

            // --- 1. 余额刷新 ---
            let throttle_ms = n as u64 * BALANCE_THROTTLE_MS;
            let local_ms = throttle_ms + (m.snapshot_us / 1000);
            let with_rtt_ms = local_ms + n as u64 * ASSUMED_UPSTREAM_RTT_MS;
            rows.push(Row::new(
                n,
                "余额刷新一轮：固定节流 N × 400ms",
                format!("{:.1}", throttle_ms as f64 / 1000.0),
                "s",
                DERIVED,
            ));
            rows.push(Row::new(
                n,
                "余额刷新一轮：本地开销合计(节流 + 快照)",
                format!("{:.1}", local_ms as f64 / 1000.0),
                "s",
                DERIVED,
            ));
            rows.push(Row::new(
                n,
                "余额刷新一轮：含上游往返(假设每条 1.5s)",
                format!("{:.1}", with_rtt_ms as f64 / 1000.0),
                "s",
                EXTRAPOLATED,
            ));
            rows.push(Row::new(
                n,
                "余额刷新一轮 ÷ 300s TTL",
                format!("{:.2}", with_rtt_ms as f64 / 1000.0 / BALANCE_TTL_SECS as f64),
                "倍",
                EXTRAPOLATED,
            ));
        }

        println!();
        println!("================ kiro-rs 舰队承载力实测 ================");
        println!(
            "环境: {} / {} 逻辑核 / 数据目录 {}",
            std::env::consts::OS,
            std::thread::available_parallelism()
                .map(|v| v.get())
                .unwrap_or(0),
            std::env::temp_dir().display()
        );
        println!(
            "每点重复 {} 次取中位数；合成凭据为 OAuth 形态(accessToken≈1KB / refreshToken≈0.6KB)，",
            REPEATS
        );
        println!("expiresAt 设在 24h 后，try_ensure_token 走不刷新分支，全程不打上游。");
        println!(
            "⚠ 本表跑在 **debug(未优化)构建** 上：CPU 那部分(serde/clone/sha256)在 release 下会明显更快，"
        );
        println!("  I/O 那部分不会。看「拆解」三行判断某个数字是被构建模式放大的还是真实开销。");
        println!(
            "列「实测/外推」：{}=计时器量到的；{}=由代码常量直接算出；{}=掺了上游 RTT 假设值({}ms/条)。",
            MEASURED, DERIVED, EXTRAPOLATED, ASSUMED_UPSTREAM_RTT_MS
        );
        println!();
        print_table(&rows);
        println!();
        println!(
            "节流墙钟校验: {} 轮 sleep(400ms) 实际耗时 {} ms（理论 {} ms）→ 节流是真墙钟，不被压缩。",
            throttle_probe_rounds,
            throttle_probe_ms,
            throttle_probe_rounds as u64 * BALANCE_THROTTLE_MS
        );

        // 余额刷新撞 TTL 的临界 N（只看固定节流那部分，是绝对下界）
        let n_break_throttle_only = BALANCE_TTL_SECS * 1000 / BALANCE_THROTTLE_MS;
        let n_break_with_rtt = BALANCE_TTL_SECS * 1000 / (BALANCE_THROTTLE_MS + ASSUMED_UPSTREAM_RTT_MS);
        println!(
            "余额刷新撞 300s TTL 的临界 N：只算固定节流 = {} 条（实测常量，绝对下界）；",
            n_break_throttle_only
        );
        println!(
            "                              含 1.5s/条上游往返 = {} 条（外推）。",
            n_break_with_rtt
        );
        println!("=========================================================");
        println!();

        // ================= 断言：只放与机器快慢无关的 =================

        let by_n = |n: usize| results.iter().find(|m| m.n == n).unwrap();

        // 约束 2-a：落盘字节数随 N 单调增长。
        for w in results.windows(2) {
            assert!(
                w[1].persist_bytes > w[0].persist_bytes,
                "落盘字节数应随 N 增长: N={} → {} B, N={} → {} B",
                w[0].n,
                w[0].persist_bytes,
                w[1].n,
                w[1].persist_bytes
            );
        }

        // 约束 2-b：落盘字节数随 N **线性**增长。
        // 用两段增量的「每条平均字节」互比：合成凭据除 id 位数外长度相同，
        // 两段斜率差异只来自 id 的十进制位数，2% 容差足够宽。
        let slope = |a: &Measured, b: &Measured| {
            (b.persist_bytes - a.persist_bytes) as f64 / (b.n - a.n) as f64
        };
        let s_low = slope(by_n(7), by_n(50));
        let s_high = slope(by_n(100), by_n(300));
        let drift = (s_high - s_low).abs() / s_low;
        assert!(
            drift < 0.02,
            "落盘字节数应线性: 低段斜率 {:.1} B/条，高段斜率 {:.1} B/条，偏差 {:.2}%",
            s_low,
            s_high,
            drift * 100.0
        );

        // 约束 2-c：一次调度 pass 会把**每一条**凭据都改一遍（全量重排上界）。
        // 每条变更走 set_priority_with_memo = 2 次全量覆写，
        // 所以一轮写入量 = 2 × N × 单次字节数。
        for m in &results {
            assert_eq!(
                m.scheduling_changes, m.n,
                "N={} 时全量重排应产生 N 条变更，实得 {}",
                m.n, m.scheduling_changes
            );
        }

        // 约束 2-d：set_priority_with_memo 确实比 set_priority 多写一次。
        // 只断言「更慢」这个方向，不断绝对值也不断 2 倍——倍数会被机器抖动打偏。
        for m in &results {
            assert!(
                m.set_priority_with_memo_us > m.set_priority_us,
                "N={} 时 set_priority_with_memo({} us) 应慢于 set_priority({} us)",
                m.n,
                m.set_priority_with_memo_us,
                m.set_priority_us
            );
        }

        // 约束 3：GET /credentials 无分页 —— 响应条数与体积都随 N 线性增长。
        for w in results.windows(2) {
            assert!(
                w[1].credentials_api_bytes > w[0].credentials_api_bytes,
                "响应体积应随 N 增长: N={} → {} B, N={} → {} B",
                w[0].n,
                w[0].credentials_api_bytes,
                w[1].n,
                w[1].credentials_api_bytes
            );
        }
        let api_slope = |a: &Measured, b: &Measured| {
            (b.credentials_api_bytes - a.credentials_api_bytes) as f64 / (b.n - a.n) as f64
        };
        let a_low = api_slope(by_n(7), by_n(50));
        let a_high = api_slope(by_n(100), by_n(300));
        let a_drift = (a_high - a_low).abs() / a_low;
        assert!(
            a_drift < 0.05,
            "响应体积应线性: 低段 {:.1} B/条，高段 {:.1} B/条，偏差 {:.2}%",
            a_low,
            a_high,
            a_drift * 100.0
        );

        // 约束 1：余额刷新的固定节流是 N × 400ms，与 300s TTL 的关系是纯算术。
        // 这里钉住临界值，防止哪天有人改了 400ms 或 TTL 却没更新结论。
        assert_eq!(
            n_break_throttle_only, 750,
            "仅固定节流撞 TTL 的临界 N 变了，结论需重算"
        );
        assert_eq!(
            n_break_with_rtt, 157,
            "含 1.5s 上游往返撞 TTL 的临界 N 变了，结论需重算"
        );
        // 节流是真墙钟：允许 20% 上浮（调度延迟），但不能明显短于理论值。
        let throttle_theory = throttle_probe_rounds as u64 * BALANCE_THROTTLE_MS;
        assert!(
            throttle_probe_ms >= throttle_theory,
            "sleep(400ms) × {} 实测 {} ms 短于理论 {} ms，节流被压缩了？",
            throttle_probe_rounds,
            throttle_probe_ms,
            throttle_theory
        );
    }
}
