import { keepPreviousData, useQuery } from '@tanstack/react-query'
import { getTraces, getFailureStats, getTraceSummary } from '@/api/traces'
import type { TraceQuery } from '@/types/api'

/**
 * 请求链路查询 hook
 *
 * 复用与 stats 一致的刷新策略：30s 自动刷新、切换筛选时保留旧数据避免闪烁。
 * `enabled=false` 时不发请求（用于弹框未打开时的懒加载）。
 */
export function useTraces(query: TraceQuery, enabled = true) {
  return useQuery({
    queryKey: ['traces', query],
    queryFn: () => getTraces(query),
    enabled,
    refetchInterval: enabled ? 30_000 : false,
    staleTime: 10_000,
    placeholderData: keepPreviousData,
    refetchOnWindowFocus: false,
  })
}

/**
 * 按模型汇总当前筛选下的用量与成本（含合计行），给「请求日志」页的汇总条用。
 * 与 useTraces 共享同一套筛选参数、同样的自动刷新与切换保留旧数据策略。
 */
export function useTraceSummary(query: TraceQuery, enabled = true) {
  return useQuery({
    queryKey: ['traces', 'summary', query],
    queryFn: () => getTraceSummary(query),
    enabled,
    refetchInterval: enabled ? 30_000 : false,
    staleTime: 10_000,
    placeholderData: keepPreviousData,
    refetchOnWindowFocus: false,
  })
}

/** 按凭据的失败分类计数（鉴权/风控/其他），用于卡片分色展示 */
export function useFailureStats() {
  return useQuery({
    queryKey: ['traces', 'failure-stats'],
    queryFn: getFailureStats,
    refetchInterval: 30_000,
    staleTime: 10_000,
    refetchOnWindowFocus: false,
  })
}
