# kiro.rs 系统架构与 Kiro 对接分析

> 分析时间：2026-08-24。基线 master @ 362d543 (v0.7.6)；`cargo build` 通过（44s），`bun run build` 通过（906ms）。
> 结论分三档：**已验证**（读代码坐实，带行号）／**待运行时验证**（推断，不可当结论用）／**设计如此**（不是 bug，别去"修"）。

## 执行摘要

你反馈的三件事，根因如下：

| 症状 | 根因 | 性质 |
|---|---|---|
| 有时候 cache 没展示 | **两个独立根因**：① 上游 `tokenUsage` 缺失时 cache 数字完全由中转层模拟，与 Kiro 实际缓存无关；② 部分 payload 会静默把 cache 清零，并丢弃本地算好的模拟值 | ① 设计取舍<br>② **真 bug** |
| 有时候没有流量 | 纯 websearch 请求全程不写 traces.db（**已修**，§4.1）；叠加 `credential_id==0` 被踢出凭据分布（未动） | **真 bug** + 设计如此叠加放大 |
| 要 RPM / TPM | RPM 只是限流闸门不是指标（默认关闭、重启清零、口径是上游跳数）；TPM 完全不存在；聚合器最细只到小时 | 需新建采集层 |

最尖锐的一条：**`TokenUsage` 每个字段都是 `#[serde(default)]`，上游发来部分 payload 会静默变成全零，且该值优先级最高、直接覆盖本地算好的缓存值。** 见 §3.2。

## 1. 端到端请求链路

**装配顺序**（`src/main.rs:22-352`）：
config.json + credentials.json → `MultiTokenManager`（凭据池）→ `KiroProvider`（持 ide/cli 两端点注册表，`main.rs:161-166`）→ `CacheMeter`（进程内提示词缓存模拟，`main.rs:260-263`）→ Axum 路由。

**入口鉴权**：`/v1/*` 与 `/cc/v1/*` 两组路由共用 `auth_middleware`（`middleware.rs:110-137`），API Key 精确匹配，命中后把 `KeyContext{key_id, group, key_source}` 注入 request extensions。

**四条 handler**（均在 `handlers.rs`）：

| 入口 | 流式实现 | 关键差异 |
|---|---|---|
| `/v1/messages` (`:608`) | `handle_stream_request` (`:844`) | 真流式，边收边转发 |
| `/cc/v1/messages` (`:1538`) | `handle_stream_request_buffered` (`:1764`) | **假流式**，见下 |

`/cc/v1` 用 `BufferedStreamContext`（`stream.rs:2562-2670`）：把整条上游流吃完、事件全缓冲，流结束后才回填 `message_start.usage.input_tokens/cache_*`，再一次性发出所有 SSE（`stream.rs:2636-2647`）。动机是让 `message_start` 里的 `input_tokens` 是准确值（Anthropic 官方协议要求），而 Kiro 只有流跑完才给得出这个数。**代价是完全牺牲 TTFB**：客户端拿到第一个字节前要空等整轮生成完成，长输出场景下是数十秒到数分钟，期间只有 ping 保活，且**没有硬超时保护**。

**websearch 两条特殊路径**（四个入口都检测，`handlers.rs:657, 1576`）：

1. **纯 websearch**（`has_web_search_tool`，`websearch.rs:117-121`）——tools 只有一个原生 `web_search` 时，走 `handle_websearch_request`（`websearch.rs:571`）直接拼 MCP JSON-RPC 打 `/mcp` 端点，**不经过 `convert_request`、不进正常 chat 流程**。
2. **混合工具**（`has_web_search_among_tools`，`websearch.rs:128-132`）——`web_search` 与其他工具混用时落普通 chat 路径，上游可能回 `tool_use{name:"web_search"}`，进入 `run_web_search_loop`（`websearch_loop.rs:785`）：中转层内部执行 MCP 搜索、结果当 `tool_result` 喂回、重新 `convert_request` 再调一次上游，最多 `MAX_WEB_SEARCH_ROUNDS = 5` 轮（`websearch_loop.rs:40`）。每轮都是独立 provider 调用，各自走完整凭据故障转移。

## 2. 与 Kiro 上游的对接

### 2.1 认证与凭据

四种 auth_method（`credentials.rs:45-47, 65-70`），归一化在 `canonicalize_auth_method_value`（`:266-279`）。刷新统一入口 `refresh_token`（`token_manager.rs:118-157`）按类型分流：

