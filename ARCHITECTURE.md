# kiro.rs 架构与 Kiro 对接

> 描述系统**当前**的样子，基线 `6291136`。
>
> 与 `ANALYSIS.md` 的分工：本文讲系统怎么运作；那份讲症状分诊与修复历史（含三条我判错又推翻的结论）。
> 引用优先用符号名（函数 / 字段 / 分支）而非行号——行号会随改动漂移，指错比不指更坏。

## 1. 装配与请求链路

**启动顺序**（`main.rs`）：

```
config.json + credentials.json
  → MultiTokenManager（凭据池：刷新、故障转移、冷却、自愈）
  → KiroProvider（持 ide / cli 两端点注册表）
  → CacheMeter（进程内提示词缓存模拟）
  → RateRing（分钟级 RPM/TPM 采集）
  → Axum 路由（/v1、/cc/v1、/api/admin）
```

启动期**没有**自检。两项本该有的检测目前是缺口，见 §8.1（多实例占用）与 §4.4（窗口表漂移）。

**入口鉴权**：`/v1/*` 与 `/cc/v1/*` 共用 `auth_middleware`，API Key 按完整值精确匹配（不校验前缀），命中后把 `KeyContext{key_id, group, key_source}` 注入 request extensions。

**四条 handler**（均在 `anthropic/handlers.rs`）：

| 入口 | 流式实现 | 关键差异 |
|---|---|---|
| `/v1/messages` | `handle_stream_request` | 真流式，边收边转发 |
| `/cc/v1/messages` | `handle_stream_request_buffered` | **假流式**，见下 |

⚠️ 这两组是**近乎逐行复制的两份 handler**，维护漂移已经发生：`count_image_budget` 只在 `post_messages` 调用，`post_messages_cc` 没有。改一处必须查另一处。

### 1.1 `/cc/v1` 为什么是假流式

`BufferedStreamContext` 把整条上游流吃完、事件全缓冲，流结束后才回填 `message_start.usage.input_tokens/cache_*`，再一次性发出所有 SSE。

**动机**：Anthropic 协议要求 `message_start` 里的 `input_tokens` 是准确值，而 Kiro 只有流跑完才给得出这个数。

**代价**：完全牺牲 TTFB。客户端拿到第一个字节前要空等整轮生成完成，长输出是数十秒到数分钟，期间只有 ping 保活（`PING_INTERVAL_SECS = 25`）。上游连接用**读空闲**超时（`build_streaming_client(.., UPSTREAM_IDLE_TIMEOUT_SECS=300, ..)`，reqwest 的 `read_timeout()`，与 nginx `proxy_read_timeout` 同语义）。
> ⚠️ 曾经是 `timeout(720)` 整请求上限——它一路作用到 body 读取，会把正在正常产出的长流掐断：实测三条已推送 690-843 KB、1.2 万 output token 的 opus-5 请求在 720.04s 被切成 `interrupted`，而终止性的 `meteringEvent` 在流末尾，于是这些请求全部记 0 credit（2026-08-25 修）。

断流时的处理见 §5.2——那里有个曾经会对客户谎报的坑。

### 1.2 websearch 两条互斥路径

| 路径 | 触发条件 | 行为 |
|---|---|---|
| 纯 websearch | `tools` 恰好一个且是原生 `web_search` | 直接拼 MCP JSON-RPC 打 `/mcp`，**不经 `convert_request`、不进正常 chat 流程** |
| 混合工具 | `web_search` 与其他工具共存 | 落普通 chat 路径，上游可能回 `tool_use{name:"web_search"}`，进 `run_web_search_loop` |

混合工具那条：中转层自己执行 MCP 搜索、结果当 `tool_result` 喂回、重新 `convert_request` 再调上游，最多 `MAX_WEB_SEARCH_ROUNDS = 5` 轮。**每轮都是独立 provider 调用、各自走完整凭据故障转移** —— 所以一次外部请求可能产生远多于一次的上游调用。这正是 §6 要把入口 RPM 与上游 RPM 分开的原因。

