import { useState } from 'react'
import { Card, CardContent } from '@/components/ui/card'
import { Button } from '@/components/ui/button'
import { useTpm } from '@/hooks/use-stats'
import { formatCredits, formatDiscount, formatNumber, formatUsd } from '@/lib/utils'
import { TD_LABEL, TD_NUM, TD_NUM_STRONG, TH_LABEL, TH_NUM, TH_TEXT } from '@/lib/table-styles'
import type { StatsFilter, StatsTimeFilter, TpmDim, TpmEntityStats } from '@/types/api'

const DIM_OPTIONS: { label: string; value: TpmDim }[] = [
  { label: '按入口 Key（用户）', value: 'key' },
  { label: '按上游凭据（账号）', value: 'credential' },
]

/**
 * 用量与承载面板：分「入口 Key / 上游凭据」维度展示每个实体的速率、用量与错误率，
 * 顶部一行是全系统合计。
 *
 * 数据源是请求日志（traces.db）旁路查询，不是 rate 环——所以会跟随页面上的时间 /
 * 入口 Key / 分组筛选联动，与 RatePanel（固定 120 分钟窗口、不受筛选影响）互补：
 * RatePanel 看"此刻"，本面板看"筛选窗口内谁扛了多少、错了多少"。
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
  const totals = data?.totals

  return (
    <Card className="mb-6">
      <CardContent className="p-4 sm:p-5">
        <div className="mb-3 flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
          <div>
            <h2 className="text-base font-semibold tracking-tight">用量与承载</h2>
            <p className="text-[12px] text-muted-foreground">
              峰值 = 窗口内单分钟最大量 · 合计按分钟合并全部实体后取峰值（非各行相加）· 数据源：请求日志
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

        {totals && totals.totalCalls > 0 && <TotalsStrip totals={totals} />}

        {isLoading ? (
          <div className="flex h-24 items-center justify-center text-[13px] text-muted-foreground">
            加载中…
          </div>
        ) : entities.length === 0 ? (
          <div className="flex h-24 items-center justify-center text-[13px] text-muted-foreground">
            窗口内无数据
          </div>
        ) : (
          <div className="overflow-x-auto text-sm">
            <table className="w-full min-w-[1260px]">
              <thead className="text-muted-foreground">
                <tr>
                  <th className={TH_LABEL}>{dim === 'key' ? '入口 Key' : '上游凭据'}</th>
                  <th className={TH_TEXT}>主用模型</th>
                  <th className={TH_NUM}>峰值 TPM</th>
                  <th className={TH_NUM}>计费峰值</th>
                  <th className={TH_NUM}>峰值 RPM</th>
                  <th className={TH_NUM}>均值 TPM</th>
                  <th className={TH_NUM}>均值 RPM</th>
                  <th className={TH_NUM}>活跃分钟</th>
                  <th className={TH_NUM}>总调用</th>
                  <th className={TH_NUM}>成功率</th>
                  <th className={TH_NUM}>Credit</th>
                  <th className={TH_NUM}>实付$</th>
                  <th className={TH_NUM}>官方$</th>
                  <th className={TH_NUM}>折扣</th>
                </tr>
              </thead>
              <tbody>
                {entities.map((e) => (
                  <EntityRow key={e.entityId} e={e} />
                ))}
                {totals && totals.totalCalls > 0 && (
                  <tr className="border-t-2 border-border/70 font-medium">
                    <td className="py-2.5 pr-4 whitespace-nowrap">合计</td>
                    <td className="py-2.5 pl-3 whitespace-nowrap text-muted-foreground">—</td>
                    <td className={TD_NUM}>{formatNumber(totals.peakTpmTotal)}</td>
                    <td className={TD_NUM}>{formatNumber(totals.peakTpmBillable)}</td>
                    <td className={TD_NUM}>{formatNumber(totals.peakRpm)}</td>
                    <td className={TD_NUM}>{formatNumber(totals.avgTpmActive)}</td>
                    <td className={TD_NUM}>{formatNumber(totals.avgRpmActive)}</td>
                    <td className={TD_NUM}>{formatNumber(totals.activeMinutes)}</td>
                    <td className={TD_NUM}>{formatNumber(totals.totalCalls)}</td>
                    <td className={TD_NUM}>{totals.successRate.toFixed(1)}%</td>
                    <td className={TD_NUM}>{formatCredits(totals.credits)}</td>
                    <td className={TD_NUM}>{formatUsd(totals.creditUsd)}</td>
                    <td className={TD_NUM}>{formatUsd(totals.officialUsd)}</td>
                    <td className={TD_NUM_STRONG}>{formatDiscount(totals.discountRatio)}</td>
                  </tr>
                )}
              </tbody>
            </table>
          </div>
        )}
      </CardContent>
    </Card>
  )
}

/** 全系统合计条：一眼看总承载，不用去表尾找。 */
function TotalsStrip({ totals }: { totals: TpmEntityStats }) {
  const items: { label: string; value: string; hint?: string }[] = [
    { label: '峰值 TPM', value: formatNumber(totals.peakTpmTotal), hint: '全系统单分钟最大 token（含缓存读）' },
    { label: '计费峰值 TPM', value: formatNumber(totals.peakTpmBillable), hint: '不含缓存读' },
    { label: '峰值 RPM', value: formatNumber(totals.peakRpm), hint: '全系统单分钟最大请求数' },
    { label: '均值 TPM', value: formatNumber(totals.avgTpmActive), hint: '活跃分钟平均' },
    { label: '均值 RPM', value: formatNumber(totals.avgRpmActive), hint: '活跃分钟平均' },
    { label: '成功率', value: `${totals.successRate.toFixed(1)}%`, hint: `异常 ${formatNumber(totals.errors)} 次` },
    { label: '实付', value: formatUsd(totals.creditUsd), hint: `${formatCredits(totals.credits)} credit` },
    { label: '折扣', value: formatDiscount(totals.discountRatio), hint: `官方 ${formatUsd(totals.officialUsd)}` },
  ]
  return (
    <div className="mb-4 grid grid-cols-2 gap-3 rounded-lg border border-border/60 bg-muted/30 p-3 sm:grid-cols-4 lg:grid-cols-8">
      {items.map((it) => (
        <div key={it.label} title={it.hint}>
          <div className="text-[11px] text-muted-foreground">{it.label}</div>
          <div className="text-lg font-semibold tabular-nums leading-tight">{it.value}</div>
          {it.hint && <div className="text-[10px] text-muted-foreground/70">{it.hint}</div>}
        </div>
      ))}
    </div>
  )
}