| auth_method | 刷新端点 | 备注 |
|---|---|---|
| `social` | `prod.{region}.auth.desktop.kiro.dev/refreshToken` | `token_manager.rs:160-246` |
| `idc` / `builder-id` / `iam` | AWS SSO OIDC `oidc.{region}.amazonaws.com/token` | `:249-350` |
| `external_idp` | 凭据自带 `token_endpoint`（企业 IdP） | 必过域名白名单 `validate_external_idp_endpoint`（`credentials.rs:303-339`），仅允许 `.microsoftonline.com/.us/.cn`，防 SSRF + refreshToken 外泄 |
| `api_key` | 不刷新，`kiro_api_key` 直接当 Bearer | `token_manager.rs:2117-2130` |

**profileArn 三层优先级**：凭据显式配置的真 ARN → `resolve_profile_arn_for` 调 `ListAvailableProfiles` 解析出的真 Enterprise ARN（`token_manager.rs:3284-3315`，按凭据 id 进程内去重，仅在缺失或是占位符时触发一次，`provider.rs:206-250`）→ 按登录方式推断的**硬编码共享占位符**（`credentials.rs:13-16`）。

⚠️ `BUILDER_ID_PROFILE_ARN` / `SOCIAL_PROFILE_ARN` 是从真实 Kiro IDE 抓包得到的共享 ARN，所有走 fallback 的凭据共用同一个 profile。AWS 侧一旦收紧该 profile 速率或下线，会同时影响所有依赖占位符的凭据，代码里**无任何对该假设的运行时校验或告警**。

### 2.2 请求与响应格式

**请求体** `KiroRequest{conversationState, profileArn, additionalModelRequestFields}`（`requests/kiro.rs:32-51`）。核心是 `currentMessage.userInputMessage`（content/modelId/images/userInputMessageContext）+ `history`（User/Assistant 交替，`requests/conversation.rs:14-31, 244-249`）。

**响应是 AWS event-stream 二进制帧**（`parser/frame.rs`）：
`Total Length(4B) + Header Length(4B) + Prelude CRC(4B) + Headers + Payload + Message CRC(4B)`，CRC32 用 ISO-HDLC（`crc.rs:8`）。`EventStreamDecoder`（`decoder.rs`）是四态状态机（Ready/Parsing/Recovering/Stopped），容错恢复：Prelude 阶段错误逐字节跳边界，Data 阶段错误按 `total_length` 跳整帧（`:228-291`），连续错误达 `DEFAULT_MAX_ERRORS=5` 才停（`:41`）。

**ide vs cli 端点差异**（`endpoint/ide.rs`, `endpoint/cli.rs`）：

| | ide（默认） | cli |
|---|---|---|
| URL | `q.{region}.amazonaws.com/generateAssistantResponse` | 根路径 + `x-amz-target` 头 |
| Content-Type | `application/json` | `application/x-amz-json-1.0` |
| User-Agent | `aws-sdk-js/1.0.34 ... KiroIDE-{ver}-{machineId}` | `aws-sdk-rust/1.3.15 ... app/AmazonQ-For-CLI` |
| profileArn | 注入请求体根对象（`ide.rs:116-126`） | 不注入（协议无此位） |
| 特有头 | `x-amzn-kiro-agent-mode: vibe` | `x-amzn-codewhisperer-optout: false` |
| 其他 | — | **移除** `agentContinuationId` 与历史 `modelId`，`origin` 从 `AI_EDITOR` 换成 `KIRO_CLI`（`cli.rs:119-142`） |

**上游事件类型**（`kiro/model/events/`）：`assistantResponseEvent`（增量文本）、`toolUseEvent`（input 按 tool_use_id 分片，仅 `stop=true` 才整体解析）、`reasoningContentEvent`（原生 thinking）、`metadataEvent.tokenUsage`、`contextUsageEvent`、`meteringEvent`、error/exception 帧。

## 3. 上游到底给了什么计量数据（"cache 不展示"的根因）

### 3.0 三个计量事件的真实覆盖面

