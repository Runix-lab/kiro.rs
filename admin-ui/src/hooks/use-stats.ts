import { keepPreviousData, useMutation, useQuery } from '@tanstack/react-query'
import { toast } from 'sonner'
import { exportBilling, getBilling, getByCredential, getByModel, getOverview, getTimeSeries, getTpm } from '@/api/stats'
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
 * 导出单个客户端 Key 指定月份的对账单 CSV。
 *
 * 每个调用方各自持有一份 mutation 状态（不共享 queryKey），因此表格里逐行调用本 hook
 * 即可拿到互不干扰的按钮 pending 态。成功后直接触发浏览器下载；若服务端标出了缺日志的
 * 日期，额外弹一条提示——"当天无日志"和"当天无消耗"是两回事，对账时不能混为一谈。
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
    },
    onError: (err) => {
      toast.error('导出对账单失败：' + extractErrorMessage(err))
    },
  })
}