function EntityRow({ e }: { e: TpmEntityStats }) {
  // 成功率低于 95% 标红、低于 99% 标黄——错误率是这张表最该被一眼看到的信号
  const rateClass =
    e.successRate < 95 ? 'text-destructive' : e.successRate < 99 ? 'text-amber-600' : ''
  return (
    <tr className="border-t border-border/40">
      <td className={TD_LABEL} title={e.label}>
        {e.label}
      </td>
      <td
        className="max-w-[200px] truncate py-2.5 pl-3 text-muted-foreground"
        title={e.topModel ? `${e.topModel}（占比 ${e.topModelShare.toFixed(0)}%）` : undefined}
      >
        {e.topModel ? (
          <>
            {e.topModel}
            <span className="ml-1 text-[11px] text-muted-foreground/70">
              {e.topModelShare.toFixed(0)}%
            </span>
          </>
        ) : (
          '—'
        )}
      </td>
      <td className={TD_NUM}>{formatNumber(e.peakTpmTotal)}</td>
      <td className={TD_NUM}>{formatNumber(e.peakTpmBillable)}</td>
      <td className={TD_NUM}>{formatNumber(e.peakRpm)}</td>
      <td className={TD_NUM}>{formatNumber(e.avgTpmActive)}</td>
      <td className={TD_NUM}>{formatNumber(e.avgRpmActive)}</td>
      <td className={TD_NUM}>{formatNumber(e.activeMinutes)}</td>
      <td className={TD_NUM}>{formatNumber(e.totalCalls)}</td>
      <td className={`${TD_NUM} ${rateClass}`}>
        {e.successRate.toFixed(1)}%
        {e.errors > 0 && (
          <span className="ml-1 text-[11px] text-muted-foreground/70">
            ({formatNumber(e.errors)})
          </span>
        )}
      </td>
      <td className={TD_NUM}>{formatCredits(e.credits)}</td>
      <td className={TD_NUM}>{formatUsd(e.creditUsd)}</td>
      <td className={TD_NUM}>{formatUsd(e.officialUsd)}</td>
      <td className={TD_NUM_STRONG}>{formatDiscount(e.discountRatio)}</td>
    </tr>
  )
}