| 事件 | 给什么 | 可靠性 |
|---|---|---|
| `metadataEvent.tokenUsage` | `uncachedInputTokens` / `outputTokens` / `cacheReadInputTokens` / `cacheWriteInputTokens` 四字段，单次调用最终快照 | **`Option`，不保证下发**。`metadata.rs:75-77` 注释原文：「有些 metadataEvent 只携带 stopReason，因此 tokenUsage 必须保持可选」 |
| `contextUsageEvent` | **只给百分比**（0-100） | 反推 token 靠 `pct × window_size / 100`（`stream.rs:1575-1578`, `handlers.rs:1224-1228`），依赖硬编码窗口表 |
| `meteringEvent` | 只有 `{unit, unitPlural, usage}` | `metering.rs:8` 注释明确「上游**不下发** token / cache 字段（实测确认）」。credit 是 Kiro 自己的计费口径，与 token 无关，帮不上 cache |

同一条流内 `tokenUsage` 重复出现取最后一份、不累加（`stream.rs:1567-1569`，注释说明累加会重复计费）。

### 3.1 根因一：tokenUsage 缺失 → cache 数字完全是编的（设计取舍）

拿不到 `tokenUsage` 时，cache 相关数据**没有任何上游来源**。此时走 `CacheMeter`（`cache_metering.rs`）——纯本地实现：

- 把 prompt 按 message 边界切成递增前缀段链，每段 SHA-256 折叠成 u64
- token 用 `estimate_tokens` 中英文字符粗估（`stream.rs:2675-2693`）
- **命中与否只取决于进程内 `Mutex<HashMap>` 记不记得这个前缀哈希**，与 Kiro 后端是否真做了 prompt cache **零关联**
- 最后用 `CacheUsage::split_against_total`（`cache_metering.rs:85-105`）按比例分摊到同样是估算值的 total 上

**两套精度完全不同的数字塞进同一个 JSON 字段返回客户端，客户端无法区分自己看到的 cache 是真值还是猜的。**

衍生失真：
- **多实例部署下彻底失效**：纯进程内 HashMap，无 Redis 无跨进程层。请求被 LB 打到不同进程 → 前缀链永远 miss → `cache_read` 恒 0，**且无任何日志/指标提示"这次因跨进程而 miss"**，运营侧只看到"命中率莫名很低"。
- 主 apiKey（`key_id=0`）且请求无 session 时 `isolation_seed()` 返回 None（`cache_metering.rs:542-555`）→ 整个缓存模拟关闭、cache 恒 0。**这是故意的**（该 Key 被多用户共享，按 key 模拟会产生跨用户假命中），不是 bug。

### 3.2 根因二：部分 payload 静默清零并丢弃模拟值（**真 bug**）

这条是本次分析最尖锐的发现，完整链路逐环坐实：

1. `TokenUsage` **每个字段都是 `#[serde(default)]`**，结构体本身 `derive(Default)`（`metadata.rs:14-29`）
2. 上游若发 `{"outputTokens": 500}` 这种部分 payload → **反序列化成功**，四个 cache/input 字段全部填 0
3. 赋值处**无任何守卫**，直接 `Some(usage)`（`stream.rs:1569`、`handlers.rs:1219`）
4. `sanitized()` **只做 `max(0)` 钳负，不判全零、不把零当"缺失"**（`metadata.rs:33-41`）
5. `resolve_non_stream_usage`（`handlers.rs:410-418`）见 `Some` **立即 return，完全跳过** `cache_usage.split_against_total()`；`StreamContext::resolved_usage`（`stream.rs:1420-1432`）同构

**后果**：CacheMeter 明明做完了全部工作（切段、哈希、lookup、record），算出的缓存值被**静默丢弃**。更糟的是若 `uncached_input_tokens` 也是 0，连 `input_tokens` 一起归零——一个部分 payload 能把整个输入侧账目全部抹平。

全仓 grep `is_zero` / `is_empty` / `has_usage` / `is_all_zero`：**零命中**，确实没有任何守卫。

> **待运行时验证**：Kiro 实际会不会发部分 `tokenUsage`（只有 outputTokens、无 cache 字段）。**代码脆弱性已确认无疑**，但上游是否真触发这条路径，光读代码定不了。这决定根因二的实际权重。
> 验证方法：在 `stream.rs:1560` 那条 debug 日志基础上加一条 warn——当 `tokenUsage` 存在但四字段全零、或 cache 字段为零而本地 `cache_usage.cache_covered_est > 0` 时打点，跑一天生产流量看命中次数。

### 3.3 反推 input_tokens 依赖硬编码窗口表

`get_context_window_size`（`converter.rs:305-330`）是**手工维护的模型→窗口白名单**：GPT-5.6 系列 272K；一份列举的 Claude 家族（sonnet-4.6/4.8/5、opus-4.6/4.7/4.8/5、fable-5）1M；**其余一律回退 200K**。