## 2. 与 Kiro 的认证对接

四种 `auth_method`，归一化在 `canonicalize_auth_method_value`，刷新统一入口 `refresh_token` 按类型分流：

| auth_method | 刷新端点 | 备注 |
|---|---|---|
| `social` | `prod.{region}.auth.desktop.kiro.dev/refreshToken` | |
| `idc` / `builder-id` / `iam` | AWS SSO OIDC `oidc.{region}.amazonaws.com/token` | |
| `external_idp` | 凭据自带 `token_endpoint`（企业 IdP） | 必过域名白名单 `validate_external_idp_endpoint`，仅允许 `.microsoftonline.com/.us/.cn` —— 防 SSRF 与 refreshToken 外泄 |
| `api_key` | 不刷新 | `kiro_api_key` 直接当 Bearer |

**关键副作用：refresh token 会轮换。** 刷新成功后 `new_credentials.refresh_token = Some(new_refresh_token)`，新值写回 `credentials.json`。这一条是 §7 多实例危害的根源 —— 不是"可能不一致"那种软风险，是会让账号掉线。

### 2.1 profileArn 三层优先级

1. 凭据显式配置的真 ARN
2. `resolve_profile_arn_for` 调 `ListAvailableProfiles` 解出的真 Enterprise ARN（按凭据 id 进程内去重，仅在缺失或是占位符时触发一次）
3. 按登录方式推断的**硬编码共享占位符**（`BUILDER_ID_PROFILE_ARN` / `SOCIAL_PROFILE_ARN`）

⚠️ 那两个占位符是从真实 Kiro IDE 抓包得到的**共享** ARN，所有走 fallback 的凭据共用同一个 profile。AWS 侧一旦收紧该 profile 速率或下线，会同时影响所有依赖占位符的凭据，而代码里**没有任何对该假设的运行时校验或告警**。

## 3. 线协议

**请求体** `KiroRequest{conversationState, profileArn, additionalModelRequestFields}`，核心是 `currentMessage.userInputMessage`（content / modelId / images / userInputMessageContext）+ `history`（User/Assistant 交替）。

**响应是 AWS event-stream 二进制帧**：

```
Total Length(4B) + Header Length(4B) + Prelude CRC(4B) + Headers + Payload + Message CRC(4B)
```

CRC32 用 ISO-HDLC。`EventStreamDecoder` 是四态状态机（Ready / Parsing / Recovering / Stopped），容错恢复：Prelude 阶段错误逐字节跳边界，Data 阶段错误按 `total_length` 跳整帧，连续错误达 `DEFAULT_MAX_ERRORS = 5` 才停。

### 3.1 ide 与 cli 端点差异

| | ide（默认） | cli |
|---|---|---|
| URL | `q.{region}.amazonaws.com/generateAssistantResponse` | 根路径 + `x-amz-target` 头 |
| Content-Type | `application/json` | `application/x-amz-json-1.0` |
| User-Agent | `aws-sdk-js/... KiroIDE-{ver}-{machineId}` | `aws-sdk-rust/... app/AmazonQ-For-CLI` |
| profileArn | 注入请求体根对象 | 不注入（协议无此位） |
| 特有头 | `x-amzn-kiro-agent-mode: vibe` | `x-amzn-codewhisperer-optout: false` |
| 其他 | — | **移除** `agentContinuationId` 与历史 `modelId`，`origin` 从 `AI_EDITOR` 换成 `KIRO_CLI` |

**上游事件类型**：`assistantResponseEvent`（增量文本）、`toolUseEvent`（input 按 tool_use_id 分片，仅 `stop=true` 才整体解析）、`reasoningContentEvent`（原生 thinking）、`metadataEvent.tokenUsage`、`contextUsageEvent`、`meteringEvent`、error/exception 帧。

