import { keepPreviousData, useQuery } from '@tanstack/react-query'
import { getTraces, getFailureStats, getTraceSummary, getTracePrompt } from '@/api/traces'
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

/**
 * 单条请求的原始请求体（懒加载）。
 *
 * `enabled: false` 使其挂载时不随查询失效自动重新发起请求——请求体可能有几百 KB，
 * 展开一行 trace 不该顺带拉取它；仅在运营点击「查看原始请求」调用 `refetch()`
 * 时才会真正发出。404（留存未开启 / 超保留期 / 超体积上限）不当 react-query 的
 * error 处理，而是编码进 `getTracePrompt` 返回值的 `found: false` 分支，
 * 所以这里不需要处理 `isError`。
 */
export function useTracePrompt(traceId: string) {
  return useQuery({
    queryKey: ['traces', 'prompt', traceId],
    queryFn: () => getTracePrompt(traceId),
    enabled: false,
    retry: false,
    staleTime: Infinity,
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