`converter.rs:301-304` 注释自陈风险：「漏配某个 1M 模型不会影响发往上游的请求，但会让该模型的 usage 上报**缩小 5 倍**」。`converter.rs:2018-2021` 有 Opus 5 曾被漏配的回归测试。

问题在于：新模型上线要人肉改代码，**没有任何机制拿上游 `ListAvailableModels` 的 `TokenLimits` 去交叉验证这张表**——而这个接口本来就在调（`available_models.rs:42-50` 有 `maxInputTokens` 字段）。这是可以自动化却没自动化的地方。

### 3.4 cache 归零路径穷举

| # | 路径 | 触发条件 | 性质 |
|---|---|---|---|
| 1 | `tokenUsage` 缺失 → CacheMeter 模拟 | 上游未下发该事件 | 设计取舍（数字与上游实际缓存无关） |
| 2 | 部分 payload 静默清零 + 丢弃模拟值 | 上游发不含 cache 字段的 `tokenUsage` | **真 bug**（§3.2） |
| 3 | `isolation_seed()` 返回 None → 整个模拟关闭 | 主 apiKey(`key_id=0`) 且请求无 session（`cache_metering.rs:542-555`） | **设计如此**（防跨用户假命中） |
| 4 | `extract_segments` 无可切段 → covered=0 | 单条 message、无 system/tools（最后一条不切段） | 设计如此（确实无可复用前缀） |
| 5 | **混合工具 agentic 循环无 CacheMeter 兜底** | `web_search` 与其他工具混用走 `run_web_search_loop` | **缺陷**：`websearch_loop.rs` 全文零 `CacheUsage`/`cache_metering` 引用（已 grep 确认），完全依赖上游 `TokenUsage`；上游不给就是硬 0，连模拟兜底都没有 |
| 6 | 纯 websearch 早退硬编码零 | `handlers.rs` 早退处 `hook.record(0, input_tokens, 0, 0, 0, 0.0, ...)` | trace 缺失部分**已修**（§4.1）；cache 恒 0 是该路径固有性质（不打上游、无上游用量） |
| 7 | `cache_meter` 为 None → `unwrap_or_default()` 全零 | `handlers.rs:786-789 / 1709-1712` | 理论路径（`main.rs:272` 恒传 `Some`，仅测试/其他嵌入方式会命中） |
| 8 | 多进程部署前缀链永不命中 | 水平扩展、请求被 LB 分散 | **缺陷**：cache_read 恒 0 且无任何告警（§3.1） |

判读要点：**只有 #2、#5、#6、#8 值得修**。#1 是口径问题（要治得让来源可区分，不是"修零"），#3/#4 是正确行为，#7 生产不会发生。排查现场先定位落在哪条路径上，别把 #3/#4 当 bug 追。

## 4. "有时候没有流量"的根因

### 4.1 纯 websearch 请求全程不写 traces.db（**真 bug — 已修**）

**原状**：`handlers.rs` 那条早退路径估算 input_tokens → 调 `handle_websearch_request` → 记 `hook.record(0, input_tokens, 0, 0, 0, 0.0, status)` → `return resp`，**期间从未构造 `RequestTracer`**。后果是这类请求**进了聚合器**（概览卡片、趋势图有它），但**请求日志页完全看不到**——两个页面对同一批流量给出矛盾的答案。

值得注意的是 v0.7.6 那个 `fix(trace): record mixed web-search requests`（#66）只覆盖了**混合工具**路径：它把 `call_mcp_api` 的 `sink: Option<&dyn TraceSink>` 参数和 `call_mcp_with_trace` 都铺好了，但**纯 websearch 这条一直传 `None`**。所以修复不是新建管子，是把已有的管子接上。

**修法**（本次）：`handle_websearch_request` 增 `sink` 参数透传给 `call_mcp_api`；`/v1` 与 `/cc/v1` **两个入口各自**自建 tracer 并在早退前 `finalize_websearch_trace`。附带收益：`call_mcp_with_trace` 会记下带真实 credential_id 的 attempt，trace 里的凭据归属不再恒为 0。

**刻意未动**：同处 `hook.record(0, ...)` 一个字没改。聚合器侧 `credential_id==0` 的记录不进凭据分布（§4.2），改它会连带变更聚合口径，属另一件事。因此 trace 有真实凭据、聚合器仍记 0，这是有意的边界。