## 4. 计量：上游到底给了什么

**这一节是「cache 有时不展示」的正题。**

### 4.1 三个计量事件的真实覆盖面

| 事件 | 给什么 | 可靠性 |
|---|---|---|
| `metadataEvent.tokenUsage` | `uncachedInputTokens` / `outputTokens` / `cacheReadInputTokens` / `cacheWriteInputTokens`，单次调用最终快照 | **`Option`，不保证下发**；即使下发，**键也不保证齐** |
| `contextUsageEvent` | **只给百分比**（0-100） | 反推 token 靠 `pct × window_size / 100`，依赖 §4.4 那张手工表 |
| `meteringEvent` | 只有 `{unit, unitPlural, usage}` | 上游**不下发** token / cache 字段（实测确认）。credit 是 Kiro 自己的计费口径，**在流末尾才到**——流被打断就永远收不到，该请求记 0 credit |

### 4.1.1 Kiro 的 credit 到底按什么收（2026-08-25 实测）

对 7 天真实流量做过三种互相独立的估计（控制 cache_creation 后分桶、用零缓存样本拟合再外推高缓存样本、按 Key×日期做固定效应回归），三者对同一模型收敛到 2% 以内。折成美金（$0.02/credit）的**边际**单价：

| 模型 | 输入 | 输出 | 缓存写 | 缓存读 | 账单主要构成 |
|---|---|---|---|---|---|
| claude-opus-5 | ~0（不显著） | $7.5/M | $3.2/M | **$0.36/M** | 缓存读 80-100%，输出仅 ~10% |
| claude-sonnet-5 | $0.08/M | $9.3/M | $1.9/M | **$0.05/M** | 输入+缓存读 ~86%，输出 ~5% |
| gpt-5.6-terra | $0.21/M | $4.9/M | — | $0.069/M | 输出 85%（唯一"按输出计费"的） |
| gpt-5.6-luna | $0.22/M | ~0（不显著） | $0.27/M | $0.24/M | 几乎全按输入 |

**两条方法论教训（都栽过）：**

1. **别用「总花费 ÷ 总输出 token」当"输出单价"。** 那个算法在构造上就把 100% 成本摊给输出，只有当输出真的解释成本时才有意义。判据现成：把它与回归得到的**边际**单价比——terra 两者接近（$5.91 vs $5.00，说明确实按输出收），Claude 家族差好几倍（说明不是）。
2. **做"控制变量"对照时要检查没被控制的那些变量。** 「固定输出量、比较不同缓存读占比」看起来干净，但低缓存组的 `cache_creation` 均值高出 80 倍（11,161 vs 138），足以把结论带反。

⚠️ **2026-08-24 有口径断点**：那天镜像重建 8 次，之后部分凭据的「缓存读/请求」跳约 5 倍而 credit 未同步跳变。跨 08-24 混算缓存相关的定价会得到错误结论，未查清前请分段分析。

同一流内 `tokenUsage` 重复出现取最后一份、**不累加**（累加会重复计费）。

### 4.2 三档来源，不是两档

`cache_read_input_tokens` / `cache_creation_input_tokens` 这两个数**有两套精度完全不同的口径塞在同一个 JSON 字段里**。`classify_usage_source` 把它分成三档：

| `UsageSource` | 含义 | 可信 |
|---|---|---|
| `upstream` | 上游四键齐全，Kiro 后端真值 | ✅ |
| `upstream_partial` | 上游给了但键不齐 —— **混了真值与 serde 填的假零** | ❌ |
| `simulated` | 未收到 `tokenUsage`，全靠 CacheMeter | ❌ |

`upstream_partial` 判不可信是刻意的：它最像权威数据，却恰好在关键位置掺了假零，拿去做精确判断错得最隐蔽。

