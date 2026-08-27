import { useMemo, useState } from 'react'
import { ChevronDown, Info, Receipt } from 'lucide-react'
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
} from '@/components/ui/dialog'
import { Button } from '@/components/ui/button'
import { Tooltip, TooltipContent, TooltipProvider, TooltipTrigger } from '@/components/ui/tooltip'
import { useBilling } from '@/hooks/use-stats'
import { cn, formatCredits, formatUsd } from '@/lib/utils'
import { PricingAdvicePanel } from '@/components/pricing-advice-panel'
import { MonthPicker, currentMonthValue } from '@/components/month-picker'
import { WarningBanner } from '@/components/warning-banner'
import type { BillingTotals, UnpricedKey, ZeroCreditKey } from '@/types/api'

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

const UNPRICED_COLLAPSE_THRESHOLD = 5

/**
 * 未定价 Key 列表：按成本降序排列，默认只展示前 5 个，其余折叠在「展开」按钮后面。
 * 单个账户可能有十几个只烧了几分钱的条目，全展开会把警示条撑成整页——折叠仅隐藏
 * 渲染，不从 items 里删除条目，点击展开按钮可看到完整列表。
 */
function UnpricedKeysList({ items }: { items: UnpricedKey[] }) {
  const [expanded, setExpanded] = useState(false)
  const sorted = useMemo(() => [...items].sort((a, b) => b.costUsd - a.costUsd), [items])
  const visible = expanded ? sorted : sorted.slice(0, UNPRICED_COLLAPSE_THRESHOLD)
  const hiddenCount = sorted.length - visible.length

  return (
    <>
      <ul className="space-y-1.5">
        {visible.map((it) => (
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
      {hiddenCount > 0 && (
        <Button
          type="button"
          size="sm"
          variant="ghost"
          className="mt-2 h-7 gap-1 px-2 text-[12px] text-muted-foreground"
          onClick={() => setExpanded(true)}
        >
          <ChevronDown className="h-3.5 w-3.5" />
          展开其余 {hiddenCount} 个
        </Button>
      )}
    </>
  )
}

interface MonthlySettlementDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
}

/**
 * 月度总账弹窗：凭据管理页的成本侧汇总（我方向上游付了多少、能收回多少、赚了多少）。
 *
 * 原先以卡片形式常驻凭据管理 Tab 顶部，连同警示条会把整页撑满、挤掉凭据列表；
 * 现在挪进设置齿轮菜单，按需弹出。逐 Key 的常规账单明细（调用/Credit/成本/应收/毛利）
 * 不在此展示，那属于「客户端 Key」页的客户视角；这里唯一的逐 Key 表格是下方的
 * 「定价建议」——它需要联动同一个 `month`，单独开一个月份选择器只会重演本文件要修的
 * 联动缺失问题。
 *
 * 弹窗下方是一叠按严重度排列的警示条：zeroCreditKeys（红，上游计费可能已变更，
 * 页面会看起来完全正常）→ malformedLines（金额未知）→ missingDays（当天没日志）→
 * unpriced（有消耗但算不出应收）。这些信号只在后端 JSON 里出现是没用的——必须露出来。
 * 再往下是「定价建议」面板：目标毛利率 + raiseOnly 开关驱动 `usePricingAdvice`，
 * 应用建议时复用账单页同款的 `useUpdateClientKey`（PUT billingDiscount），成功后额外
 * 失效 `['stats','pricing-advice']`——那是本面板独有的 query key，不在 useUpdateClientKey
 * 默认失效的范围内。
 */
export function MonthlySettlementDialog({ open, onOpenChange }: MonthlySettlementDialogProps) {
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
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-5xl max-h-[85vh] overflow-y-auto">
        <TooltipProvider delayDuration={150}>
          <DialogHeader>
            <DialogTitle className="flex items-center gap-2">
              <Receipt className="h-4 w-4" />
              月度总账
            </DialogTitle>
            <DialogDescription>
              我方对上游的月度成本与应收，成本口径可信，应收口径见「客户端 Key」页明细
            </DialogDescription>
          </DialogHeader>

          <div className="flex justify-end">
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
                <div className="flex items-center gap-1.5 border-t border-border/40 pt-3 text-[12px] text-muted-foreground">
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

          {hasWarnings && (
            <div className="space-y-3">
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
                  <UnpricedKeysList items={unpriced} />
                </WarningBanner>
              )}
            </div>
          )}

          {!isLoading && data && (
            <PricingAdvicePanel month={month} billingKeys={data.keys} />
          )}
        </TooltipProvider>
      </DialogContent>
    </Dialog>
  )
}
