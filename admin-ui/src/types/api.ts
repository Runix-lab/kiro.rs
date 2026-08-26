// 凭据状态响应
export interface CredentialsStatusResponse {
  total: number
  available: number
  /** 优先级模式下的当前优先凭据 ID；均衡模式为 0 */
  currentId: number
  credentials: CredentialStatusItem[]
}

// 单个凭据状态
export interface CredentialStatusItem {
  id: number
  priority: number
  disabled: boolean
  failureCount: number
  /** 累计失败次数（所有失败类型，只增不减，仅手动重置归零） */
  totalFailureCount: number
  /** 是否为优先级模式下的当前优先凭据；均衡模式恒为 false */
  isCurrent: boolean
  expiresAt: string | null
  authMethod: string | null
  provider?: string | null
  hasProfileArn: boolean
  email?: string
  refreshTokenHash?: string
  apiKeyHash?: string
  maskedApiKey?: string
  successCount: number
  lastUsedAt: string | null
  hasProxy: boolean
  proxyUrl?: string
  refreshFailureCount: number
  disabledReason?: string
  /** 账号级风控冷却剩余秒数（>0 表示冷却中） */
  throttledRemainingSecs?: number
  endpoint: string
  /** 账号所属分组（可属于多个分组） */
  groups?: string[]
  /** 账号来源渠道（纯备注） */
  sourceChannel?: string
  /** 后端缓存的最近一次余额（5 分钟内） */
  balance?: BalanceResponse
  /** 余额缓存的更新时间（Unix 秒） */
  balanceUpdatedAt?: number
  /** 凭据添加（创建）时间（RFC3339 格式）；旧凭据缺失时为 undefined */
  createdAt?: string
}

// 余额响应
export interface BalanceResponse {
  id: number
  subscriptionTitle: string | null
  currentUsage: number
  usageLimit: number
  remaining: number
  usagePercentage: number
  nextResetAt: number | null
  /** 用户是否当前开启了超额 */
  overageEnabled?: boolean
  /** 账号订阅是否可以开启超额 */
  overageCapable?: boolean
  /** 上游 overageCapability 原始字符串，用于排查"未知"状态 */
  overageCapabilityRaw?: string
}

// 某凭据当前可用的模型列表响应
export interface AvailableModelsResponse {
  id: number
  selectionMode: 'specified' | 'priority' | 'balanced'
  models: AvailableModelItem[]
}

// 单个可用模型
export interface AvailableModelItem {
  modelId: string
  modelName?: string
  description?: string
  maxInputTokens?: number
  maxOutputTokens?: number
}

// 真实模型请求测试结果
export interface ModelTestResponse {
  modelId: string
  credentialId: number
  latencyMs: number
  responseText: string
  creditUsage?: number
  creditUnit?: string
}

// 成功响应
export interface SuccessResponse {
  success: boolean
  message: string
}

// 错误响应
export interface AdminErrorResponse {
  error: {
    type: string
    message: string
  }
}

// 请求类型
export interface SetDisabledRequest {
  disabled: boolean
}

export interface SetPriorityRequest {
  priority: number
}

// 添加凭据请求
export interface AddCredentialRequest {
  refreshToken?: string
  accessToken?: string
  profileArn?: string
  expiresAt?: string
  authMethod?: 'social' | 'idc' | 'api_key' | 'external_idp'
  provider?: string
  clientId?: string
  clientSecret?: string
  startUrl?: string
  /** 企业 SSO (external_idp) 的 OAuth2 Token 端点（external_idp 必填） */
  tokenEndpoint?: string
  /** 企业 SSO 的 OIDC Issuer URL（可选） */
  issuerUrl?: string
  /** 企业 SSO 授予的 scopes（空格分隔，可选） */
  scopes?: string
  priority?: number
  authRegion?: string
  apiRegion?: string
  machineId?: string
  proxyUrl?: string
  proxyUsername?: string
  proxyPassword?: string
  kiroApiKey?: string
  endpoint?: string
  email?: string
  groups?: string[]
  sourceChannel?: string
}