**能分出这三档，靠的是 `present` 位掩码。** `TokenUsage` 手写 `Deserialize`（四字段先读成 `Option`）记下上游实际下发了哪些键 —— `#[serde(default)]` 会把「键缺失」与「键存在且为 0」抹成同一个值，掩码是唯一能分开的办法。传播规则两条：`sanitized()` 原样带过掩码（钳负不改变"给过哪些键"）；`saturating_add` 取**交集**（多跳合并时，一份完整 payload 不该掩盖另一份的残缺）。

判据必须与 `resolve_non_stream_usage` 的分支一致（`Some` 走上游、`None` 走模拟），否则标的来源与实际用的数字不符。

### 4.3 CacheMeter 的边界

拿不到 `tokenUsage` 时，cache 数字**没有任何上游来源**，全由 `CacheMeter` 生成：

- 把 prompt 按 `cache_control` 断点切成递增前缀段链，每段 SHA-256 折叠成 u64
- token 用 `estimate_tokens` 中英文字符粗估
- **命中与否只取决于进程内 `Mutex<HashMap>` 记不记得这个前缀哈希，与 Kiro 后端是否真做了 prompt cache 零关联**
- 最后 `split_against_total` 按比例分摊到同样是估算的 total 上

两条**设计如此**的归零路径（不是 bug，别去修）：

1. 主 apiKey（`key_id = 0`）且请求无 session 时 `isolation_seed()` 返回 `None` → 整个模拟关闭、cache 恒 0。**故意的** —— 该 Key 被多用户共享，按 key 模拟会产生跨用户假命中。
2. `extract_segments` 切不出可复用段（单条 message、无 system/tools）→ covered = 0。确实无可复用前缀。

### 4.4 上下文窗口表与漂移自检

`get_context_window_size` 是**手工维护的模型→窗口白名单**：GPT-5.6 系列 272K；一份列举的 Claude 家族（sonnet-4.6/4.8/5、opus-4.6/4.7/4.8/5、fable-5）1M；**其余一律回退 200K**。

危险在于这个数直接乘进 `contextUsageEvent` 的百分比反推：**漏配一个 1M 模型，该模型 usage 缩小 5 倍**，而请求本身不会失败 —— 静默偏差。

⚠️ **当前无任何交叉校验 —— 这是个缺口。** 新模型上线要人肉改代码，漏配没有任何提示。

而校验所需的数据**本来就在手上**：`ListAvailableModels` 已经在调，`ModelInfo.token_limits.max_input_tokens` 已经在反序列化。缺的只是在模型清单缓存填充时比一次，按方向分级告警（本地 < 上游 → 缩小上报，该 `warn` 并给出缩小倍数；本地 > 上游 → 偏大但不丢流量，`info` 即可）。

做的时候有一条要守住：**表只读，不自动采纳上游数字** —— 让计费口径随上游申报漂移是架构决定，不该是一个诊断的副作用。

## 5. 可观测性：两条独立记账通道

**这一节是「有时候没有流量」的正题。**

每个请求结束时走 `UsageRecordHook::record`，它同时写四处：

```
UsageRecordHook::record
  ├─ usage_log.YYYY-MM-DD.jsonl   持久化历史
  ├─ UsageAggregator::ingest       内存桶（概览卡片 / 趋势图），最细到小时
  ├─ ClientKeyManager::record_usage 按 Key 累计（仅 status == "success" 且 key_id != 0）
  └─ RateRing::record_ingress      分钟环，入口口径（§6）
```

而 `RequestTracer` 是**另一条通道**，逐跳记 attempt 与最终状态到 `traces.db`（请求日志页）。

**两条通道口径不同 —— 这就是"概览有流量、请求日志没有"的来源。** 具体分诊见 `ANALYSIS.md` §4。要记住的结构性事实：

- `credential_id == 0` 的记录被 `add_record_to_bucket` 踢出凭据分布，所以各类早退错误在「按凭据分布」里不存在
- `traces.db` 可以被用户从面板关掉 —— 所以**核心运维指标不能架在它上面**（§6 的采集层因此与它解耦）

