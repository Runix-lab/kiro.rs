import { useMemo, useState } from 'react'
import { Info, Microscope, PieChart } from 'lucide-react'
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
} from '@/components/ui/dialog'
import { Tooltip, TooltipContent, TooltipProvider, TooltipTrigger } from '@/components/ui/tooltip'
import { useModelCostAnalysis } from '@/hooks/use-stats'
import { cn, formatDiscount, formatNumber, formatUsd } from '@/lib/utils'
import { MonthPicker, currentMonthValue } from '@/components/month-picker'
import { WarningBanner } from '@/components/warning-banner'
import { TD_LABEL, TD_NUM, TH_LABEL, TH_NUM, TH_TEXT } from '@/lib/table-styles'
import type { ModelCostAnalysisRow } from '@/types/api'

/** 上游缓存折扣的判定阈值：≥ 0.9 视为"几乎没有折扣"，与后端 advice 文案的口径一致 */
const CACHE_DISCOUNT_THRESHOLD = 0.9

/** 超过这么多天没有重新实测，就该提醒运营重测而不是一直信旧数字 */
const STALE_MEASUREMENT_DAYS = 60

function daysSince(iso: string): number | null {
  const t = new Date(iso).getTime()
  if (Number.isNaN(t)) return null
  return (Date.now() - t) / (1000 * 60 * 60 * 24)
}

/** 表头小标签：给"估算"/"实测"字段贴一个不刺眼但一眼能看到的口径提示 */
function ProvenanceBadge({ kind }: { kind: 'estimated' | 'measured' }) {
  return (
    <span
      className={cn(
        'ml-1 rounded px-1 py-0.5 text-[9px] font-normal leading-none',
        kind === 'estimated'
          ? 'bg-amber-500/15 text-amber-600 dark:text-amber-400'
          : 'bg-emerald-500/15 text-emerald-600 dark:text-emerald-400',
      )}
    >
      {kind === 'estimated' ? '估算' : '实测'}
    </span>
  )
}

/** 带 tooltip 的表头：数值列右对齐，图标放在文字后面不挤占列宽视觉重心 */
function HeaderWithTooltip({
  label,
  sublabel,
  badge,
  tooltip,
}: {
  label: string
  sublabel?: string
  badge?: 'estimated' | 'measured'
  tooltip: string
}) {
  return (
    <span className="inline-flex flex-col items-end gap-0.5">
      <span className="inline-flex items-center gap-1">
        {label}
        {badge && <ProvenanceBadge kind={badge} />}
        <Tooltip>
          <TooltipTrigger asChild>
            <Info className="h-3 w-3 cursor-help text-muted-foreground" />
          </TooltipTrigger>
          <TooltipContent>{tooltip}</TooltipContent>
        </Tooltip>
      </span>
      {sublabel && <span className="text-[10px] font-normal text-muted-foreground">{sublabel}</span>}
    </span>
  )
}

function CallsCell({ row }: { row: ModelCostAnalysisRow }) {
  return (
    <div>
      <div>{formatNumber(row.calls)}</div>
      {row.errors > 0 && (
        <div className="text-[11px] font-normal text-destructive">错误 {formatNumber(row.errors)}</div>
      )}
    </div>
  )
}

function CacheDiscountCell({ row }: { row: ModelCostAnalysisRow }) {
  if (row.upstreamCacheReadRatio == null) {
    return <span className="text-[12px] font-normal text-muted-foreground">未实测</span>
  }
  const hasDiscount = row.upstreamCacheReadRatio < CACHE_DISCOUNT_THRESHOLD
  return (
    <span className={cn('font-semibold', hasDiscount ? 'text-emerald-600 dark:text-emerald-400' : 'text-destructive')}>
      {formatDiscount(row.upstreamCacheReadRatio)}
    </span>
  )
}

/** 表体一行：数值列全部 `tabular-nums` + 不换行，建议列例外——它需要完整显示中文长文案 */
function ModelRow({ row }: { row: ModelCostAnalysisRow }) {
  return (
    <tr className="border-t border-border/40 align-top">
      <td className={TD_LABEL} title={row.model}>
        {row.model}
      </td>
      <td className={TD_NUM}>
        <CallsCell row={row} />
      </td>
      <td className={TD_NUM}>{formatUsd(row.costUsd)}</td>
      <td className={TD_NUM}>{row.costSharePct.toFixed(1)}%</td>
      <td className={TD_NUM}>{formatUsd(row.officialUsd)}</td>
      <td className={cn(TD_NUM, 'font-semibold')}>{formatDiscount(row.effectiveDiscount)}</td>
      <td className={TD_NUM}>
        {row.cacheHitRate != null ? `${(row.cacheHitRate * 100).toFixed(1)}%` : '—'}
      </td>
      <td className={TD_NUM}>
        <CacheDiscountCell row={row} />
      </td>
      <td className={TD_NUM}>{row.tokensPerCredit != null ? formatNumber(row.tokensPerCredit) : '—'}</td>
      <td className="min-w-[240px] max-w-[380px] whitespace-normal break-words py-2.5 pl-3 pr-2 text-left text-[12px] leading-relaxed">
        {row.advice}
      </td>
    </tr>
  )
}