// 添加凭据响应
export interface AddCredentialResponse {
  success: boolean
  message: string
  credentialId: number
  email?: string
}

// 更新凭据请求（字段为 undefined 表示不修改，空字符串表示清除）
export interface UpdateCredentialRequest {
  email?: string
  proxyUrl?: string
  proxyUsername?: string
  proxyPassword?: string
  /** 账号所属分组（undefined 表示不修改，数组表示整体替换） */
  groups?: string[]
  /** 账号来源渠道（undefined 表示不修改，空串表示清除） */
  sourceChannel?: string
}

// 更新 refreshToken 请求
export interface UpdateRefreshTokenRequest {
  refreshToken: string
  accessToken?: string
  expiresAt?: string
}

// 代理健康状态
export type ProxyHealth = 'unknown' | 'healthy' | 'unhealthy'

// 代理池条目
export interface ProxyPoolEntry {
  id: number
  url: string
  label?: string
  enabled: boolean
  credentialCount: number
  health: ProxyHealth
  latencyMs?: number
  lastCheckedAt?: string
  consecutiveFailures: number
  autoDisabled: boolean
}

// 代理池列表响应
export interface ProxyPoolResponse {
  total: number
  proxies: ProxyPoolEntry[]
}

// 添加代理请求
export interface AddProxyRequest {
  url: string
  label?: string
}

// 批量添加代理请求
export interface BatchAddProxyRequest {
  urls: string[]
}

// 分配代理给凭据请求
export interface AssignProxyRequest {
  proxyId?: number | null
}

// 批量添加代理响应
export interface BatchAddProxyResponse {
  added: number
  errors: number
  proxies: ProxyPoolEntry[]
  errorMessages: string[]
}

// 单个代理健康检查响应
export interface ProxyCheckResponse {
  id: number
  health: ProxyHealth
  latencyMs?: number
  lastCheckedAt?: string
  enabled: boolean
  autoDisabled: boolean
}

// 全量健康检查响应
export interface ProxyCheckAllResponse {
  healthy: number
  unhealthy: number
  autoDisabled: number
}

// 轮询批量分配请求
export interface AssignRoundRobinRequest {
  credentialIds?: number[] | null
}

// 轮询批量分配响应
export interface AssignRoundRobinResponse {
  assigned: number
  proxyCount: number
}

// 全局代理配置
export interface GlobalProxyResponse {
  proxyUrl: string | null
}

export interface SetGlobalProxyRequest {
  proxyUrl: string | null
}

// 在线更新配置
export interface UpdateConfigResponse {
  /** 上一次更新前正在运行的版本号（带 v 前缀）；存在时可调用回退接口 */
  previousVersion?: string
  /** 上一次成功完成在线更新的时间（RFC3339） */
  lastAppliedAt?: string
  /** 是否已配置 GitHub Token（仅返回布尔，不回明文） */
  githubTokenSet: boolean
  /** 是否开启无人值守自动更新 */
  autoApply: boolean
  /** 自动更新触发时间（本地时区，HH:MM 24 小时制） */
  autoApplyTime: string
}

export interface SetUpdateConfigRequest {
  /** GitHub Personal Access Token；空字符串表示清除 */
  githubToken?: string
  autoApply?: boolean
  autoApplyTime?: string
}

/** GitHub API 限流状态（含 token 验证结果） */
export interface GitHubRateLimitInfo {
  /** 提供的 token 是否有效（无 token 时为 false 但仍能查到匿名限额） */
  valid: boolean
  /** 是否带 token 调用（false = 匿名查询） */
  authenticated: boolean
  /** 限流上限（匿名 60，认证 5000） */
  limit: number
  /** 剩余可用次数 */
  remaining: number
  /** 已用次数 */
  used: number
  /** 限流窗口重置时间（Unix 秒） */
  reset: number
  /** token 对应的用户名（可能为空） */
  login?: string
  /** 失败时的提示信息 */
  warning?: string
}