用量口径也刻意与 `hook.record` 对齐（只记 input，output/cache/credits 记 0）：该路径打 MCP 端点、不产生上游 token 用量，响应里的 output_tokens 是本地摘要的字符估算值。两个 sink 记同一份数，避免再造一处矛盾。

### 4.2 `credential_id == 0` 被踢出凭据分布（设计如此，但叠加放大）

`usage_stats.rs:640-641`：`add_record_to_bucket` 在 `rec.credential_id == 0` 时直接 `return`，不写 `by_credential` / `by_key_credential`。

全仓共 **12 处** `hook.record(0, ...)`（websearch 两处 + 各早退错误路径），这些请求在「按凭据分布」里不存在。单看是合理的（没凭据无法归属），但与 §4.1 叠加后失真被放大。

### 4.3 错误请求的记账方式

早退路径记的是全零（`hook.record(0, 0, 0, 0, 0, 0.0, "error")`）。它们**会计入 calls 和 errors**（`BucketStats::add`，`usage_stats.rs:186-196`），所以调用数不会丢，但 token 维度全零会拉低平均值。

## 5. 故障转移与凭据调度

**重试预算**（`provider.rs:702-1151` `call_api_with_retry`）：
```
max_retries = (该分组凭据数 × MAX_RETRIES_PER_CREDENTIAL(3)).min(MAX_TOTAL_RETRIES(4))
```
（`provider.rs:711`，注意是 `total_count_in_group` **按分组**算，不是全局）

`MAX_TOTAL_RETRIES = 4` 是硬上限（`provider.rs:32`）。**这是刻意权衡不是疏漏**——注释写明：429 多为账号级速率配额，过多重试会「在账号间连环撞墙、放大限流」，故上限取小值 + 429 专用长退避。

但代价真实存在：账号池扩到几十个时，故障转移事实上只试 4 次就放弃。设计意图与"大池子充分轮转"是冲突的，这个权衡点值得你按实际池子规模重新校准。

**错误分类处理**：

| 类别 | 动作 | 触发条件 |
|---|---|---|
| 换凭据 | `report_quota_exhausted_for_request` | 402 额度用尽 |
| 换凭据 | `report_failure_for_request`（含 force-refresh） | 401 / 403 认证失败 |
| 换凭据 + **永久禁用不参与自愈** | `report_suspended_for_request` | 403 + 明确封禁文案 |
| 换凭据 + 临时冷却 | `report_account_throttled_for_request` | 429 + `suspicious activity` |
| 纯重试不换号 | 指数退避 | 网络错误 / 408 / 429(普通) / 5xx |
| **直接终止不重试** | — | 400；`is_client_validation_error`（`TOOL_USE_RESULT_MISMATCH` / `TOOL_SCHEMA_INVALID`，`endpoint/mod.rs:196-232`）；524 |

最后一类的设计很关键：客户端消息数组违反协议是**客户端的错**，若按上游瞬态错误重试会触发冷却，把一个客户端错误放大成 503 风暴。集中识别在 endpoint 层，`map_provider_error` 映射成 400 兜底。

**冷却**：`throttled_until: Option<Instant>`（`token_manager.rs:900`，**不持久化，重启清空**），秒数取 `max(上游 Retry-After, account_throttle_cooldown_secs(默认1800))`（`provider.rs:1244-1257`）。

**自愈**（`try_self_heal`，`token_manager.rs:2697-2777`）：某作用域内全部凭据不可用时，**只复活因 `TooManyFailures` 被禁用的**（`Suspended`/`QuotaExceeded`/`InvalidRefreshToken` 一律不参与）。三重约束：同凭据同模型才清连续轮数、`self_heal_min_interval_secs`(默认300s) 冷却、`self_heal_max_consecutive_rounds`(默认5) 上限。状态跨重启持久化。这套约束是为了打断 issue #51 的「全禁→自愈→403→再禁」死循环。

## 6. RPM / TPM：现状与方案

**结论先行：RPM 存在，但它是一个「限流闸门」，不是一个「可观测指标」。TPM 完全不存在。** 想在面板上看到这两个数，现在没有任何现成数据可直接读，必须新建采集层。

完整分析见 `RPM-TPM-ANALYSIS.md`，此处只列关键事实：