function CostAnalysisTable({ models }: { models: ModelCostAnalysisRow[] }) {
  const sorted = useMemo(() => [...models].sort((a, b) => b.costUsd - a.costUsd), [models])

  if (sorted.length === 0) {
    return (
      <div className="flex h-24 items-center justify-center text-[13px] text-muted-foreground">
        本期无数据
      </div>
    )
  }

  return (
    <div className="overflow-x-auto text-sm">
      <table className="w-full min-w-[1240px]">
        <thead className="text-muted-foreground">
          <tr>
            <th className={TH_LABEL}>模型</th>
            <th className={TH_NUM}>调用</th>
            <th className={TH_NUM}>成本$</th>
            <th className={TH_NUM}>成本占比</th>
            <th className={TH_NUM}>
              <HeaderWithTooltip
                label="官方牌价$"
                badge="estimated"
                tooltip="上游未逐条下发 token 明细时，官方牌价由本地按 token 拆分估算补齐，不是上游实报值。"
              />
            </th>
            <th className={TH_NUM}>
              <HeaderWithTooltip
                label="实付折扣"
                sublabel="我方买入"
                badge="estimated"
                tooltip="我方买入折扣 = 实付成本 ÷ 官方牌价。分子（实付）来自上游真实计费，分母（官方牌价）是估算值，因此该折扣本身也是估算。这是我们向上游买入的折扣，与我们卖给客户的折扣（对客折扣）是两回事。"
              />
            </th>
            <th className={TH_NUM}>
              <HeaderWithTooltip
                label="缓存命中率"
                badge="estimated"
                tooltip="缓存读 ÷ (输入 + 缓存写 + 缓存读)。上游不下发逐条 token 明细时，这个拆分由本地估算补齐，仅供参考。"
              />
            </th>
            <th className={TH_NUM}>
              <HeaderWithTooltip
                label="上游缓存折扣"
                badge="measured"
                tooltip="直接来自上游返回的缓存读 credits ÷ 缓存写 credits，实测得出，不是本地估算。这是本页最值得据此做决策的数字：绿色=有折扣，红色=几乎没有折扣。"
              />
            </th>
            <th className={TH_NUM}>每credit token</th>
            <th className={TH_TEXT}>建议</th>
          </tr>
        </thead>
        <tbody>
          {sorted.map((row) => (
            <ModelRow key={row.model} row={row} />
          ))}
        </tbody>
      </table>
    </div>
  )
}

function MeasurementNote({ measuredAt, method, finding, caveat }: {
  measuredAt: string
  method: string
  finding: string
  caveat: string
}) {
  const age = daysSince(measuredAt)
  const stale = age != null && age > STALE_MEASUREMENT_DAYS

  return (
    <div className="space-y-3">
      <div className="space-y-2 rounded-md border border-border/60 bg-secondary/10 p-3 sm:p-4">
        <h3 className="flex items-center gap-2 text-[13px] font-semibold">
          <Microscope className="h-4 w-4 text-muted-foreground" />
          计量说明 · 上游缓存折扣实测
        </h3>
        <dl className="grid grid-cols-1 gap-x-6 gap-y-1 text-[12px] sm:grid-cols-2">
          <div className="flex gap-1.5">
            <dt className="shrink-0 text-muted-foreground">实测时间</dt>
            <dd className="tabular-nums">{measuredAt}</dd>
          </div>
          <div className="flex gap-1.5">
            <dt className="shrink-0 text-muted-foreground">实测方法</dt>
            <dd>{method}</dd>
          </div>
        </dl>
        <p className="text-[12px] leading-relaxed">
          <span className="font-medium">结论：</span>
          {finding}
        </p>
        {/* caveat 必须常驻可见，不能收进 tooltip——上一次只信单次实测结果就误判过一回,
            每个数字后来都被重新核实过，这句话就是那次教训的存档。 */}
        <p className="text-[11px] leading-relaxed text-muted-foreground">{caveat}</p>
      </div>
      {stale && (
        <WarningBanner tone="amber" title="实测数据可能已过期，建议重测">
          <p className="text-[11px] leading-relaxed text-muted-foreground">
            上一次实测距今已超过 {STALE_MEASUREMENT_DAYS} 天（{measuredAt}）。上游缓存计费策略可能已变化，
            表格中的「上游缓存折扣」列仍按旧实测值展示，请安排重新实测后再据此做定价决策。
          </p>
        </WarningBanner>
      )}
    </div>
  )
}

