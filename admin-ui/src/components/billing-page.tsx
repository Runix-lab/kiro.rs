import { useMemo, useState } from 'react'
import { AlertTriangle, Calendar, Coins, Percent, TrendingUp } from 'lucide-react'
import { Card, CardContent } from '@/components/ui/card'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Badge } from '@/components/ui/badge'
import { Tooltip, TooltipContent, TooltipProvider, TooltipTrigger } from '@/components/ui/tooltip'
import { useBilling } from '@/hooks/use-stats'
import { cn, formatCredits, formatDiscount, formatNumber, formatUsd } from '@/lib/utils'
import type { BillingKeyRow, BillingTotals, UnpricedKey } from '@/types/api'

const ESTIMATED_HINT =
  '官方牌价依赖 token 明细，上游未下发时由本地估算补齐，此应收为参考值'

function currentMonthValue(): string {
  const d = new Date()
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}`
}

function shiftMonth(month: string, delta: number): string {
  const [y, m] = month.split('-').map(Number)
  const d = new Date(y, m - 1 + delta, 1)
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}`
}

/** 客户端 Key 明细行按前端聚合出的补充合计（后端 totals 只带金额，量级字段本地求和）。 */
function aggregateKeyRows(keys: BillingKeyRow[]) {
  return keys.reduce(
    (acc, k) => ({
      calls: acc.calls + k.calls,
      totalTokens:
        acc.totalTokens + k.inputTokens + k.outputTokens + k.cacheCreationTokens + k.cacheReadTokens,
      credits: acc.credits + k.credits,
    }),
    { calls: 0, totalTokens: 0, credits: 0 },
  )
}

/** 单行毛利率 = 毛利 ÷ 应收（后端只给合计的 marginRate，行级别按同口径本地推导用于展示）。 */
function rowMarginRate(row: BillingKeyRow): number | null {
  if (row.marginUsd == null || row.receivableUsd == null || row.receivableUsd === 0) return null
  return (row.marginUsd / row.receivableUsd) * 100
}

/**
 * 月度账单页：按客户端 Key 汇总本月成本 / 应收 / 毛利，用于月结对账。
 *
 * 成本（costUsd）来自上游计费事件（credits），可信；应收依赖 receivableBasis ——
 * perCredit（单价直乘）可信，discount（官方价 × 折扣）依赖本地估算的官方牌价，
 * 页面对所有 discount 口径的应收/毛利做了估算标注。
 */
export function BillingPage() {
  const [month, setMonth] = useState(currentMonthValue)
  const { data, isLoading } = useBilling(month)
  const keys = data?.keys ?? []
  const unpriced = data?.unpricedKeys ?? []
  const rowTotals = useMemo(() => aggregateKeyRows(keys), [keys])

  return (
    <TooltipProvider delayDuration={150}>
      <div>
        <PageHeader month={month} onChange={setMonth} />
        <SummaryCards
          totals={data?.totals}
          totalCredits={rowTotals.credits}
          creditUsdRate={data?.creditUsdRate}
        />
        {unpriced.length > 0 && <UnpricedBanner items={unpriced} />}
        <KeyTable keys={keys} totals={data?.totals} rowTotals={rowTotals} isLoading={isLoading} />
      </div>
    </TooltipProvider>
  )
}