| 事实 | 位置 |
|---|---|
| `rpm_window: VecDeque<Instant>` 挂在每个凭据上，只存最近 60s 时间戳 | `token_manager.rs:904` |
| **默认关闭** | `config.rs:326-328` |
| 关闭时窗口被主动 `clear()`——**连原始素材都不留** | `token_manager.rs:1823-1827` |
| 内存态，重启归零 | `:903` 注释明写 |
| Admin 只暴露**配置**（enabled/limit），不暴露**当前速率** | `service.rs:2075-2079` |
| TPM 全仓 grep `TPM`/`tokens_per_minute`/`tpm` | **零命中** |

**口径陷阱（做之前必须定清）**：现有 `rpm_window` 记的是「被选中的凭据请求数」。一次外部请求若重试 3 跳打了 3 个凭据，会在 3 个凭据窗口各记一次。**作为限流是对的**（保护上游），**作为 RPM 指标是错的**（对外只发生了 1 次请求）。

→ **上游 RPM ≠ 入口 RPM，必须分开暴露**：入口 RPM 给你看真实流量，上游 RPM 给你判上游压力与重试放大倍数。

**为什么不能直接给现有聚合器加分钟粒度**：

`usage_stats.rs` 的 `upsert_bucket`（`:608-623`）是 O(n) 三连——线性 `find` 找桶 → 新桶时全量 `sort_by_key` → 超容量 `remove(0)`（Vec 头删是 O(n) 内存搬移）。744 个小时桶勉强能扛，照搬到 1440 个分钟桶就是写入热点。而且 `ingest` 一把写锁跨两次 upsert（`:427-430`），加第三粒度就是三次。更要命的是 `BucketEntry` 挂 5 个 HashMap、其中 2 个嵌套（`:210-221`），分钟桶照这结构建内存会爆。

**建议方案**：新建独立的**精简分钟环**，不复用现有桶结构。
- 120 个分钟桶（覆盖 2 小时，够看当前速率 + 近期峰值）
- **每桶只放标量计数器**：calls / input / output / cache_write / cache_read / errors（+ cost）
- **不放** by_key / by_model / by_credential 等嵌套 map
- 环形数组 `minute_ts % 120` 直接索引，O(1) 写入，无排序无头删
- **与 trace 开关解耦**：只要走过 `UsageRecordHook` 就计数（traces.db 可被用户从面板关掉，把核心运维指标架在可选功能上是脆弱设计）
- 双口径分别暴露（入口 / 上游）
- TPM 口径要写清：算总 token 还是只算计费 token、含不含 cache_read——这两个数能差几十倍

## 7. 缺陷清单（按建议优先级）

**P0 — 数据正确性，会让你看错数**

1. **部分 `tokenUsage` payload 静默清零 + 丢弃模拟值**（§3.2）。`metadata.rs:14-29` / `stream.rs:1569` / `handlers.rs:1219` / `handlers.rs:410-418`。修法：赋值处或 `resolve_*` 处加守卫——`tokenUsage` 全零时视作未下发；cache 字段为零但本地 `cache_covered_est > 0` 时至少打 warn。**动手前先按 §3.2 末尾的方法确认上游真会触发。**
2. ~~**纯 websearch 不写 traces.db**（§4.1）~~ → **已修**：`sink` 透传 + 两入口各自建 tracer 并 finalize，4 条单测覆盖（含红绿反向验证）。**注意**：单测只覆盖 finalize 行为，"sink 真的透传到 MCP 调用"这层因仓内无 mock server 依赖未做自动化覆盖，靠类型系统（调用方必须显式传参）与 diff 可读性保证。
3. **cache 两套口径不可区分**（§3.1）。修法：响应/trace 里加来源标记（upstream / simulated），前端据此区别展示；根治要比"继续把两种精度塞一个字段"更彻底。

**P1 — 可观测性缺口**

4. RPM/TPM 无采集层（§6）
5. 多实例下 CacheMeter 静默失效、无任何提示（§3.1）
6. ~~`traces` 表 `key_id` / `model` 裸筛无索引~~ → **已修，但修法与本文初版建议相反，见 §7.1**

### 7.1 索引那条：本文初版建议是错的（实测推翻）

初版写"给 `key_id`/`model` 加索引"。**照做会让查询慢上千倍**，实测数据如下。