interface ModelCostAnalysisDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
}

/**
 * 模型成本分析弹窗：按模型拆开看"钱花在哪、买得贵不贵、缓存有没有省到"。
 *
 * 与「月度总账」是同一层级但不同视角——总账回答"这个月赚了多少"，这里回答
 * "哪个模型该少用 / 哪个模型缓存没吃到折扣"。两者都挂在设置齿轮的「账务」分组下，
 * 各自持有独立的月份状态（互不联动，因为不会同时打开）。
 *
 * 页面的核心矛盾是"两种数字的可信度不一样"：`credits`（进而 `costUsd`）与
 * `upstreamCacheReadRatio` 是上游真值/实测；`cacheHitRate`、`officialUsd`
 * 以及依赖它的 `effectiveDiscount` 建立在上游不下发 token 明细时的本地估算之上。
 * 这个区分必须在读到数字的地方就标出来（表头 tooltip + 估算/实测徽标），
 * 不能只写在页脚——2026-08 那次 opus-5 折扣从 1.3 折被误算成 5.4 折的教训就是
 * 分子分母可信度不同却被当作同一种数字看待。
 */
export function ModelCostAnalysisDialog({ open, onOpenChange }: ModelCostAnalysisDialogProps) {
  const [month, setMonth] = useState(currentMonthValue)
  const { data, isLoading } = useModelCostAnalysis(month)

  const missingDays = data?.missingDays ?? []
  const malformedLines = data?.malformedLines ?? 0
  const hasWarnings = missingDays.length > 0 || malformedLines > 0

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-6xl max-h-[85vh] overflow-y-auto">
        <TooltipProvider delayDuration={150}>
          <DialogHeader>
            <DialogTitle className="flex items-center gap-2">
              <PieChart className="h-4 w-4" />
              模型成本分析
            </DialogTitle>
            <DialogDescription>
              按模型拆分成本、我方买入折扣与缓存表现，用来判断每个模型到底买得贵不贵、缓存有没有真省钱
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
            data && (
              <div className="flex flex-col gap-4 rounded-md border border-border/60 p-4 sm:flex-row sm:items-center">
                <div className="shrink-0">
                  <div className="text-[11px] font-medium text-muted-foreground sm:text-[13px]">
                    本期总成本（{data.month}）
                  </div>
                  <div className="mt-1.5 text-2xl font-semibold tracking-tight tabular-nums">
                    {formatUsd(data.totalCostUsd)}
                  </div>
                </div>
                <div className="border-t border-border/40 pt-3 text-[13px] leading-relaxed sm:border-l sm:border-t-0 sm:pl-4 sm:pt-0">
                  <span className="font-medium">{data.cacheMeasurement.finding}</span>
                </div>
              </div>
            )
          )}

          {hasWarnings && (
            <div className="space-y-3">
              {malformedLines > 0 && (
                <WarningBanner tone="amber" title={`本期有 ${malformedLines} 行用量日志无法解析`}>
                  <p className="text-[11px] leading-relaxed text-muted-foreground">
                    这些请求的金额未知，本页的成本与折扣计算不含它们。
                  </p>
                </WarningBanner>
              )}
              {missingDays.length > 0 && (
                <WarningBanner tone="amber" title={`本期有 ${missingDays.length} 天没有用量日志`}>
                  <p className="text-[11px] leading-relaxed text-muted-foreground">
                    这些日期没有日志文件，不等于"当天没有流量"——可能是日志缺失。本期成本可能因此偏低，
                    据此做定价或选型决策前请先确认。
                  </p>
                  <p className="mt-1.5 font-mono text-[11px] text-muted-foreground">
                    {missingDays.join('、')}
                  </p>
                </WarningBanner>
              )}
            </div>
          )}

          {!isLoading && data && <CostAnalysisTable models={data.models} />}

          {!isLoading && data && (
            <MeasurementNote
              measuredAt={data.cacheMeasurement.measuredAt}
              method={data.cacheMeasurement.method}
              finding={data.cacheMeasurement.finding}
              caveat={data.cacheMeasurement.caveat}
            />
          )}

          {data?.note && <p className="text-[11px] text-muted-foreground">{data.note}</p>}
        </TooltipProvider>
      </DialogContent>
    </Dialog>
  )
}