### 5.1 traces 查询形态决定索引形状

请求日志的 SQL 恒为「筛某列 + `ORDER BY ts_epoch DESC` + `LIMIT`」，且分页要先跑一次**无 LIMIT 的 `COUNT(*)`**，与分页查询同握一把 conn 锁。

因此索引是 `(key_id, ts_epoch DESC)` / `(model, ts_epoch DESC)` **复合**索引：过滤与排序由同一索引服务，`COUNT` 走覆盖索引。

⚠️ **别加单列索引。** 加 `traces(key_id)` 会让 planner 放弃 `idx_traces_ts` 的天然有序性、改成捞全部匹配行再排序 —— 实测慢 84~1100 倍。测试守的是 **planner 的选择**而非"索引存在"，就是为了拦这个。

### 5.2 断流不得谎报正常收尾

`/cc/v1` 的响应头（200 + `text/event-stream`）在收到第一个 chunk 前就已发出。上游被掐断时错误分支会 flush 缓冲事件并补齐 `message_delta` / `message_stop`，而 `stop_reason` 取自 `get_stop_reason()`，其兜底是 `end_turn`。

**若不干预，客户端会收到一个完整、合法、声称模型自然说完的 SSE 序列，从外部无法察觉只拿到半截答案** —— 而内部 trace 记的是 `interrupted`。

所以断流现场调用 `mark_upstream_interrupted()`，让 `stop_reason` 报 `max_tokens`（该值在本仓已表示"输出被截断"，`ContentLengthExceededException` 走的就是它）。已收到上游 `stopReason` 时不覆盖。

**为什么不能改兜底**：纯 web_search 请求同样没有上游 `stopReason`，却必须报 `end_turn`。断流与 web-search-only 在数据上完全同形，只有断流现场知道自己是断的。

### 5.3 用量计数的落盘节奏

`ClientKeyManager` 有 10 个 `save_locked` 调用点，只有 `record_usage` 那个走延迟：置脏标记 + 30s 后台 flush。其余 9 个（create / delete / set_disabled / update_meta / rename_group / clear_group / rotate / reset_stats / sync_system_key）**仍立即写盘** —— 低频结构性变更，丢失代价远高于丢一批计数。

`flush_if_dirty` **先清标记再写盘**：写盘期间新到的 `record_usage` 会重新置脏、下周期继续落。反过来会把这期间的变更连同标记一起清掉。

这些计数器**不 gate 任何东西**（无 quota/limit/remaining 字段，`disabled` 只由管理动作显式设置），所以崩溃丢最多 30s 计数损失的是面板精度，不影响资金安全。

### 5.4 成本换算与分维度 TPM（2026-08-25 起）

**计价**：`src/common/pricing.rs`。启动时从 `config.json` 的 `pricing` 段解析成只读
`PricingTable`（挂在 `AdminState`），两套口径并行——实付（credits × `creditUsdRate`，
默认 0.02）与官方牌价（内置 Claude 家族 $/Mtok，cache 写 1.25×、读 0.1× 输入价；
`pricing.models` 可覆盖/补充任意模型）。查不到价返回 `None` 而非 0，前端显示"—"；
折扣 = 实付 ÷ 官方。模型名先归一化（小写、点转横线）再查表，因为 trace 里点号名与
官方横线名并存。

**新增只读端点**（全部旁路查询，不碰请求热路径）：