export interface ImageUpdateResponse {
  success: boolean
  message: string
  output?: string
  applied: boolean
  needRestart: boolean
}

export interface UpdateCheckInfo {
  currentVersion: string
  latestVersion: string
  hasUpdate: boolean
  buildType: string
  releaseName?: string
  releaseNotes?: string
  releaseUrl?: string
  publishedAt?: string
  checkedAt: string
  cached: boolean
  warning?: string
}

// 登录API密钥修改（adminApiKey —— 管理面板登录密钥）
export interface UpdateAdminKeyRequest {
  newKey: string
}

// IdC 设备授权登录
export interface StartIdcLoginRequest {
  region: string
  startUrl?: string
  priority?: number
  email?: string
  proxyUrl?: string
}

export interface StartIdcLoginResponse {
  sessionId: string
  userCode: string
  verificationUri: string
  verificationUriComplete?: string
  expiresAt: string
  pollInterval: number
}

export type PollIdcLoginResponse =
  | { status: 'pending' }
  | { status: 'success'; credentialId: number }
  | { status: 'expired' }

// Social 登录（Portal PKCE OAuth）
export interface StartSocialLoginRequest {
  priority?: number
  email?: string
  proxyUrl?: string
  authEndpoint?: string
}

/** 远程访问时手动完成 Social 登录：从浏览器地址栏粘贴的回调 URL 中提取参数 */
export interface CompleteSocialLoginRequest {
  code: string
  state: string
  loginOption?: string
  path?: string
}

export interface StartSocialLoginResponse {
  sessionId: string
  portalUrl: string
  expiresAt: string
}

export type PollSocialLoginResponse = PollIdcLoginResponse

// ============ 客户端 API Key 分发 ============

export interface ClientKeyItem {
  id: number
  /** 脱敏后的 Key（仅展示） */
  maskedKey: string
  name: string
  description?: string
  disabled: boolean
  createdAt: string
  lastUsedAt?: string
  totalCalls: number
  totalInputTokens: number
  totalOutputTokens: number
  totalCacheCreationTokens: number
  totalCacheReadTokens: number
  /** 绑定的账号分组（未绑定时为 undefined） */
  group?: string
  /** 是否系统密钥（由 config.json apiKey 同步，不可删除、可轮换） */
  isSystem: boolean
  /** 对客折扣系数（0.3 = 三折）；未设置为 null/undefined。与 billingPricePerCredit 互斥，后写者生效 */
  billingDiscount?: number | null
  /** 对客单价 $/credit；未设置为 null/undefined。与 billingDiscount 互斥，后写者生效 */
  billingPricePerCredit?: number | null
}

export interface ClientKeysResponse {
  total: number
  keys: ClientKeyItem[]
}

export interface CreateClientKeyRequest {
  name: string
  description?: string
  group?: string
}

/** 创建响应：明文 Key 仅在此处返回一次 */
export interface CreateClientKeyResponse {
  id: number
  key: string
  name: string
  createdAt: string
}

export interface UpdateClientKeyRequest {
  name?: string
  description?: string
  group?: string
  /** 对客折扣系数（0.3 = 三折）；传 0 清除该字段 */
  billingDiscount?: number
  /** 对客单价 $/credit；传 0 清除该字段 */
  billingPricePerCredit?: number
}

// ============ 用量统计 ============

export type StatsRange = '1h' | '3h' | '6h' | '24h' | '7d' | '30d'
export type StatsGranularity = 'hour' | 'day'

export interface StatsTimeFilter {
  range?: StatsRange
  startDate?: string
  endDate?: string
  granularity: StatsGranularity
}

export interface StatsFilter {
  /** 不传 = 全部；其它值 = 客户端 Key id */
  keyId?: number
  /** 按账号分组筛选（仅影响 timeseries / by-credential，by-model 不支持） */
  group?: string
}

export interface OverviewStats {
  todayCalls: number
  todayInputTokens: number
  todayOutputTokens: number
  todayErrors: number
  todayCredits: number
  weekCalls: number
  weekInputTokens: number
  weekOutputTokens: number
  weekCredits: number
  activeClientKeys: number
  activeCredentials: number
}

