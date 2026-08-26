import { useMemo, useState } from 'react'
import { useQueryClient } from '@tanstack/react-query'
import { toast } from 'sonner'
import { Info, Sparkles } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Switch } from '@/components/ui/switch'
import { Tooltip, TooltipContent, TooltipTrigger } from '@/components/ui/tooltip'
import { useConfirm } from '@/components/ui/confirm-dialog'
import { usePricingAdvice } from '@/hooks/use-stats'
import { useUpdateClientKey } from '@/hooks/use-client-keys'
import { cn, extractErrorMessage, formatDiscount, formatUsd } from '@/lib/utils'
import { TD_LABEL, TD_NUM, TH_LABEL, TH_NUM } from '@/lib/table-styles'
import type { BillingKeyRow, PricingAdviceKeyRow } from '@/types/api'

const DEFAULT_TARGET_MARGIN_PCT = 50
const MIN_TARGET_MARGIN_PCT = 0
const MAX_TARGET_MARGIN_PCT = 95

function clamp(v: number, min: number, max: number): number {
  return Math.min(max, Math.max(min, v))
}

/** 应收变化：正负号显式标出，颜色跟着符号走——光看颜色不够，$0 附近容易看错方向。 */
function formatSignedUsd(v: number | null | undefined): string {
  if (v == null || Number.isNaN(v)) return '—'
  if (v === 0) return formatUsd(0)
  const s = formatUsd(Math.abs(v))
  return v > 0 ? `+${s}` : `-${s}`
}

/**
 * 毛利率格式化。
 *
 * 后端已把字段名改成 `...RatePct`（0-100 的百分数，与 `BillingTotals.marginRate`
 * 同口径），所以这里**不再猜单位**。此前用「≤1 就当小数」的启发式，
 * 在毛利率正好 100% 时会把它显示成 1% —— 一个会直接误导定价决策的读数。
 * 单位写进字段名比任何注释都可靠。
 */
function formatMarginPct(v: number | null | undefined): string {
  if (v == null || Number.isNaN(v)) return '—'
  return `${v.toFixed(1)}%`
}

function cacheReadShare(row: BillingKeyRow | undefined): number | null {
  if (!row) return null
  const denom = row.inputTokens + row.cacheCreationTokens + row.cacheReadTokens
  if (denom <= 0) return null
  return row.cacheReadTokens / denom
}

function BreakevenHeaderLabel() {
  return (
    <span className="inline-flex items-center justify-end gap-1">
      保本线
      <Tooltip>
        <TooltipTrigger asChild>
          <Info className="h-3 w-3 cursor-help" />
        </TooltipTrigger>
        <TooltipContent>
          保本线 = 成本 ÷ 官方牌价，由该客户的模型组合决定，不是谈出来的。折扣低于它，无论卖多少都在亏。
        </TooltipContent>
      </Tooltip>
    </span>
  )
}

function CacheReadHeaderLabel() {
  return (
    <span className="inline-flex items-center justify-end gap-1">
      缓存读占比
      <Tooltip>
        <TooltipTrigger asChild>
          <Info className="h-3 w-3 cursor-help" />
        </TooltipTrigger>
        <TooltipContent>
          上游不逐条下发 token 明细时，这个拆分由本地估算补齐——占比仅供参考，不是上游实报值。
        </TooltipContent>
      </Tooltip>
    </span>
  )
}

