# RPM / TPM 可观测性现状与设计

> 本文只覆盖 RPM/TPM 这一块。缓存缺失、流量缺失、整体架构与 Kiro 对接由另外两份分析覆盖。

## 〇、已交付（2026-08-24）

第五节那套方案已经实现。**以下各节保留为设计依据与实现前的现状记录**，不再是待办。

| 交付物 | 位置 |
|---|---|
| 分钟环采集层（120 桶、每桶 8 个 `AtomicU64`、`minute % 120` 索引、写入 O(1) 无锁） | `src/anthropic/rate_ring.rs` |
| 入口口径计数（每次外部请求 +1） | `UsageRecordHook::record`（`src/anthropic/handlers.rs`） |
| 上游口径计数（每跳 +1，含重试与故障转移） | `KiroProvider::emit_attempt`（`src/kiro/provider.rs`） |
| 读取接口 | `GET /api/admin/stats/rate` |
| 面板 | 概览页「实时速率」（`src/components/rate-panel.tsx` + `charts/rate-chart.tsx`） |

commit：采集层 `5baf3c2`，面板 `3a3e6d4`。测试 656 通过（含 8 条采集层单测）。

**实现时撞到的两条设计阶段没预见的约束：**

1. **上游计数不能挂 `TraceSink`。** `emit_attempt` 在 sink 为 `None` 时直接 `return`，挂在那里等于「关掉 traces.db → 上游 RPM 归零」，而 traces.db 恰好是用户可以从面板关的。做法是把 `emit_attempt` 从关联函数改成方法（31 个调用点跟着改），并把计数放在早退**之前**。这正是第五节「与 trace 开关解耦」那条要求的具体落地方式，比原文写的更棘手。

2. **环必须全局只有一个实例。** 环由 anthropic 路由装配时建立、塞进 `KiroProvider`，主程序再通过 `provider.rate_ring()` 取回交给 Admin。若 Admin 与 API 各建一个环，两边各只数到一半流量 —— 这个坑不是理论上的，是装配顺序自然会导致的。

**另一个判读细节**：采集层未注入时接口回 **503 而不是一堆 0**。回 0 会让调用方无法区分「没装采集层」和「真的没流量」，前端也据此分三种空状态显示。

⚠️ **尚未上生产**：线上镜像仍是 `kiro-rs:518dfbd`，不含采集层与面板。要生效需重新构建镜像并切换（约 12 秒停机），属生产处置，等人工确认。

## 一、结论先行（分析当时）

**RPM 存在，但它是一个「限流闸门」，不是一个「可观测指标」。TPM 完全不存在。**

想在面板上看到 RPM/TPM，现在**没有任何现成数据可以直接读**，必须新建采集层。

## 二、RPM 现状（代码坐实）

现有实现是按凭据的滑动窗口限流器：

| 事实 | 位置 |
|---|---|
| 每个凭据一个 `rpm_window: VecDeque<Instant>`，只存最近 60 秒被选中的时间戳 | `src/kiro/token_manager.rs:904` |
| 窗口固定 60 秒 | `src/kiro/token_manager.rs:1185` |
| **默认关闭** | `src/model/config.rs:326-328`（`default_account_rpm_limit_enabled() -> false`） |
| 默认上限 60 | `src/model/config.rs:330-332` |
| **不持久化，进程重启即清空** | `src/kiro/token_manager.rs:903`（注释明写） |
| 关闭时窗口被主动清空，连计数都不留 | `src/kiro/token_manager.rs:1823-1827` |
| 判超限：线性 filter 数窗口内条目 | `src/kiro/token_manager.rs:1732-1747` |
| 占额度：先剪过期再 push | `src/kiro/token_manager.rs:1815-1846` |
| Admin 接口只返回**配置**（enabled/limit），不返回**当前速率** | `src/admin/service.rs:2075-2079` |

四个致命限制：

1. **默认关就没有数据**。限流关闭时 `rpm_window` 被 clear，等于连原始素材都不留。
2. **只有凭据维度**。没有按客户端 Key、按模型、按分组、也没有全局 RPM。你要的"这些数据"大概率是全局 + 按 Key + 按模型。
3. **内存态、重启归零**。不能看历史曲线，只能看当下这一分钟。
4. **口径是"被选中的凭据请求次数"，不是"入口请求数"**。一次外部请求如果重试 3 跳打了 3 个凭据，会在 3 个凭据的窗口里各记一次 — 作为限流是对的（保护上游），**作为 RPM 指标是错的**（对外只发生了 1 次请求）。这个口径差异必须在设计时明确区分：**上游 RPM ≠ 入口 RPM**。

## 三、TPM 现状

全仓 grep `TPM` / `tokens_per_minute` / `tpm`：**零命中**。没有任何实现、没有任何字段。