export interface TimeSeriesPoint {
  ts: string
  inputTokens: number
  outputTokens: number
  cacheCreationTokens: number
  cacheReadTokens: number
  calls: number
  errors: number
  credits: number
  /** 实付美金 = credits × 汇率 */
  creditUsd: number
  /** 官方牌价美金；null = 该桶无法按模型计价（如启用了分组筛选或全是未配价模型） */
  officialUsd: number | null
}

export interface ModelDistribution {
  model: string
  calls: number
  inputTokens: number
  outputTokens: number
  cacheCreationTokens: number
  cacheReadTokens: number
  errors: number
  credits: number
  /** 实付美金 = credits × 汇率 */
  creditUsd: number
  /** 官方牌价美金；null = 该模型未配价 */
  officialUsd: number | null
  /** 折扣比 = 实付 ÷ 官方（0.14 即 1.4 折）；未配价时为 null */
  discountRatio: number | null
}

export interface CredentialDistribution {
  credentialId: number
  email?: string
  calls: number
  inputTokens: number
  outputTokens: number
  cacheCreationTokens: number
  cacheReadTokens: number
  errors: number
  credits: number
  /** 实付美金 = credits × 汇率 */
  creditUsd: number
}

// ============ 月度账单 ============

/**
 * 单个客户端 Key 的月度账单行。
 *
 * `costUsd` 由 credits（上游计费事件量）换算而来，可信；`officialUsd` 依赖上游按 token
 * 明细下发牌价，未下发时由本地估算补齐，因此当 `receivableBasis === 'discount'` 时
 * `receivableUsd` / `marginUsd` 都是估算值，UI 需要显著标注。
 */
export interface BillingKeyRow {
  keyId: number
  name: string | null
  calls: number
  inputTokens: number
  outputTokens: number
  cacheCreationTokens: number
  cacheReadTokens: number
  credits: number
  /** 成本 = credits × creditUsdRate，可信 */
  costUsd: number
  /** 失败请求数（error 结果的调用数，已计入 calls） */
  errors: number
  /** 未配官方牌价的调用数（拉低 officialUsd 估算可信度，不影响 costUsd） */
  unpricedCalls: number
  /** 失败请求的成本（美金）：这些请求上游已计费但返回失败，我方承担成本，不向客户收取，不计入 receivableUsd */
  errorCredits: number
  /** 官方牌价美金；ESTIMATED —— 上游未下发 token 明细时由本地估算补齐 */
  officialUsd: number | null
  /** 对客折扣系数（0.3 = 三折）；未配置为 null */
  billingDiscount: number | null
  /** 对客单价 $/credit；未配置为 null */
  pricePerCredit: number | null
  /** 应收美金；无法定价时为 null */
  receivableUsd: number | null
  /** 应收口径：perCredit 可信；discount 依赖官方牌价估算；null = 未定价 */
  receivableBasis: 'perCredit' | 'discount' | null
  /** 毛利 = 应收 − 成本 */
  marginUsd: number | null
}

/** 月度账单合计（跨全部已定价 Key） */
export interface BillingTotals {
  costUsd: number
  officialUsd: number | null
  receivableUsd: number | null
  marginUsd: number | null
  marginRate: number | null
}

/** 有消耗但算不出应收的 Key（未定价 / 该窗口全部模型未配官方牌价等） */
export interface UnpricedKey {
  keyId: number
  name: string | null
  costUsd: number
  reason: string
}

/**
 * 有成功调用但上游计费 credit 为 0 的 Key —— 最危险的信号。
 * 若上游重命名了计费事件（meteringEvent），每一笔账都会静默变成 $0，
 * 页面看起来完全正常。月结前必须优先核实这份清单。
 */
export interface ZeroCreditKey {
  keyId: number
  name: string | null
  calls: number
}