| 端点 | 数据源 | 说明 |
|---|---|---|
| `GET /api/admin/traces/summary` | traces.db | 与 `/traces` 完全同一套 WHERE（`build_trace_query` 共享），按模型汇总用量/成本/折扣 + 合计 |
| `GET /api/admin/stats/tpm?dim=key\|credential` | traces.db | 按 (实体, 分钟) 内层聚合再取峰值/均值；峰值 TPM 即该 Key/凭据的实测承载。无时间窗时服务端兜底最近 24h——trace 库是单连接互斥锁，全表分钟聚合实测 ~600ms 占锁、24h 窗口 ~50ms |
| `/traces` | traces.db | 新增 `startDate`/`endDate`（本地零点、end 含当天，与 stats 系同语义）；每行附 `creditUsd`/`officialUsd` |
| `/stats/timeseries` `by-model` `by-credential` | 聚合器 | 补 cache token 列与成本字段；by-model 另有 `discountRatio`。分组筛选路径没有 凭据×模型 维度，timeseries 的 `officialUsd` 在该路径为 `None` |

**TPM 口径注意**：trace 在请求**结束**时落库，token 整体计入完成分钟；长流式请求的
产出不按时长摊薄。作为承载/峰值参考足够，不能当精确的瞬时速率用（那是 rate 环的活）。

## 6. RPM / TPM

`rate_ring.rs`：120 个分钟桶、每桶 8 个 `AtomicU64`、`minute % 120` 直接索引，写入 O(1) 无锁。接口 `GET /api/admin/stats/rate`，面板在概览页「实时速率」。

**为什么不复用 `UsageAggregator`**：那边 `upsert_bucket` 是线性 `find` → 新桶全量 `sort_by_key` → 超容量 `remove(0)`（Vec 头删是 O(n) 搬移）的三连，`BucketEntry` 还挂 5 个 HashMap（2 个嵌套）。744 个小时桶勉强扛，1440 个分钟桶就是写入热点 + 内存按「桶数 × Key 数 × 模型数」放大。

### 6.1 双口径，不是一个数

| 口径 | 计在哪 | 含义 |
|---|---|---|
| 入口 RPM | `UsageRecordHook::record` | 外部请求数，**真实流量** |
| 上游 RPM | `KiroProvider::emit_attempt` | provider 跳数（含重试与故障转移），**上游压力** |

一次外部请求故障转移 3 个凭据 = 入口 1 / 上游 3。混在一起既高估流量又看不出重试放大，所以 `retryAmplification` 直接给比值。生产实测：入口 3 / 上游 9 → 3.0。

**上游计数刻意不挂 `TraceSink`**：`emit_attempt` 在 sink 为 `None` 时早退，挂那儿等于"关掉 traces.db → 上游 RPM 归零"。计数放在早退**之前**，所以 `emit_attempt` 从关联函数改成了方法（31 个调用点）。

**环全局只有一个实例**：由 anthropic 路由建立、塞进 `KiroProvider`，主程序再 `provider.rate_ring()` 取回给 Admin。按自然装配顺序写，Admin 与 API 会各建一个环、各只数一半流量。

### 6.2 TPM 两个口径

`tpmTotal` 含缓存读取，`tpmBillable` 不含。生产实测这两个数能差几十倍（100/50/20 对 9000 缓存 = 9170 vs 170），只暴露一个没法解释。

### 6.3 读数的两个约定

- **取上一个完整分钟**，不取当前分钟 —— 当前分钟还在累加，读出来偏低
- 槽位带**代次标记**（`minute` 兼作代次），读写都校验，2 小时前的残留不会被当成当前分钟；`reset_to` 先清计数再宣告归属
- 缺口**补零**而非缺项，前端时间轴才连续
- 采集层未注入时接口回 **503 而不是一堆 0** —— 否则分不清"没装采集层"与"真的没流量"

### 6.4 与旧 `rpm_window` 的关系

`token_manager` 里那个 `rpm_window: VecDeque<Instant>` **是限流闸门，不是指标**：默认关闭、关闭时窗口被主动 `clear()`（连原始素材都不留）、内存态重启归零、Admin 只暴露配置不暴露当前速率。它记的是"被选中的凭据请求数"，作为限流对、作为 RPM 指标错。两者互不干扰，不要混用。