function PageHeader({
  month,
  onChange,
}: {
  month: string
  onChange: (value: string) => void
}) {
  return (
    <div className="mb-6 flex flex-col gap-3 sm:flex-row sm:items-end sm:justify-between">
      <div>
        <h1 className="text-[28px] font-semibold tracking-tight leading-tight">月度账单</h1>
        <p className="mt-1 text-sm text-muted-foreground">
          按客户端 Key 汇总成本、应收与毛利，成本口径可信，应收口径见每行标注
        </p>
      </div>
      <MonthPicker month={month} onChange={onChange} />
    </div>
  )
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

function SummaryCards({
  totals,
  totalCredits,
  creditUsdRate,
}: {
  totals?: BillingTotals
  totalCredits: number
  creditUsdRate?: number
}) {
  const marginPositive = (totals?.marginUsd ?? 0) >= 0
  const cards = [
    {
      icon: <Coins className="h-4 w-4" />,
      label: '成本',
      value: formatUsd(totals?.costUsd),
      caption: `${formatCredits(totalCredits)} credit · ${formatUsd(creditUsdRate)}/credit`,
    },
    {
      icon: <TrendingUp className="h-4 w-4" />,
      label: '应收',
      value: formatUsd(totals?.receivableUsd),
      caption: '客户应付金额（口径见明细）',
    },
    {
      icon: <Coins className="h-4 w-4" />,
      label: '毛利',
      value: formatUsd(totals?.marginUsd),
      valueClassName:
        totals?.marginUsd == null ? '' : marginPositive ? 'text-emerald-600' : 'text-destructive',
      caption: '毛利 = 应收 − 成本',
    },
    {
      icon: <Percent className="h-4 w-4" />,
      label: '毛利率',
      value: totals?.marginRate != null ? `${totals.marginRate.toFixed(1)}%` : '—',
      caption: '毛利 ÷ 应收',
    },
  ]
  return (
    <div className="mb-6 grid grid-cols-1 gap-3 sm:grid-cols-2 lg:grid-cols-4">
      {cards.map((c) => (
        <SummaryCard key={c.label} {...c} />
      ))}
    </div>
  )
}

function SummaryCard({
  icon,
  label,
  value,
  caption,
  valueClassName,
}: {
  icon: React.ReactNode
  label: string
  value: string
  caption: string
  valueClassName?: string
}) {
  return (
    <Card>
      <CardContent className="p-4 sm:p-5">
        <div className="flex items-center gap-2 text-muted-foreground">
          {icon}
          <span className="text-[13px] font-medium text-foreground">{label}</span>
        </div>
        <div
          className={cn(
            'mt-3 text-2xl font-semibold tracking-tight tabular-nums sm:text-3xl',
            valueClassName,
          )}
        >
          {value}
        </div>
        <div className="mt-1 text-[11px] text-muted-foreground">{caption}</div>
      </CardContent>
    </Card>
  )
}

function UnpricedBanner({ items }: { items: UnpricedKey[] }) {
  return (
    <Card className="mb-6 border-amber-500/40 bg-amber-500/5">
      <CardContent className="p-4 sm:p-5">
        <div className="mb-2 flex items-center gap-2 text-amber-600">
          <AlertTriangle className="h-4 w-4 shrink-0" />
          <h2 className="text-sm font-semibold">
            以下 Key 有消耗但算不出应收，月结前请处理
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

function EstimatedBadge() {
  return (
    <Tooltip>
      <TooltipTrigger asChild>
        <Badge variant="warning" className="cursor-help">
          估算
        </Badge>
      </TooltipTrigger>
      <TooltipContent>{ESTIMATED_HINT}</TooltipContent>
    </Tooltip>
  )
}

function EstimatedMark() {
  return (
    <Tooltip>
      <TooltipTrigger asChild>
        <sup className="ml-0.5 cursor-help font-semibold text-amber-600">*</sup>
      </TooltipTrigger>
      <TooltipContent>{ESTIMATED_HINT}</TooltipContent>
    </Tooltip>
  )
}

function KeyTable({
  keys,
  totals,
  rowTotals,
  isLoading,
}: {
  keys: BillingKeyRow[]
  totals?: BillingTotals
  rowTotals: { calls: number; totalTokens: number; credits: number }
  isLoading: boolean
}) {
  const marginTotalClass =
    totals?.marginUsd == null ? '' : totals.marginUsd >= 0 ? 'text-emerald-600' : 'text-destructive'

  return (
    <Card>
      <CardContent className="p-4 sm:p-5">
        <div className="mb-3">
          <h2 className="text-base font-semibold tracking-tight">按客户端 Key 明细</h2>
          <p className="text-[12px] text-muted-foreground">
            成本按 credit 折算，可信 · 应收按单价计价可信，按官方价折扣计价为估算值（标 *）
          </p>
        </div>

        {isLoading ? (
          <div className="flex h-24 items-center justify-center text-[13px] text-muted-foreground">
            加载中…
          </div>
        ) : keys.length === 0 ? (
          <div className="flex h-24 items-center justify-center text-[13px] text-muted-foreground">
            本月无消耗数据
          </div>
        ) : (
          <div className="overflow-x-auto text-sm">
            <table className="w-full min-w-[1040px]">
              <thead className="text-muted-foreground">
                <tr>
                  <th className="pb-2.5 text-left font-medium">客户端 Key</th>
                  <th className="pb-2.5 pl-3 text-right font-medium">调用</th>
                  <th className="pb-2.5 pl-3 text-right font-medium">总 Token</th>
                  <th className="pb-2.5 pl-3 text-right font-medium">Credit</th>
                  <th className="pb-2.5 pl-3 text-right font-medium">成本$</th>
                  <th className="pb-2.5 pl-3 text-right font-medium">官方$</th>
                  <th className="pb-2.5 pl-3 text-left font-medium">计价方式</th>
                  <th className="pb-2.5 pl-3 text-right font-medium">应收$</th>
                  <th className="pb-2.5 pl-3 text-right font-medium">毛利$</th>
                  <th className="pb-2.5 pl-3 text-right font-medium">毛利率</th>
                </tr>
              </thead>
              <tbody>
                {keys.map((k) => (
                  <KeyRow key={k.keyId} k={k} />
                ))}
                <tr className="border-t-2 border-border/70 font-medium">
                  <td className="py-2.5 pr-4">合计</td>
                  <td className="py-2.5 pl-3 text-right tabular-nums">{formatNumber(rowTotals.calls)}</td>
                  <td className="py-2.5 pl-3 text-right tabular-nums">
                    {formatNumber(rowTotals.totalTokens)}
                  </td>
                  <td className="py-2.5 pl-3 text-right tabular-nums">{formatCredits(rowTotals.credits)}</td>
                  <td className="py-2.5 pl-3 text-right tabular-nums">{formatUsd(totals?.costUsd)}</td>
                  <td className="py-2.5 pl-3 text-right tabular-nums">{formatUsd(totals?.officialUsd)}</td>
                  <td className="py-2.5 pl-3 text-muted-foreground">—</td>
                  <td className="py-2.5 pl-3 text-right tabular-nums">{formatUsd(totals?.receivableUsd)}</td>
                  <td className={`py-2.5 pl-3 text-right tabular-nums ${marginTotalClass}`}>
                    {formatUsd(totals?.marginUsd)}
                  </td>
                  <td className="py-2.5 pl-3 text-right tabular-nums">
                    {totals?.marginRate != null ? `${totals.marginRate.toFixed(1)}%` : '—'}
                  </td>
                </tr>
              </tbody>
            </table>
          </div>
        )}
      </CardContent>
    </Card>
  )
}

function PricingModeCell({ k }: { k: BillingKeyRow }) {
  if (k.receivableBasis === 'perCredit') {
    return <span className="text-[12px]">单价 {formatUsd(k.pricePerCredit)}/credit</span>
  }
  if (k.receivableBasis === 'discount') {
    return (
      <span className="inline-flex items-center gap-1.5 text-[12px]">
        官方价 × {formatDiscount(k.billingDiscount)}
        <EstimatedBadge />
      </span>
    )
  }
  return <span className="text-[12px] text-muted-foreground">未定价</span>
}

function KeyRow({ k }: { k: BillingKeyRow }) {
  const estimated = k.receivableBasis === 'discount'
  const totalTokens = k.inputTokens + k.outputTokens + k.cacheCreationTokens + k.cacheReadTokens
  const marginRate = rowMarginRate(k)
  const marginClass = k.marginUsd == null ? '' : k.marginUsd >= 0 ? 'text-emerald-600' : 'text-destructive'

  return (
    <tr className="border-t border-border/40">
      <td className="max-w-[220px] truncate py-2.5 pr-4 font-medium">{k.name ?? `#${k.keyId}`}</td>
      <td className="py-2.5 pl-3 text-right tabular-nums">{formatNumber(k.calls)}</td>
      <td className="py-2.5 pl-3 text-right tabular-nums">{formatNumber(totalTokens)}</td>
      <td className="py-2.5 pl-3 text-right tabular-nums">{formatCredits(k.credits)}</td>
      <td className="py-2.5 pl-3 text-right tabular-nums">{formatUsd(k.costUsd)}</td>
      <td className="py-2.5 pl-3 text-right tabular-nums">{formatUsd(k.officialUsd)}</td>
      <td className="py-2.5 pl-3">
        <PricingModeCell k={k} />
      </td>
      <td className="py-2.5 pl-3 text-right tabular-nums">
        {formatUsd(k.receivableUsd)}
        {estimated && <EstimatedMark />}
      </td>
      <td className={`py-2.5 pl-3 text-right tabular-nums ${marginClass}`}>
        {formatUsd(k.marginUsd)}
        {estimated && <EstimatedMark />}
      </td>
      <td className="py-2.5 pl-3 text-right tabular-nums">
        {marginRate != null ? `${marginRate.toFixed(1)}%` : '—'}
      </td>
    </tr>
  )
}