/** GET /api/admin/billing 响应 */
export interface BillingResponse {
  windowStart: string | null
  windowEnd: string | null
  keys: BillingKeyRow[]
  totals: BillingTotals
  /** 月结前需要人工处理的"漏收"清单 */
  unpricedKeys: UnpricedKey[]
  /** 有成功调用但 credit 计费为 0 的 Key，见 ZeroCreditKey 注释 */
  zeroCreditKeys: ZeroCreditKey[]
  /** 账期内无法解析的用量日志行数；这些请求的金额未知且不可恢复 */
  malformedLines: number
  /** 账期内没有日志文件的日期。"那天没日志" ≠ "那天没消费"，月结时必须区分 */
  missingDays: string[]
  /** 账期时区，固定 Asia/Shanghai */
  timezone: string
  /** credit → USD 汇率 */
  creditUsdRate: number
  /** 成本口径固定可信 */
  costReliable: true
  /** 官方牌价口径固定为本地估算 */
  officialUsdEstimated: true
  note: string
}

// ============ 请求链路追踪 ============

/** 单次上游尝试 */
export interface TraceAttempt {
  attempt: number
  credentialId: number
  email?: string | null
  endpoint: string
  /** 上游 HTTP 状态码；null = 网络层失败 */
  httpStatus: number | null
  /** success / quota_exhausted / account_throttled / auth_failed / transient / network_error / bad_request / unknown */
  outcome: string
  /** 上游错误体片段（已截断） */
  errorSnippet: string | null
  durationMs: number
}

/** 一个外部请求的完整链路 */
export interface TraceRecord {
  traceId: string
  ts: string
  keyId: number
  /** masterApiKey = 历史 master 调用（已下线）；clientKey = 客户端 Key */
  keySource: 'masterApiKey' | 'clientKey'
  /** 发起请求的客户端 Key 名称（master 表示主 apiKey；管理员业务 Key 可为 null） */
  keyName?: string | null
  model: string
  isStream: boolean
  /** success / error / interrupted */
  finalStatus: string
  finalCredentialId: number
  finalEmail?: string | null
  errorType: string | null
  errorMessage: string | null
  totalAttempts: number
  durationMs: number
  /** 流式中断时已发送字节数 */
  interruptedAfterBytes: number | null
  /** 输入 token */
  inputTokens?: number
  /** 输出 token */
  outputTokens?: number
  /** 缓存创建 token */
  cacheCreationTokens?: number
  /** 缓存读取 token */
  cacheReadTokens?: number
  /** 总 token = input + output + cache_creation + cache_read */
  totalTokens?: number
  /** 费用（credits） */
  credits?: number
  /** 实付美金 = credits × 汇率 */
  creditUsd: number
  /** 官方牌价美金；null = 该模型未配价 */
  officialUsd: number | null
  /** 首 Token 延迟（毫秒，仅流式有值） */
  firstTokenMs?: number | null
  attempts: TraceAttempt[]
}

/** 链路查询参数 */
export interface TraceQuery {
  status?: string
  errorType?: string
  credentialId?: number
  /** 按发起请求的客户端 Key 筛选（0 = master apiKey） */
  keyId?: number
  /** 该凭据在某一跳失败过（即便 trace 最终成功）——用于凭据失败详情 */
  failedAttemptCredentialId?: number
  model?: string
  /** 按账号分组名筛选（只返回 final_credential_id 属于该分组的 trace） */
  group?: string
  onlyFailed?: boolean
  /** YYYY-MM-DD，必须与 endDate 同时提供；本地时区，endDate 含当天 */
  startDate?: string
  endDate?: string
  limit?: number
  offset?: number
}

/** 分页响应 */
export interface TracePage {
  records: TraceRecord[]
  total: number
}

/** 按模型汇总一行（/traces/summary） */
export interface TraceSummaryModelItem {
  model: string
  calls: number
  errors: number
  inputTokens: number
  outputTokens: number
  cacheCreationTokens: number
  cacheReadTokens: number
  credits: number
  creditUsd: number
  officialUsd: number | null
  discountRatio: number | null
}