## 7. 故障转移与凭据调度

**重试预算**（`call_api_with_retry`）：

```
max_retries = (该分组凭据数 × MAX_RETRIES_PER_CREDENTIAL(3)).min(MAX_TOTAL_RETRIES(4))
```

注意是 `total_count_in_group` **按分组**算，不是全局。`MAX_TOTAL_RETRIES = 4` 是硬上限，**这是刻意权衡不是疏漏** —— 429 多为账号级速率配额，过多重试会在账号间连环撞墙、放大限流，故上限取小值 + 429 专用长退避。

代价真实存在：账号池扩到几十个时，故障转移事实上只试 4 次就放弃。设计意图与"大池子充分轮转"冲突，需按实际池子规模校准。

### 7.1 错误分类

| 类别 | 动作 | 触发 |
|---|---|---|
| 换凭据 | `report_quota_exhausted_for_request` | 402 额度用尽 |
| 换凭据（含 force-refresh） | `report_failure_for_request` | 401 / 403 认证失败 |
| 换凭据 + **永久禁用、不参与自愈** | `report_suspended_for_request` | 403 + 明确封禁文案 |
| 换凭据 + 临时冷却 | `report_account_throttled_for_request` | 429 + `suspicious activity` |
| 纯重试不换号 | 指数退避 | 网络错误 / 408 / 429(普通) / 5xx |
| **直接终止不重试** | — | 400；`is_client_validation_error`（`TOOL_USE_RESULT_MISMATCH` / `TOOL_SCHEMA_INVALID`）；524 |

最后一类的设计很关键：客户端消息数组违反协议是**客户端的错**，若按上游瞬态错误重试会触发冷却，把一个客户端错误放大成 503 风暴。集中识别在 endpoint 层，`map_provider_error` 映射成 400 兜底。

### 7.2 冷却与自愈

**冷却**：`throttled_until: Option<Instant>`，**不持久化、重启清空**，秒数取 `max(上游 Retry-After, account_throttle_cooldown_secs)`（默认 1800）。

**自愈**（`try_self_heal`）：某作用域内全部凭据不可用时，**只复活因 `TooManyFailures` 被禁用的**。`Suspended` / `QuotaExceeded` / `InvalidRefreshToken` **一律不参与**。三重约束：同凭据同模型才清连续轮数、`self_heal_min_interval_secs`(默认 300) 冷却、`self_heal_max_consecutive_rounds`(默认 5) 上限。状态跨重启持久化。

这套约束是为了打断「全禁 → 自愈 → 403 → 再禁」的死循环。

**`InvalidRefreshToken` 不参与自愈这一条，是下面那个危害为什么严重的原因。**

## 8. 单写者假设与多实例危害

**这个应用假设自己是数据目录的唯一写者。** 全仓无任何跨进程文件锁（`flock` / `LOCK_EX` / `O_EXCL` 零命中）。

`persist_credentials` 的注释自陈：整文件覆写，并发调用会互相踩踏。它用 `persist_lock: Mutex<()>` 防住 —— 但 `Mutex` 是**进程内锁，跨进程完全不设防**。

两个实例共享 `data/` 会这样：

```
蓝刷新凭据 → 拿到新 refresh_token → 写进 credentials.json
绿手里还是旧快照 → 它也落盘 → 把蓝的新 token 覆盖回旧的
旧 token 已被上游作废 → 下次刷新失败 → 标 InvalidRefreshToken → 账号掉线
                                          ↑ 而这类不参与自愈（§7.2），要人工救
```

同理受损：`client_api_keys.json`（用量统计互相丢）、`cache_metering.json`。只有 `traces.db` 安全 —— WAL 模式支持多进程。

**这直接决定部署方式**：常规蓝绿（两实例同时对着真凭据跑）在这个应用上不可行。现行做法是绿容器挂**独立数据目录且不放 credentials**，验证通过后 `docker compose up -d` 秒级替换（实测停机 12~17 秒）。要做真零停机，得先把单写者状态挪出去（Redis 或加文件锁），那是架构改动。

