import { useState } from 'react'
import { Card, CardContent } from '@/components/ui/card'
import { Button } from '@/components/ui/button'
import { useTpm } from '@/hooks/use-stats'
import { formatNumber } from '@/lib/utils'
import type { StatsFilter, StatsTimeFilter, TpmDim } from '@/types/api'

const DIM_OPTIONS: { label: string; value: TpmDim }[] = [
  { label: '按入口 Key', value: 'key' },
  { label: '按上游凭据', value: 'credential' },
]

/**
 * TPM 承载面板：分「入口 Key / 上游凭据」维度展示分钟级峰值承载能力。
 *
 * 数据源是请求日志（traces.db）旁路查询，不是 rate 环——所以会跟随页面上的
 * 时间范围 / 入口 Key / 分组筛选联动，与 RatePanel（固定 120 分钟窗口、不受
 * 筛选影响）刻意不同，两者互补：RatePanel 看"现在"，本面板看"筛选窗口内谁扛得住"。
 */
export function TpmPanel({
  timeFilter,
  statsFilter,
}: {
  timeFilter: StatsTimeFilter
  statsFilter: StatsFilter
}) {
  const [dim, setDim] = useState<TpmDim>('key')
  const { data, isLoading } = useTpm(dim, timeFilter, statsFilter)
  const entities = data?.entities ?? []

  return (
    <Card className="mb-6">
      <CardContent className="p-4 sm:p-5">
        <div className="mb-3 flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
          <div>
            <h2 className="text-base font-semibold tracking-tight">TPM 承载（分维度）</h2>
            <p className="text-[12px] text-muted-foreground">
              峰值 = 窗口内单分钟最大 token 消耗（含缓存读）· 数据源：请求日志
            </p>
          </div>
          <div className="grid grid-cols-2 gap-1 rounded-md border border-border/60 p-0.5 sm:flex sm:items-center">
            {DIM_OPTIONS.map((opt) => (
              <Button
                key={opt.value}
                size="sm"
                variant={dim === opt.value ? 'default' : 'ghost'}
                className="h-8 rounded-md px-3 text-xs"
                onClick={() => setDim(opt.value)}
              >
                {opt.label}
              </Button>
            ))}
          </div>
        </div>
        {data?.traceEnabled === false && (
          <p className="mb-3 rounded-md bg-amber-500/10 px-2.5 py-1.5 text-[11px] text-amber-600">
            请求日志已关闭，仅显示历史数据
          </p>
        )}
        {isLoading ? (
          <div className="flex h-24 items-center justify-center text-[13px] text-muted-foreground">
            加载中…
          </div>
        ) : entities.length === 0 ? (
          <div className="flex h-24 items-center justify-center text-[13px] text-muted-foreground">
            窗口内无数据
          </div>
        ) : (
          <div className="overflow-x-auto text-[12px]">
            <table className="w-full min-w-[560px]">
              <thead className="text-muted-foreground">
                <tr>
                  <th className="text-left font-medium pb-1">名称</th>
                  <th className="text-right font-medium pb-1">峰值 TPM</th>
                  <th className="text-right font-medium pb-1">计费峰值 TPM</th>
                  <th className="text-right font-medium pb-1">峰值 RPM</th>
                  <th className="text-right font-medium pb-1">均值 TPM</th>
                  <th className="text-right font-medium pb-1">活跃分钟</th>
                  <th className="text-right font-medium pb-1">总调用</th>
                </tr>
              </thead>
              <tbody>
                {entities.map((e) => (
                  <tr key={e.entityId} className="border-t border-border/40">
                    <td className="max-w-[220px] truncate py-1.5">{e.label}</td>
                    <td className="text-right tabular-nums">{formatNumber(e.peakTpmTotal)}</td>
                    <td className="text-right tabular-nums">{formatNumber(e.peakTpmBillable)}</td>
                    <td className="text-right tabular-nums">{formatNumber(e.peakRpm)}</td>
                    <td className="text-right tabular-nums">{formatNumber(e.avgTpmActive)}</td>
                    <td className="text-right tabular-nums">{formatNumber(e.activeMinutes)}</td>
                    <td className="text-right tabular-nums">{formatNumber(e.totalCalls)}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </CardContent>
    </Card>
  )
}