/** /traces/summary 的合计行，字段与按模型行一致（少 model 字段） */
export type TraceSummaryTotals = Omit<TraceSummaryModelItem, 'model'>

/** GET /traces/summary 响应：与 /traces 同一套筛选参数，按模型汇总 + 合计 */
export interface TraceSummary {
  models: TraceSummaryModelItem[]
  totals: TraceSummaryTotals
  /** credit → USD 汇率，与 totals.creditUsd / totals.credits 一致 */
  creditUsdRate: number
}

/** TPM 统计的分维度口径 */
export type TpmDim = 'key' | 'credential'

/** 单个实体（入口 Key 或上游凭据）的 TPM/RPM 承载统计 */
export interface TpmEntityStats {
  entityId: number
  label: string
  /** 窗口内单分钟最大 token 消耗（全口径，含缓存读） */
  peakTpmTotal: number
  /** 窗口内单分钟最大 token 消耗（计费口径，不含缓存读） */
  peakTpmBillable: number
  peakRpm: number
  /** 窗口内有调用的分钟数 */
  activeMinutes: number
  /** 活跃分钟平均 TPM */
  avgTpmActive: number
  /** 活跃分钟平均 RPM */
  avgRpmActive: number
  totalTokens: number
  totalCalls: number
  errors: number
  /** 成功率百分比（0-100） */
  successRate: number
  credits: number
  creditUsd: number
  /** 官方牌价成本；该实体全部模型都未配价时为 null */
  officialUsd: number | null
  /** 折扣比 = 实付 ÷ 官方（单用户视角） */
  discountRatio: number | null
  /** 调用量最大的模型；窗口内无数据时为 null */
  topModel: string | null
  /** 该模型占该实体调用量的百分比（0-100） */
  topModelShare: number
}

/** GET /stats/tpm 响应 */
export interface TpmStats {
  dim: string
  /** 请求日志（trace）当前是否启用；false 时仅剩历史数据 */
  traceEnabled: boolean
  entities: TpmEntityStats[]
  /**
   * 全系统合计。峰值是"按分钟合并所有实体后取最大"，不是各实体峰值相加
   * （各自峰值多半落在不同分钟，相加得到的是从未发生过的数）。
   */
  totals: TpmEntityStats
}

/** 单凭据失败分类计数（鉴权 / 账号风控 / 其他） */
export interface FailureStats {
  auth: number
  throttle: number
  other: number
}

/** credentialId(字符串) → 失败分类计数 */
export type FailureStatsMap = Record<string, FailureStats>

// ============ 账号分组（独立实体）============

export interface GroupItem {
  name: string
  description?: string
  createdAt: string
  /** 引用计数：有多少个凭据带这个分组 */
  credentialCount: number
  /** 引用计数：有多少把客户端 Key 绑定这个分组 */
  clientKeyCount: number
}

export interface GroupsResponse {
  total: number
  groups: GroupItem[]
}

export interface CreateGroupRequest {
  name: string
  description?: string
}

export interface UpdateGroupRequest {
  /** 新名字；不传或与原名一致则不改名 */
  newName?: string
  /** 新备注；空字符串清除；undefined 保留原值 */
  description?: string
}

/** 速率环里的一个分钟桶。字段与后端 `MinuteSample` 一一对应。 */
export interface MinuteSample {
  /** Unix 分钟数（epoch 秒 / 60）。画图要 ×60000 换成毫秒。 */
  minute: number
  /** 入口请求数：外部请求，一次算一次。 */
  ingressCalls: number
  ingressErrors: number
  inputTokens: number
  outputTokens: number
  cacheWriteTokens: number
  cacheReadTokens: number
  /** 上游跳数：含重试与故障转移，一次外部请求可能有多跳。 */
  upstreamAttempts: number
  upstreamFailures: number
}

// ============ 调度策略 ============

