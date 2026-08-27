import { keepPreviousData, useMutation, useQuery } from '@tanstack/react-query'
import { toast } from 'sonner'
import { exportBilling, getBilling, getByCredential, getByModel, getModelCostAnalysis, getOverview, getPricingAdvice, getRate, getTimeSeries, getTpm } from '@/api/stats'
import { extractErrorMessage } from '@/lib/utils'
import type { StatsFilter, StatsTimeFilter, TpmDim } from '@/types/api'

/**
 * 统计接口共用配置
 *
 * - `staleTime: 25_000`：30s 自动刷新前不再触发后台 refetch（防止跨 Tab 切换抖动）
 * - `placeholderData: keepPreviousData`：切换 range 或 tab 期间保留上次数据，
 *   chart 组件输入引用稳定 → 不会卸载重挂
 * - `refetchOnWindowFocus: false`：Admin 面板长时间挂着时减少瞬时压力
 */
const COMMON = {
  refetchInterval: 30_000,
  staleTime: 25_000,
  placeholderData: keepPreviousData,
  refetchOnWindowFocus: false,
} as const

export function useOverview() {
  return useQuery({
    queryKey: ['stats', 'overview'],
    queryFn: getOverview,
    ...COMMON,
  })
}

function timeKey(time: StatsTimeFilter) {
  return [
    time.range ?? 'custom',
    time.startDate ?? '',
    time.endDate ?? '',
    time.granularity,
  ] as const
}

export function useTimeSeries(time: StatsTimeFilter, filter?: StatsFilter) {
  return useQuery({
    queryKey: ['stats', 'timeseries', ...timeKey(time), filter?.keyId ?? 'all', filter?.group ?? 'all'],
    queryFn: () => getTimeSeries(time, filter),
    ...COMMON,
  })
}

export function useByModel(time: StatsTimeFilter, filter?: StatsFilter) {
  return useQuery({
    queryKey: ['stats', 'by-model', ...timeKey(time), filter?.keyId ?? 'all', filter?.group ?? 'all'],
    queryFn: () => getByModel(time, filter),
    ...COMMON,
  })
}

export function useByCredential(time: StatsTimeFilter, filter?: StatsFilter) {
  return useQuery({
    queryKey: ['stats', 'by-credential', ...timeKey(time), filter?.keyId ?? 'all', filter?.group ?? 'all'],
    queryFn: () => getByCredential(time, filter),
    ...COMMON,
  })
}

/** 分维度 TPM/RPM 承载统计（数据源：请求日志，旁路查询）。 */
export function useTpm(dim: TpmDim, time: StatsTimeFilter, filter?: StatsFilter) {
  return useQuery({
    queryKey: [
      'stats',
      'tpm',
      dim,
      time.startDate ?? '',
      time.endDate ?? '',
      filter?.keyId ?? 'all',
      filter?.group ?? 'all',
    ],
    queryFn: () => getTpm(dim, time, filter),
    ...COMMON,
  })
}

/** 速率环轮询间隔。后端按整分钟聚合，比这更快只会重复读同一个桶。 */
const RATE_POLL_MS = 15_000

/**
 * 分钟级 RPM / TPM（速率环，最长覆盖 1440 分钟 = 24h）。
 *
 * 只支持"以当前时刻为终点、往回数 N 分钟"的相对窗口——调用方（RatePanel）需自行
 * 保证仅在选中的是这类窗口（1h/3h/6h/24h 预设）时把 `enabled` 置为 true；任意
 * 历史区间（7d/30d 预设或自定义日期）请改用 `useRateBuckets`，环回答不了"上周三
 * 那小时"这种问题。
 *
 * 与其它统计接口分开轮询：这里要 15s 一刷，合并到 `COMMON`（30s）会让"实时"变得
 * 不够实时；`placeholderData` 保留切换窗口时的旧数据，避免每次点预设按钮都闪一下。
 */
export function useRateRing(minutes: number, enabled: boolean) {
  return useQuery({
    queryKey: ['stats', 'rate', 'ring', minutes],
    queryFn: () => getRate(minutes),
    enabled,
    refetchInterval: enabled ? RATE_POLL_MS : false,
    staleTime: RATE_POLL_MS - 1000,
    placeholderData: keepPreviousData,
    refetchOnWindowFocus: false,
  })
}

