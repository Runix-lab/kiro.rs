import { useMemo, useState } from 'react'
import { AlertTriangle, Calendar } from 'lucide-react'
import { Card, CardContent } from '@/components/ui/card'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { useBilling } from '@/hooks/use-stats'
import { cn, formatCredits, formatUsd } from '@/lib/utils'
import type { BillingTotals, UnpricedKey } from '@/types/api'

function currentMonthValue(): string {
  const d = new Date()
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}`
}

function shiftMonth(month: string, delta: number): string {
  const [y, m] = month.split('-').map(Number)
  const d = new Date(y, m - 1 + delta, 1)
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}`
}

function MonthPicker({
  month,
  onChange,
}: {
  month: string
  onChange: (value: string) => void
}) {
  const thisMonth = useMemo(() => currentMonthValue(), [])
  const lastMonth = useMemo(() => shiftMonth(thisMonth, -1), [thisMonth])
  return (
    <div className="flex items-center gap-2">
      <div className="relative min-w-0">
        <Calendar className="pointer-events-none absolute left-2.5 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground" />
        <Input
          type="month"
          value={month}
          onChange={(e) => e.target.value && onChange(e.target.value)}
          className="h-8 w-[150px] rounded-md pl-8 text-xs"
        />
      </div>
      <div className="flex items-center gap-1 rounded-md border border-border/60 p-0.5">
        <Button
          size="sm"
          variant={month === thisMonth ? 'default' : 'ghost'}
          className="h-7 rounded-md px-2.5 text-xs"
          onClick={() => onChange(thisMonth)}
        >
          本月
        </Button>
        <Button
          size="sm"
          variant={month === lastMonth ? 'default' : 'ghost'}
          className="h-7 rounded-md px-2.5 text-xs"
          onClick={() => onChange(lastMonth)}
        >
          上月
        </Button>
      </div>
    </div>
  )
}

function StatBlock({
  label,
  value,
  caption,
  valueClassName,
}: {
  label: string
  value: string
  caption: string
  valueClassName?: string
}) {
  return (
    <div>
      <div className="text-[11px] font-medium text-muted-foreground sm:text-[13px]">{label}</div>
      <div
        className={cn(
          'mt-1.5 text-xl font-semibold tracking-tight tabular-nums sm:mt-2 sm:text-2xl',
          valueClassName,
        )}
      >
        {value}
      </div>
      <div className="mt-1 text-[11px] text-muted-foreground">{caption}</div>
    </div>
  )
}

function UnpricedBanner({ items }: { items: UnpricedKey[] }) {
  return (
    <Card className="mb-5 border-amber-500/40 bg-amber-500/5 sm:mb-6">
      <CardContent className="p-4 sm:p-5">
        <div className="mb-2 flex items-center gap-2 text-amber-600">
          <AlertTriangle className="h-4 w-4 shrink-0" />
          <h2 className="text-sm font-semibold">
            以下 Key 有消耗但收不出钱，月结前请处理
          </h2>
        </div>
        <ul className="space-y-1.5">
          {items.map((it) => (
            <li
              key={it.keyId}
              className="flex flex-wrap items-baseline gap-x-2 text-[12px] text-amber-700 dark:text-amber-400"
            >
              <span className="font-medium">{it.name ?? `#${it.keyId}`}</span>
              <span className="tabular-nums">{formatUsd(it.costUsd)} 成本</span>
              <span className="text-muted-foreground">{it.reason}</span>
            </li>
          ))}
        </ul>
      </CardContent>
    </Card>
  )
}

/**
 * 月度总账卡：凭据管理页的成本侧汇总（我方向上游付了多少、能收回多少、赚了多少）。
 *
 * 与 QuotaBurndownCard 紧邻——一个看额度烧多快，一个看这些消耗值多少钱，
 * 成本语境放在一起。逐 Key 明细不在此展示，那属于「客户端 Key」页的客户视角。
 */
export function MonthlySettlementCard() {
  const [month, setMonth] = useState(currentMonthValue)
  const { data, isLoading } = useBilling(month)
  const totals: BillingTotals | undefined = data?.totals
  const unpriced = data?.unpricedKeys ?? []
  const missingDays = data?.missingDays ?? []
  const totalCredits = useMemo(
    () => (data?.keys ?? []).reduce((s, k) => s + k.credits, 0),
    [data],
  )

  const marginPositive = (totals?.marginUsd ?? 0) >= 0
  const marginClassName =
    totals?.marginUsd == null ? '' : marginPositive ? 'text-emerald-600' : 'text-destructive'

  return (
    <>
      <Card className="mb-5 sm:mb-6">
        <CardContent className="p-3 sm:p-5">
          <div className="mb-4 flex flex-wrap items-center justify-between gap-3">
            <div>
              <h2 className="text-base font-semibold tracking-tight">月度总账</h2>
              <p className="mt-0.5 text-[11px] text-muted-foreground">
                我方对上游的月度成本与应收，成本口径可信，应收口径见「客户端 Key」页明细
              </p>
            </div>
            <MonthPicker month={month} onChange={setMonth} />
          </div>

          {isLoading ? (
            <div className="flex h-16 items-center justify-center text-[13px] text-muted-foreground">
              加载中…
            </div>
          ) : (
            <div className="grid grid-cols-2 gap-4 sm:grid-cols-4">
              <StatBlock
                label="成本"
                value={formatUsd(totals?.costUsd)}
                caption={`${formatCredits(totalCredits)} credits`}
              />
              <StatBlock
                label="应收"
                value={formatUsd(totals?.receivableUsd)}
                caption="客户应付金额（口径见明细）"
              />
              <StatBlock
                label="毛利"
                value={formatUsd(totals?.marginUsd)}
                valueClassName={marginClassName}
                caption="毛利 = 应收 − 成本"
              />
              <StatBlock
                label="毛利率"
                value={totals?.marginRate != null ? `${totals.marginRate.toFixed(1)}%` : '—'}
                caption="毛利 ÷ 应收"
              />
            </div>
          )}
        </CardContent>
      </Card>
      {missingDays.length > 0 && (
        <div className="mb-5 rounded-md border border-amber-500/40 bg-amber-500/10 p-3 sm:mb-6">
          <p className="text-[13px] font-medium text-amber-700 dark:text-amber-400">
            本期有 {missingDays.length} 天没有用量日志
          </p>
          <p className="mt-1 text-[11px] leading-relaxed text-muted-foreground">
            这些日期没有日志文件，账目里按零消费计算。若当天其实有流量，本期成本与应收都会偏低——
            月结前请确认是「当天确实没跑」还是「日志缺失」。
          </p>
          <p className="mt-1.5 font-mono text-[11px] text-muted-foreground">
            {missingDays.join('、')}
          </p>
        </div>
      )}
      {unpriced.length > 0 && <UnpricedBanner items={unpriced} />}
    </>
  )
}