## 四、现有数据源能不能反推出 RPM/TPM

| 数据源 | 时间粒度 | 能否做分钟级 | 障碍 |
|---|---|---|---|
| `UsageAggregator` 内存桶 | **仅 小时 / 天** | ❌ | `HOUR_BUCKETS=24*31`、`DAY_BUCKETS=31`（`usage_stats.rs:22,24`）；`StatsGranularity` 只有 `Hour`/`Day`（`usage_stats.rs:255-267`）。小时桶除以 60 只能得到"过去一小时平均"，看不出峰值 |
| `traces.db` | **逐请求** | ✅ 技术可行 | 有 `ts_epoch` 且已建 `idx_traces_ts ON traces(ts_epoch DESC)`（`trace_db.rs:689`），token 列齐全，SQL `GROUP BY ts_epoch/60` 可直接算。**但**：`trace_enabled` 可以从面板关掉，关掉后 RPM/TPM 静默变 0；保留期默认仅 7 天。把核心运维指标架在一个可被用户关闭的可选功能上，是脆弱设计 |
| `usage_log.*.jsonl` | 逐请求 | 理论可行 | 实时指标去重复解析磁盘文件是错的做法 |

## 五、方案（已按此实现，见 §0）：新建独立的分钟环形缓冲

不要复用 `UsageAggregator` 的现有桶结构，原因见下面的性能分析。建议新增一个**精简的分钟级环**：

- 容量 120 个分钟桶（覆盖 2 小时，足够看当前速率 + 近期峰值曲线）
- **每桶只放标量计数器**：`calls / input / output / cache_write / cache_read / errors`（+ 后续的 cost）
- **不放** `by_key` / `by_model` / `by_credential` / `by_key_model` / `by_key_credential` 这些嵌套 HashMap
- 环形数组按 `minute_ts % 120` 直接索引，O(1) 写入，无排序、无 `remove(0)`
- 与 trace 开关解耦：只要请求走过 `UsageRecordHook` 就计数，关 trace 不影响
- 双口径分别暴露：**入口 RPM**（每次外部请求 +1）与 **上游 RPM**（每跳 +1），前者给用户看流量、后者给你判上游压力

派生指标：
- RPM = 最近 1 个完整分钟桶的 calls（或最近 60 秒滑动）
- TPM = 同桶的 (input + output + cache_write + cache_read)。**口径必须写清**：TPM 是算总 token 还是只算计费 token，含不含缓存读 — 这两个数能差几十倍
- 峰值 RPM/TPM = 环内最大值

## 六、顺带查出的性能隐患（与 RPM/TPM 直接相关）

这几条决定了"为什么不能直接给现有桶加分钟粒度"：

1. **`upsert_bucket` 是 O(n) 三连**（`usage_stats.rs:608-623`）：线性 `find` 找桶 → 新桶时全量 `sort_by_key` → 超容量时 `remove(0)`（Vec 头删是 O(n) 内存搬移）。小时桶 744 个时勉强可接受；若照搬到 1440 个分钟桶，每个新桶都要排序 + 头删，直接变成写入热点。

2. **写锁跨两次 upsert**（`usage_stats.rs:427-430`）：`ingest` 拿一次 `write()` 锁，串行做 hour + day 两次 upsert。加第三个粒度就是三次，锁持有时间进一步拉长。所有请求的 record 都要过这把锁。

3. **`BucketEntry` 内存放大**（`usage_stats.rs:210-221`）：每个桶挂 5 个 HashMap，其中两个还是嵌套的（`by_key_model`、`by_key_credential`）。桶数 × Key 数 × 模型数 的乘积级内存。分钟桶如果照这个结构建，内存会炸。

4. **`client_keys.rs` 每请求全量写盘**（`client_keys.rs:456-477`）：`record_usage` 每次调用末尾都 `save_locked`，即把**所有** Key 序列化成 pretty JSON 全量覆写文件，而且在写锁内。高 QPS 下这是明确的吞吐天花板 — 每个请求一次全量 JSON 序列化 + 磁盘写。这条和 RPM 无关但会直接压低系统能承受的真实 RPM。

5. **`verify_and_touch` 每请求扫全表**（`client_keys.rs:434-453`）：为抗时序攻击刻意不 break，扫完所有 Key 做常量时间比较。设计意图正确，但 Key 数量上去后是 O(keys)/请求。

## 七、待运行时验证（不做结论）

以下需要真实流量才能确认，现在只是代码推断：
- 上游重试放大的实际倍数（决定入口 RPM 与上游 RPM 的差距有多大）
- 当前生产的 QPS 量级，据此判断第 4 条的全量写盘是否已经是现实瓶颈
- traces.db 在生产的实际行数与 `GROUP BY` 分钟聚合的真实耗时
