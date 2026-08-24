import axios from 'axios'
import { storage } from '@/lib/storage'
import type {
  CredentialDistribution,
  ModelDistribution,
  OverviewStats,
  RateSnapshot,
  StatsFilter,
  StatsTimeFilter,
  TimeSeriesPoint,
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