/** 单行「应用」按钮：独立持有 mutation 状态，互不干扰的 pending 展示与确认弹窗。 */
function ApplyAdviceButton({ row, month }: { row: PricingAdviceKeyRow; month: string }) {
  const updateKey = useUpdateClientKey()
  const confirm = useConfirm()
  const qc = useQueryClient()

  const handleApply = async () => {
    if (row.recommendedDiscount == null) return
    const name = row.name ?? `#${row.keyId}`
    const ok = await confirm({
      title: '确认调整折扣',
      description: `客户「${name}」：折扣 ${formatDiscount(row.currentDiscount)} → ${formatDiscount(row.recommendedDiscount)}，预计应收变化 ${formatSignedUsd(row.receivableDeltaUsd)}（${month}）。确认应用？`,
      confirmText: '确认应用',
    })
    if (!ok) return
    try {
      await updateKey.mutateAsync({
        id: row.keyId,
        req: { billingDiscount: row.recommendedDiscount },
      })
      // useUpdateClientKey 已失效 client-keys 与 stats.billing；定价建议是本次新增的
      // 独立 query key，需要额外失效，否则「应用」后表格里的建议还停在调整前的旧值。
      qc.invalidateQueries({ queryKey: ['stats', 'pricing-advice'] })
      toast.success(`「${name}」折扣已更新为 ${formatDiscount(row.recommendedDiscount)}`)
    } catch (err) {
      toast.error('应用失败：' + extractErrorMessage(err))
    }
  }

  return (
    <Button
      size="sm"
      variant="outline"
      className="h-7 px-2.5 text-[12px]"
      disabled={updateKey.isPending || row.recommendedDiscount == null}
      onClick={handleApply}
    >
      {updateKey.isPending ? '应用中…' : '应用'}
    </Button>
  )
}

interface PricingAdvicePanelProps {
  /** 与「月度总账」弹窗共用同一个月份状态，不单独维护一份——这正是本页要修的联动问题 */
  month: string
  /** 已经在父组件按同一个 month 取过的账单明细，用于就地算出缓存读占比，避免重复请求 */
  billingKeys: BillingKeyRow[]
}