### 8.1 当前无任何检测 —— 缺口

⚠️ **启动时不检查数据目录是否被另一实例占用，运行时也没有任何提示。** 上面那条链路完全静默地发生。

设计过一版启动期 sentinel 检测（数据目录放 `.kiro-rs-instance.json`，`O_EXCL` 原子创建 + 记 pid，按 pid 是否活着区分"被占用 / 过期可接管"），但**未落地**。做的时候有三点要守：

1. **只 warn 不阻止启动** —— 误判导致服务起不来比漏报更糟
2. **判活行为要可注入**，不能用 `#[cfg(target_os)]` 门控 —— 平台门控的测试在非目标平台会静默走 fallback 分支、永远不验真正的语义
3. **必须写明盲区**：`/proc` 按 PID namespace 隔离，两个容器 bind mount 同一目录时判断可能错（查不到 → 误判过期；撞同号 → 误判占用）。可靠范围只有同主机非容器、同容器内、崩溃后的过期 sentinel。跨容器要可靠得引入 `flock` 或数据目录内 SQLite 排他锁 —— 仓里目前**无任何文件锁依赖**

## 9. 已知缺口索引

症状分诊、修复历史、以及三条我判错又推翻的结论在 `ANALYSIS.md`。这里只列**当前仍存在**的缺口，按性质分组：

**需要方案决定（不是实现问题）**

| 缺口 | 卡在哪 |
|---|---|
| cache 两套口径对外不可区分 | 分类已做（§4.2）但只落 debug 日志。要让运营侧看见需选：加 trace 列 / 响应加非标字段 / 改数字治根 —— 三者风险差别很大 |
| 镜像只在本机、未推 registry | 机器重建即丢。连续两版如此 |

**诊断缺口（有数据但没接线）**

| 缺口 | 现状 |
|---|---|
| 窗口表无交叉校验（§4.4） | 上游 `maxInputTokens` 已在反序列化，缺的只是比一次 |
| 多实例无检测（§8.1） | 完全静默 |
| 多实例下 CacheMeter 失效无提示 | 跨进程前缀链永不命中 → `cache_read` 恒 0，运营侧只看到"命中率莫名很低" |
| `tokenUsage` 残缺 payload 的真实频率 | 探针已完备（三判据 + `present` 掩码），**但 0.7.4 完全不解析 `tokenUsage`，所以历史数据回答不了**，只能从 2026-08-24 13:16 之后的样本统计 |

**性能与吞吐**

| 缺口 | 位置 |
|---|---|
| `upsert_bucket` O(n) 三连 + `ingest` 写锁跨两次 upsert | `usage_stats.rs` |
| `verify_and_touch` 每请求扫全表 | 抗时序攻击的**设计意图正确**，但 O(keys)/请求 |

**架构与维护性**

| 缺口 | 说明 |
|---|---|
| `/v1` 与 `/cc/v1` 两份近乎复制的 handler | 漂移已发生（§1） |
| `MAX_TOTAL_RETRIES = 4` 与大账号池冲突 | 是权衡不是 bug，需按池子规模校准（§7） |
| 共享 profileArn 占位符无运行时校验 | §2.1 |
| 单写者状态未外置 | 决定了不能做真蓝绿（§8） |

## 附：这份文档怎么用

- 想知道**系统怎么运作** → 本文
- 想知道**某个症状的根因、以及历史上判错过什么** → `ANALYSIS.md`
- 想知道 **RPM/TPM 的设计依据与实现前现状** → `RPM-TPM-ANALYSIS.md`

引用符号名而非行号是刻意的：初版 `ANALYSIS.md` 用了 37 条 `file.rs:NNN`，改了几轮后 14 条指向了错误位置。**指错比不指更坏**。