/**
 * 调度取向：决定「按取向铺排」这条自动规则怎么排优先级，以及一批运行时设置
 * （负载均衡模式 / RPM 上限 / 限流冷却 / 429 是否换号）该配成什么样。
 * - manual：只运行额度守卫 + 首选层保护两条规则，手工设置的优先级不受影响
 * - throughput：按 80%/95% 分三档（前排/中间/溢出），接近耗尽的号提前减速
 * - highConcurrency：一条线两档，95% 以下全部并列跑满，击穿才退后排——冲峰值用
 * - conserve：剩余额度最多的排最前，几个账号一起匀速消耗
 * - drain：剩余额度最少的排最前，优先烧完这部分再轮到额度充足的账号
 *
 * 具体每档的推荐数值不在前端维护，见 `SchedulingProfilePreset` ——
 * 单一事实源在后端，前端只负责渲染。
 */
export type SchedulingProfile =
  | 'manual'
  | 'throughput'
  | 'highConcurrency'
  | 'conserve'
  | 'drain'

/** GET/PUT /api/admin/config/scheduling 的配置体 */
export interface SchedulingConfig {
  /** 总开关；关闭时不运行任何自动调度规则 */
  enabled: boolean
  /** 额度守卫阈值（百分比，0-100）：用量超过该值即触发降级 */
  demoteThresholdPct: number
  /** 触发额度守卫后临时改到的优先级（必须 > 50 基准档） */
  demoteTo: number
  /** 首选层保护：至少保留几个凭据共享最小（最优先）优先级值 */
  minTopTier: number
  /** 吞吐模式专用：用量低于该百分比的凭据进前排 */
  throughputBurnBelowPct: number
  /** 吞吐模式专用：用量达到该百分比的凭据退到溢出储备档 */
  throughputReserveAtPct: number
  /** 调度取向 */
  profile: SchedulingProfile
}

/**
 * GET /api/admin/config/scheduling/presets 里的一档推荐配置。
 *
 * 单一事实源：切换调度取向时，界面上所有联动的阈值与运行时设置都从这里取值，
 * 不在前端另外硬编码一套——两边迟早会对不上，且对不上时不会报错，只会表现为
 * 「界面显示的和实际路由行为不一致」。
 */
export interface SchedulingProfilePreset {
  profile: SchedulingProfile
  /** 界面上显示的名字 */
  label: string
  /** 一句话说明这个取向在干什么 */
  summary: string
  demoteThresholdPct: number
  demoteTo: number
  minTopTier: number
  throughputBurnBelowPct: number
  throughputReserveAtPct: number
  /** 这个取向要求的负载均衡模式（priority 是粘滞的，吞吐类必须 balanced） */
  loadBalancingMode: 'priority' | 'balanced'
  /** 是否启用单账号 RPM 上限 */
  accountRpmLimitEnabled: boolean
  /** 限流冷却秒数 */
  throttleCooldownSecs: number
  /** 普通 429 是否换号（而不是原地重试） */
  spillOnRateLimit: boolean
  /** 给运营看的注意事项，每一条都是真实的代价或风险 */
  caveat: string
}

/** GET /api/admin/config/scheduling/presets 的响应 */
export interface SchedulingPresetsResponse {
  presets: SchedulingProfilePreset[]
  note: string
}

/** 一键应用（或恢复默认）里单条设置的应用结果 */
export interface MaxThroughputAppliedItem {
  setting: string
  value: unknown
  /** dryRun 探测时为 true；真正执行后这个字段不存在 */
  dryRun?: boolean
}

/** 一键应用（或恢复默认）里单条设置的失败结果 */
export interface MaxThroughputFailedItem {
  setting: string
  value: unknown
  error: string
}

/** POST /api/admin/config/max-throughput 的请求参数 */
export interface MaxThroughputParams {
  /** 传 'highConcurrency' 走两档；不传走三档吞吐 */
  profile?: 'highConcurrency'
  /** 目标 RPM，按企业凭据数均摊（含 50% 余量） */
  targetRpm?: number
  /** 只回报会改什么，不真改 */
  dryRun?: boolean
  /** 回退到风险控制默认值，忽略 profile / targetRpm */
  restore?: boolean
}

