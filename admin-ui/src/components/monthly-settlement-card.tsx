import { useMemo, useState } from 'react'
import { AlertTriangle, Calendar, Info } from 'lucide-react'
import { Card, CardContent } from '@/components/ui/card'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Tooltip, TooltipContent, TooltipProvider, TooltipTrigger } from '@/components/ui/tooltip'
import { useBilling } from '@/hooks/use-stats'
import { cn, formatCredits, formatUsd } from '@/lib/utils'
import type { BillingTotals, UnpricedKey, ZeroCreditKey } from '@/types/api'

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

/**
 * 统一的警示条外壳：月结前需要人工处理的信号都走这一个组件，读起来是一叠按严重度
 * 排列的警示，而不是三四个互不相关的盒子。red = 最高危（数据看似正常实则失真），
 * amber = 已知的、有明确处理路径的缺口。
 */
function WarningBanner({
  tone,
  title,
  children,
}: {
  tone: 'red' | 'amber'
  title: string
  children: React.ReactNode
}) {
  return (
    <div
      className={cn(
        'rounded-md border p-3 sm:p-4',
        tone === 'red' ? 'border-destructive/40 bg-destructive/5' : 'border-amber-500/40 bg-amber-500/10',
      )}
    >
      <div
        className={cn(
          'mb-1.5 flex items-center gap-2',
          tone === 'red' ? 'text-destructive' : 'text-amber-600 dark:text-amber-400',
        )}
      >
        <AlertTriangle className="h-4 w-4 shrink-0" />
        <h3 className="text-[13px] font-semibold">{title}</h3>
      </div>
      {children}
    </div>
  )
}

/**
 * 月度总账卡：凭据管理页的成本侧汇总（我方向上游付了多少、能收回多少、赚了多少）。
 *
 * 与 QuotaBurndownCard 紧邻——一个看额度烧多快，一个看这些消耗值多少钱，
 * 成本语境放在一起。逐 Key 明细不在此展示，那属于「客户端 Key」页的客户视角。
 *
 * 卡片下方是一叠按严重度排列的警示条：zeroCreditKeys（红，上游计费可能已变更，
 * 页面会看起来完全正常）→ malformedLines（金额未知）→ missingDays（当天没日志）→
 * unpriced（有消耗但算不出应收）。这些信号只在后端 JSON 里出现是没用的——必须露出来。
 */
export function MonthlySettlementCard() {
  const [month, setMonth] = useState(currentMonthValue)
  const { data, isLoading } = useBilling(month)
  const totals: BillingTotals | undefined = data?.totals
  const unpriced: UnpricedKey[] = data?.unpricedKeys ?? []
  const zeroCreditKeys: ZeroCreditKey[] = data?.zeroCreditKeys ?? []
  const malformedLines = data?.malformedLines ?? 0
  const missingDays = data?.missingDays ?? []
  const totalCredits = useMemo(
    () => (data?.keys ?? []).reduce((s, k) => s + k.credits, 0),
    [data],
  )
  const totalErrorCredits = useMemo(
    () => (data?.keys ?? []).reduce((s, k) => s + (k.errorCredits ?? 0), 0),
    [data],
  )

  const marginPositive = (totals?.marginUsd ?? 0) >= 0
  const marginClassName =
    totals?.marginUsd == null ? '' : marginPositive ? 'text-emerald-600' : 'text-destructive'

  const hasWarnings =
    zeroCreditKeys.length > 0 || malformedLines > 0 || missingDays.length > 0 || unpriced.length > 0

  return (
    <TooltipProvider delayDuration={150}>
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
            <>
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
              {totalErrorCredits > 0 && (
                <div className="mt-4 flex items-center gap-1.5 border-t border-border/40 pt-3 text-[12px] text-muted-foreground">
                  <span>本期失败请求成本 {formatUsd(totalErrorCredits)}（未计入应收）</span>
                  <Tooltip>
                    <TooltipTrigger asChild>
                      <Info className="h-3.5 w-3.5 shrink-0 cursor-help" />
                    </TooltipTrigger>
                    <TooltipContent>
                      这些请求上游已计费但返回失败，我方承担成本，不向客户收取。
                    </TooltipContent>
                  </Tooltip>
                </div>
              )}
            </>
          )}
        </CardContent>
      </Card>

      {hasWarnings && (
        <div className="mb-5 space-y-3 sm:mb-6">
          {zeroCreditKeys.length > 0 && (
            <WarningBanner
              tone="red"
              title="以下 Key 有成功调用但 Credit 为 0 —— 上游计费可能已变更"
            >
              <p className="mb-2 text-[11px] leading-relaxed text-muted-foreground">
                成本与应收会同时显示 $0，页面看起来正常。月结前必须确认上游计费事件是否改了协议。
              </p>
              <ul className="space-y-1">
                {zeroCreditKeys.map((k) => (
                  <li
                    key={k.keyId}
                    className="flex flex-wrap items-baseline gap-x-2 text-[12px] text-destructive"
                  >
                    <span className="font-medium">{k.name ?? `#${k.keyId}`}</span>
                    <span className="tabular-nums">{k.calls} 次调用</span>
                  </li>
                ))}
              </ul>
            </WarningBanner>
          )}

          {malformedLines > 0 && (
            <WarningBanner tone="amber" title={`本期有 ${malformedLines} 行用量日志无法解析`}>
              <p className="text-[11px] leading-relaxed text-muted-foreground">
                这些请求的金额未知，账目可能不完整。
              </p>
            </WarningBanner>
          )}

          {missingDays.length > 0 && (
            <WarningBanner tone="amber" title={`本期有 ${missingDays.length} 天没有用量日志`}>
              <p className="text-[11px] leading-relaxed text-muted-foreground">
                这些日期没有日志文件，账目里按零消费计算。若当天其实有流量，本期成本与应收都会偏低——
                月结前请确认是「当天确实没跑」还是「日志缺失」。
              </p>
              <p className="mt-1.5 font-mono text-[11px] text-muted-foreground">
                {missingDays.join('、')}
              </p>
            </WarningBanner>
          )}

          {unpriced.length > 0 && (
            <WarningBanner tone="amber" title="以下 Key 有消耗但收不出钱，月结前请处理">
              <ul className="space-y-1.5">
                {unpriced.map((it) => (
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
            </WarningBanner>
          )}
        </div>
      )}
    </TooltipProvider>
  )
}