/**
 * 速率环覆盖不到的窗口（7d/30d 预设或跨天的自定义区间）的回退查询：复用
 * `/stats/timeseries` 的小时/天分桶，由调用方（RatePanel）换算成"桶均值速率"
 * （calls / 桶时长）。
 *
 * 刻意不传 keyId/group 过滤——速率面板设计上是"看全局系统承载"，与页面顶部的
 * 入口 Key / 分组筛选无关，这一点与环模式保持一致。
 */
export function useRateBuckets(timeFilter: StatsTimeFilter, enabled: boolean) {
  return useQuery({
    queryKey: [
      'stats',
      'rate',
      'buckets',
      timeFilter.startDate ?? '',
      timeFilter.endDate ?? '',
      timeFilter.granularity,
    ],
    queryFn: () => getTimeSeries(timeFilter),
    enabled,
    ...COMMON,
  })
}

/**
 * 月度账单：按客户端 Key 汇总成本/应收/毛利，月份选择器（YYYY-MM）驱动。
 * 查询 key 里带上 month，切月即自动 refetch。
 */
export function useBilling(month: string) {
  return useQuery({
    queryKey: ['stats', 'billing', month],
    queryFn: () => getBilling({ month }),
    ...COMMON,
  })
}

/**
 * 定价建议：目标毛利率（分数，0.5 = 50%）+ raiseOnly 任一变化都要重新算；`month` 与
 * 账单页/月度总账弹窗共用同一个月份状态，切月联动，不单独维护一份。
 */
export function usePricingAdvice(month: string, targetMarginRate: number, raiseOnly: boolean) {
  return useQuery({
    queryKey: ['stats', 'pricing-advice', month, targetMarginRate, raiseOnly],
    queryFn: () => getPricingAdvice({ month, targetMargin: targetMarginRate, raiseOnly }),
    ...COMMON,
  })
}

/**
 * 模型成本分析：按模型汇总成本/折扣/缓存表现，月份选择器（YYYY-MM）驱动，
 * 与「月度总账」共用同一种月份心智但各自持有独立的 month 状态（两个弹窗不会
 * 同时打开，没有联动必要）。
 */
export function useModelCostAnalysis(month: string) {
  return useQuery({
    queryKey: ['stats', 'model-cost-analysis', month],
    queryFn: () => getModelCostAnalysis({ month }),
    ...COMMON,
  })
}

/**
 * 导出单个客户端 Key 指定月份的对账单 CSV。
 *
 * 每个调用方各自持有一份 mutation 状态（不共享 queryKey），因此表格里逐行调用本 hook
 * 即可拿到互不干扰的按钮 pending 态。成功后直接触发浏览器下载；若服务端标出了缺日志的
 * 日期，额外弹一条提示——"当天无日志"和"当天无消耗"是两回事，对账时不能混为一谈。
 * 服务端标出了本期无法解析的日志行数时同样弹一条提示——这些请求的金额未知，导出的
 * CSV 里没有它们。
 */
export function useExportBilling() {
  return useMutation({
    mutationFn: ({ keyId, month }: { keyId: number; month: string; keyName: string }) =>
      exportBilling(keyId, month),
    onSuccess: (result, variables) => {
      const url = URL.createObjectURL(result.blob)
      const a = document.createElement('a')
      a.href = url
      a.download = result.filename || `billing-${variables.keyName}-${variables.month}.csv`
      document.body.appendChild(a)
      a.click()
      document.body.removeChild(a)
      URL.revokeObjectURL(url)
      if (result.missingDays && result.missingDays.length > 0) {
        toast.info(`以下日期无日志：${result.missingDays.join('、')}`)
      }
      if (result.malformedLines && result.malformedLines > 0) {
        toast.info(`导出的对账单中有 ${result.malformedLines} 行日志无法解析，金额未计入`)
      }
    },
    onError: (err) => {
      toast.error('导出对账单失败：' + extractErrorMessage(err))
    },
  })
}
