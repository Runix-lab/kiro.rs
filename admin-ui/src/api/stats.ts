import axios from 'axios'
import { storage } from '@/lib/storage'
import type {
  BillingResponse,
  CredentialDistribution,
  ModelDistribution,
  OverviewStats,
  RateSnapshot,
  StatsFilter,
  StatsTimeFilter,
  TimeSeriesPoint,
  TpmDim,
  TpmStats,
} from '@/types/api'

const api = axios.create({
  baseURL: '/api/admin',
  timeout: 15000,
  headers: { 'Content-Type': 'application/json' },
})

api.interceptors.request.use((config) => {
  const apiKey = storage.getApiKey()
  if (apiKey) config.headers['x-api-key'] = apiKey
  return config
})

export async function getOverview(): Promise<OverviewStats> {
  const { data } = await api.get<OverviewStats>('/stats/overview')
  return data
}

function statsParams(time: StatsTimeFilter, filter?: StatsFilter) {
  return {
    ...time,
    ...(filter?.keyId !== undefined ? { keyId: filter.keyId } : {}),
    ...(filter?.group ? { group: filter.group } : {}),
  }
}

export async function getTimeSeries(time: StatsTimeFilter, filter?: StatsFilter): Promise<TimeSeriesPoint[]> {
  const { data } = await api.get<TimeSeriesPoint[]>('/stats/timeseries', {
    params: statsParams(time, filter),
  })
  return data
}

export async function getByModel(time: StatsTimeFilter, filter?: StatsFilter): Promise<ModelDistribution[]> {
  const { data } = await api.get<ModelDistribution[]>('/stats/by-model', {
    params: statsParams(time, filter),
  })
  return data
}

export async function getByCredential(time: StatsTimeFilter, filter?: StatsFilter): Promise<CredentialDistribution[]> {
  const { data } = await api.get<CredentialDistribution[]>('/stats/by-credential', {
    params: statsParams(time, filter),
  })
  return data
}

/**
 * 取分钟级 RPM / TPM。
 *
 * 数据来自后端内存速率环，**不受 trace 开关影响**，也不接受 range / group 过滤 ——
 * 环是固定 120 分钟窗口且只存标量，没有维度可筛。
 *
 * 采集层未注入时后端回 503，调用方应据此显示"未启用"而不是一堆 0，
 * 否则分不清"没装采集层"与"真的没流量"。
 */
export async function getRate(): Promise<RateSnapshot> {
  const { data } = await api.get<RateSnapshot>('/stats/rate')
  return data
}

/**
 * 分维度（入口 Key / 上游凭据）分钟级 TPM/RPM 承载统计。
 *
 * 数据源是 traces.db（旁路查询，与 rate 环无关），因此接受与 /traces 一致的
 * 时间与筛选参数；trace 治理开关关闭时后端仍会回历史数据，通过 `traceEnabled`
 * 字段告知前端只是"没有新数据"而非接口异常。
 */
export async function getTpm(
  dim: TpmDim,
  time: StatsTimeFilter,
  filter?: StatsFilter,
): Promise<TpmStats> {
  const params: Record<string, string> = { dim }
  if (time.startDate) params.startDate = time.startDate
  if (time.endDate) params.endDate = time.endDate
  if (filter?.keyId !== undefined) params.keyId = String(filter.keyId)
  if (filter?.group) params.group = filter.group
  const { data } = await api.get<TpmStats>('/stats/tpm', { params })
  return data
}

/**
 * 月度账单：按客户端 Key 汇总成本（可信）/ 应收（口径见各行 receivableBasis）/ 毛利。
 *
 * 后端也接受 startDate+endDate 自定义窗口，不传 month 时默认当月至今；页面固定用月份
 * 选择器驱动，因此这里只暴露 month 参数。
 */
export async function getBilling(params: { month?: string }): Promise<BillingResponse> {
  const { data } = await api.get<BillingResponse>('/billing', { params })
  return data
}