原因在查询计划里：请求日志的 SQL 恒为「筛某列 + `ORDER BY ts_epoch DESC` + `LIMIT`」。无索引时 planner 走 `idx_traces_ts` 顺序扫，**天然满足排序**，取够 LIMIT 就停。一旦有了单列 `traces(key_id)`，planner 改走它 —— 得把该 key 的全部匹配行捞出来再排序，低基数列上直接爆掉。

实测（200 万行、40 个 Key、SQLite 3.x、Apple Silicon）：

| 查询 | 现状（三索引） | 加**单列**索引 | 加**复合**索引 |
|---|---|---|---|
| 分页 `COUNT(*)` 筛 key | 88.5ms（全表扫） | 4.0ms | **0.75ms**（覆盖索引） |
| 第 1 页 筛 key | 1.2ms | **104ms ← 慢 84x** | 0.059ms |
| 第 1 页 筛 model | 0.15ms | **172ms ← 慢 1100x** | 0.054ms |
| 深分页 OFFSET 10000 | 72.6ms | — | 0.27ms |

（单列索引在高基数 key 上确实能快，但 model 只有几个取值，恒定劣化。复合索引在两种选择性下**全部只快不慢**。）

**正确修法（已落地）**：`(key_id, ts_epoch DESC)` 与 `(model, ts_epoch DESC)` 复合索引 —— 过滤与排序由同一索引服务，`COUNT` 还能走覆盖索引。

最值得修的其实是 `COUNT(*)`：`query_inner` 每次翻页都先跑一次无 LIMIT 的 `COUNT`，而它与分页查询**同握一把 `self.conn.lock()`**，那 88ms 全表扫期间 **trace 写入全阻塞**。

代价（诚实列出）：老库首次启动现建索引，200 万行约 1.65s；磁盘 +25%。表大小有上界（trace 默认开、保留 7 天、每日清理），所以这个一次性成本是有界的。

**教训**：这条是典型的"读代码看出缺索引→建议加索引"，方向对、具体做法错。索引这类结论必须实测查询计划，不能靠推理。

**P2 — 性能与吞吐天花板**

7. **`client_keys.rs:456-477` `record_usage` 每请求全量序列化 + 覆写整个 JSON 文件，且在写锁内** ← 明确的吞吐天花板，直接压低系统能承受的真实 RPM。修法：改为脏标记 + 周期 flush（`CacheMeter::spawn_background` 已有现成范式）。
8. `upsert_bucket` O(n) 三连 + `ingest` 写锁跨两次 upsert（§6）
9. `verify_and_touch` 每请求扫全表（`client_keys.rs:434-453`）——抗时序攻击的**设计意图正确**，但 O(keys)/请求，Key 多了要重新设计（如按前缀分桶后再常量时间比较）

**P3 — 架构与维护性**

10. `get_context_window_size` 硬编码窗口表，漏配即 5 倍误差且无告警（§3.3）。修法：拿上游 `ListAvailableModels` 的 `TokenLimits` 交叉校验，不一致时 warn。
11. **`/v1` 与 `/cc/v1` 是两份近乎逐行复制的 handler**，维护漂移**已经发生**：`count_image_budget` 只在 `post_messages`（`handlers.rs:614`）调用，`post_messages_cc` 完全没有这段——两个入口行为已不一致（已 grep 证实）。
12. `/cc/v1` 假流式无硬超时保护（§1）
13. `MAX_TOTAL_RETRIES=4` 与大账号池冲突（§5，是权衡不是 bug，但需按实际池子规模校准）
14. 共享 profileArn 占位符无运行时校验（§2.1）

## 8. 必须运行时验证的事项（当前不可下结论）

1. **`tokenUsage` 缺失 / 部分下发的真实频率** ← 决定 P0-1 和 §3.1 的实际权重。方法见 §3.2 末尾。
2. **上游重试放大的实际倍数** ← 决定入口 RPM 与上游 RPM 的差距。
3. **生产 QPS 量级** ← 决定 P2-7 的全量写盘是否已是现实瓶颈。
4. **traces.db 实际行数与分钟聚合耗时** ← 决定 RPM/TPM 能否退而用 SQL 实现。
5. **是否多实例部署** ← 决定 §3.1 的多进程失效是否已在发生。

## 附：环境与基线

- Rust 1.98.0（本次新装，`~/.cargo/bin`），`cargo build` 通过 44s
- bun 1.3.14，`admin-ui` 构建通过 906ms
- 关键文件索引见各章节行号引用；RPM/TPM 专项见 `RPM-TPM-ANALYSIS.md`