/** POST /api/admin/config/max-throughput 的响应 */
export interface MaxThroughputResult {
  mode: 'maxThroughput' | 'riskControl'
  dryRun: boolean
  applied: MaxThroughputAppliedItem[]
  failed: MaxThroughputFailedItem[]
  priorityChanges: number
  note: string
}

/** GET /api/admin/config/scheduling/throughput-estimate 里的预估数值 */
export interface ThroughputEstimate {
  /** 前排档（尽情烧）凭据数 */
  frontTier: number
  /** 中间档凭据数 */
  midTier: number
  /** 溢出储备档凭据数 */
  reserveTier: number
  /** 预估并发上限 = 参与主力的凭据数 × 单凭据实测并发 */
  estimatedConcurrency: number
  /** 当前并发上限（按当前实际参与主力的凭据数算） */
  currentConcurrency: number
  /** 相对当前的提升倍数 */
  concurrencyGain: number
  /** 前排 + 中间档剩余额度合计 */
  usableCredits: number
  /** 按当前烧速还能撑多少小时；无烧速数据为 null */
  runwayHours: number | null
  /** 距离额度重置还有多少小时 */
  hoursToReset: number | null
  /** 可持续 TPM：可用额度 ÷ 距重置时间换算成 token */
  sustainableTpm: number | null
  /** 人话说明，直接显示给运营——断供缺口/溢出档数量都在这里 */
  notes: string[]
}

/** GET /api/admin/config/scheduling/throughput-estimate 的响应 */
export interface ThroughputEstimateResponse {
  estimate: ThroughputEstimate
  observations: {
    perCredentialConcurrency: number
    tokensPerCredit: number
    creditsPerHour: number
    hoursToReset: number
  }
  /** 口径说明：并发是能力上限不是保证值，可持续 TPM 由额度决定 */
  caveat: string
}

/** 单条调度变更的原因 */
export type SchedulingChangeReason =
  | 'quotaDemote'
  | 'quotaRestore'
  | 'topTierRefill'
  | 'profileRebalance'

/** 单条优先级变更 */
export interface SchedulingChange {
  id: number
  from: number
  to: number
  reason: SchedulingChangeReason
}

/** POST /api/admin/config/scheduling/run 的响应 */
export interface SchedulingRunResult {
  applied: number
  changes: SchedulingChange[]
}

/**
 * GET /api/admin/stats/rate?minutes=N 的响应。`minutes` 取最近 N 分钟（1~1440，
 * 缺省给满环）——这是"以当前时刻为终点"的相对窗口，不支持任意历史区间。
 *
 * 两组数字都是成对的，不要单看其中一个：入口与上游相差的倍数就是重试放大，
 * 全口径与计费口径 TPM 的差值就是缓存读取的量。
 */
export interface RateSnapshot {
  /** 这些速率对应的 Unix 分钟（上一个完整分钟，不是当前正在累加的那个）。 */
  minute: number
  /** 入口 RPM：真实外部流量。 */
  ingressRpm: number
  ingressErrors: number
  /** 上游 RPM：provider 跳数，看上游压力。 */
  upstreamRpm: number
  upstreamFailures: number
  /** TPM 全口径，含缓存读取。 */
  tpmTotal: number
  /** TPM 计费口径，不含缓存读取。 */
  tpmBillable: number
  peakIngressRpm: number
  peakUpstreamRpm: number
  peakTpmTotal: number
  peakTpmBillable: number
  /** 上游跳数 / 入口请求数。1.0 = 零重试。 */
  retryAmplification: number
  /**
   * 本次响应实际返回的分钟数——不是环容量（环容量固定 1440 = 24h）。
   * 请求 `minutes=60` 但进程刚重启、环还没填满时，这里会小于 60；
   * 前端要用这个值判断"数据不够"，不能把短序列误读成"流量骤降"。
   */
  windowMinutes: number
  /** 逐分钟序列，时间升序、缺口补零。 */
  series: MinuteSample[]
}
