import axios from 'axios'
import { storage } from '@/lib/storage'
import type {
  FailureStatsMap,
  StoredPrompt,
  TracePage,
  TracePromptNotFound,
  TracePromptResult,
  TraceQuery,
  TraceSummary,
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

export async function getTraces(query: TraceQuery): Promise<TracePage> {
  const params: Record<string, string> = {}
  if (query.status) params.status = query.status
  if (query.errorType) params.errorType = query.errorType
  if (query.credentialId != null) params.credentialId = String(query.credentialId)
  if (query.keyId != null) params.keyId = String(query.keyId)
  if (query.failedAttemptCredentialId != null)
    params.failedAttemptCredentialId = String(query.failedAttemptCredentialId)
  if (query.model) params.model = query.model
  if (query.group) params.group = query.group
  if (query.onlyFailed) params.onlyFailed = 'true'
  if (query.startDate) params.startDate = query.startDate
  if (query.endDate) params.endDate = query.endDate
  if (query.limit != null) params.limit = String(query.limit)
  if (query.offset != null) params.offset = String(query.offset)
  const { data } = await api.get<TracePage>('/traces', { params })
  return data
}

export async function getFailureStats(): Promise<FailureStatsMap> {
  const { data } = await api.get<FailureStatsMap>('/traces/failure-stats')
  return data
}

/**
 * 与 /traces 同一套筛选参数（limit/offset 后端忽略，这里也不传），
 * 按模型汇总当前筛选下的用量与成本，另附合计行。
 */
export async function getTraceSummary(query: TraceQuery): Promise<TraceSummary> {
  const params: Record<string, string> = {}
  if (query.status) params.status = query.status
  if (query.errorType) params.errorType = query.errorType
  if (query.keyId != null) params.keyId = String(query.keyId)
  if (query.model) params.model = query.model
  if (query.group) params.group = query.group
  if (query.onlyFailed) params.onlyFailed = 'true'
  if (query.startDate) params.startDate = query.startDate
  if (query.endDate) params.endDate = query.endDate
  const { data } = await api.get<TraceSummary>('/traces/summary', { params })
  return data
}

/**
 * 取回某条 trace 的原始请求体（懒加载，只在运营点「查看原始请求」时调用）。
 *
 * 后端 404 有两种截然不同的原因（留存从未开启 / 该条超出保留期或超体积上限），
 * 且各自带一句面向运营的 hint 文案。这里用 `validateStatus` 放行 404，
 * 把它编码进返回值的 `found: false` 分支，而不是当异常抛出——调用方需要
 * 原样展示 hint，不是套一句通用的“未找到”。
 */
export async function getTracePrompt(traceId: string): Promise<TracePromptResult> {
  const { data, status } = await api.get<StoredPrompt | TracePromptNotFound>(
    `/traces/${encodeURIComponent(traceId)}/prompt`,
    { validateStatus: (s) => s === 200 || s === 404 },
  )
  if (status === 404) {
    return { found: false, notFound: data as TracePromptNotFound }
  }
  return { found: true, prompt: data as StoredPrompt }
}