export function PricingAdvicePanel({ month, billingKeys }: PricingAdvicePanelProps) {
  const [targetMarginPct, setTargetMarginPct] = useState(DEFAULT_TARGET_MARGIN_PCT)
  const [raiseOnly, setRaiseOnly] = useState(true)

  const targetMarginRate = targetMarginPct / 100
  const { data, isLoading } = usePricingAdvice(month, targetMarginRate, raiseOnly)

  const billingByKeyId = useMemo(() => {
    const m = new Map<number, BillingKeyRow>()
    for (const row of billingKeys) m.set(row.keyId, row)
    return m
  }, [billingKeys])

  const sortedKeys = useMemo(
    () => [...(data?.keys ?? [])].sort((a, b) => b.costUsd - a.costUsd),
    [data],
  )

  const delta = data?.impact.deltaUsd
  const deltaClass =
    delta == null ? '' : delta > 0 ? 'text-emerald-600' : delta < 0 ? 'text-destructive' : ''

  return (
    <div className="mt-2 border-t border-border/60 pt-4">
      <div className="mb-3 flex items-center gap-2">
        <Sparkles className="h-4 w-4 text-muted-foreground" />
        <h3 className="text-[13px] font-semibold">定价建议（{month}）</h3>
      </div>

      <div className="mb-3 flex flex-wrap items-center gap-x-6 gap-y-2">
        <div className="flex items-center gap-2">
          <span className="text-[12px] text-muted-foreground">目标毛利率</span>
          <input
            type="range"
            min={MIN_TARGET_MARGIN_PCT}
            max={MAX_TARGET_MARGIN_PCT}
            step={1}
            value={targetMarginPct}
            onChange={(e) => setTargetMarginPct(Number(e.target.value))}
            className="h-1.5 w-[160px] cursor-pointer accent-emerald-600 dark:accent-emerald-500"
          />
          <Input
            type="number"
            min={MIN_TARGET_MARGIN_PCT}
            max={MAX_TARGET_MARGIN_PCT}
            value={targetMarginPct}
            onChange={(e) => {
              const v = Number(e.target.value)
              if (!Number.isNaN(v)) setTargetMarginPct(clamp(Math.round(v), MIN_TARGET_MARGIN_PCT, MAX_TARGET_MARGIN_PCT))
            }}
            className="h-7 w-16 px-2 text-center text-[12px]"
          />
          <span className="text-[12px] text-muted-foreground">%</span>
        </div>
        <div className="flex items-center gap-2">
          <Switch checked={raiseOnly} onCheckedChange={setRaiseOnly} />
          <span className="text-[12px] text-muted-foreground">只上调，不下调已达标的客户</span>
        </div>
      </div>

      {isLoading ? (
        <div className="flex h-16 items-center justify-center text-[13px] text-muted-foreground">
          计算中…
        </div>
      ) : !data || data.keys.length === 0 ? (
        <div className="flex h-16 items-center justify-center text-[13px] text-muted-foreground">
          本期无可定价的 Key
        </div>
      ) : (
        <>
          <div className={cn('mb-3 rounded-md border border-border/60 bg-secondary/20 px-3 py-2.5 text-[13px]')}>
            按建议全部调整后，月度应收变化{' '}
            <span className={cn('font-semibold tabular-nums', deltaClass)}>
              {formatSignedUsd(data.impact.deltaUsd)}
            </span>
            <span className="ml-2 text-[11px] text-muted-foreground tabular-nums">
              {formatUsd(data.impact.currentReceivableUsd)} → {formatUsd(data.impact.afterAdviceReceivableUsd)}
            </span>
          </div>

          <div className="overflow-x-auto text-sm">
            <table className="w-full min-w-[1080px]">
              <thead className="text-muted-foreground">
                <tr>
                  <th className={TH_LABEL}>名称</th>
                  <th className={TH_NUM}>成本$</th>
                  <th className={TH_NUM}>官方牌价$</th>
                  <th className={TH_NUM}><BreakevenHeaderLabel /></th>
                  <th className={TH_NUM}>当前折扣</th>
                  <th className={TH_NUM}>当前毛利率</th>
                  <th className={TH_NUM}>建议折扣</th>
                  <th className={TH_NUM}>应收变化</th>
                  <th className={TH_NUM}><CacheReadHeaderLabel /></th>
                  <th className={TH_NUM}>操作</th>
                </tr>
              </thead>
              <tbody>
                {sortedKeys.map((row) => {
                  const cacheShare = cacheReadShare(billingByKeyId.get(row.keyId))
                  const marginNegative = row.currentMarginRatePct != null && row.currentMarginRatePct < 0
                  return (
                    <tr
                      key={row.keyId}
                      className={cn(
                        'border-t border-border/40',
                        row.targetUnreachable && 'bg-destructive/5',
                      )}
                    >
                      <td className={TD_LABEL} title={row.name ?? `#${row.keyId}`}>
                        {row.name ?? `#${row.keyId}`}
                      </td>
                      <td className={TD_NUM}>{formatUsd(row.costUsd)}</td>
                      <td className={TD_NUM}>{formatUsd(row.officialUsd)}</td>
                      <td className={TD_NUM}>{formatDiscount(row.breakevenDiscount)}</td>
                      <td className={TD_NUM}>{formatDiscount(row.currentDiscount)}</td>
                      <td className={cn(TD_NUM, marginNegative && 'text-destructive')}>
                        {formatMarginPct(row.currentMarginRatePct)}
                      </td>
                      <td className={cn(TD_NUM, row.targetUnreachable && 'text-destructive')}>
                        {row.actionSuggested ? (
                          <div>
                            <div className="font-semibold">{formatDiscount(row.recommendedDiscount)}</div>
                            {row.targetUnreachable && (
                              <div className="mt-0.5 text-[11px] font-normal leading-snug">
                                该客户成本结构下最高只能做到 {formatMarginPct(row.maxAchievableMarginRatePct)}
                                （已按 1.0 折扣封顶）
                              </div>
                            )}
                          </div>
                        ) : (
                          <span className="text-[12px] font-normal text-muted-foreground">{row.verdict}</span>
                        )}
                      </td>
                      <td className={TD_NUM}>{formatSignedUsd(row.receivableDeltaUsd)}</td>
                      <td className={TD_NUM}>
                        {cacheShare != null ? `${(cacheShare * 100).toFixed(1)}%` : '—'}
                      </td>
                      <td className="py-2.5 pl-3 text-right">
                        {row.actionSuggested ? (
                          <ApplyAdviceButton row={row} month={month} />
                        ) : (
                          <span className="text-[11px] text-muted-foreground">—</span>
                        )}
                      </td>
                    </tr>
                  )
                })}
              </tbody>
            </table>
          </div>
          {data.note && (
            <p className="mt-2 text-[11px] text-muted-foreground">{data.note}</p>
          )}
        </>
      )}
    </div>
  )
}
