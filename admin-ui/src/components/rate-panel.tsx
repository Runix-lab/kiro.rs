import { useMemo } from 'react'
import { Card, CardContent } from '@/components/ui/card'
import { Activity, ArrowUpFromLine, Coins, Repeat } from 'lucide-react'
import { useRateBuckets, useRateRing } from '@/hooks/use-stats'
import { extractErrorMessage, formatNumber, formatRate } from '@/lib/utils'
import { RateChart, type RateChartResolution, type RateChartRow } from '@/components/charts/rate-chart'
import type { StatsRange, StatsTimeFilter, TimeSeriesPoint } from '@/types/api'

/**
 * 速率环最长覆盖 1440 分钟（24h），且只支持"以当前时刻为终点"的相对窗口——
 * 因此只有这四档预设能走环；7d/30d 预设与任意自定义日期区间一律走桶均值回退。
 */
const RING_RANGE_MINUTES: Partial<Record<StatsRange, number>> = {
  '1h': 60,
  '3h': 180,
  '6h': 360,
  '24h': 1440,
}

const RANGE_LABELS: Partial<Record<StatsRange, string>> = {
  '1h': '最近 1 小时',
  '3h': '最近 3 小时',
  '6h': '最近 6 小时',
  '24h': '最近 24 小时',
  '7d': '最近 7 天',
  '30d': '最近 30 天',
}

/** windowMinutes 低于请求值的这个比例，就提示"数据不够，不是真掉量"。 */
const PARTIAL_WINDOW_RATIO = 0.8

interface Props {
  /** 页面顶部的时间范围预设；点了自定义日期（未选预设按钮）时为 undefined。 */
  range?: StatsRange
  /** 环覆盖不到时的回退查询用这个（7d/30d 预设、以及自定义区间都走这条路）。 */
  timeFilter: StatsTimeFilter
}

function rangeLabelFor(range: StatsRange | undefined, timeFilter: StatsTimeFilter): string {
  if (range) return RANGE_LABELS[range] ?? range
  const { startDate, endDate } = timeFilter
  if (startDate && endDate) {
    return `${startDate.replace(/-/g, '/')} - ${endDate.replace(/-/g, '/')}`
  }
  return '自定义区间'
}

/**
 * 实时速率面板：跟随页面顶部的时间范围选择器。
 *
 * - 选 1h/3h/6h/24h：走速率环，逐分钟真值，15s 轮询。
 * - 选 7d/30d 或自定义日期区间：环覆盖不到（环只有 24h 容量，且只回答"以当前
 *   时刻为终点"的窗口，答不了"上周三那小时"），回退到 `/stats/timeseries` 的
 *   小时/天分桶，换算成"桶均值速率"，并在标题与坐标轴上明确标出分辨率变了——
 *   不能让人以为还是分钟级数据。
 *
 * 两种模式都不受入口 Key / 分组筛选影响（全局口径），这是刻意设计：速率环本身
 * 不支持按维度筛，桶回退为了跟环模式语义一致，也不加筛选。
 */
export function RatePanel({ range, timeFilter }: Props) {
  const ringMinutes = range ? RING_RANGE_MINUTES[range] : undefined
  const ringMode = ringMinutes != null
  const rangeLabel = rangeLabelFor(range, timeFilter)

  const ring = useRateRing(ringMinutes ?? 60, ringMode)
  const buckets = useRateBuckets(timeFilter, !ringMode)

  if (ringMode) {
    return <RingRatePanel minutes={ringMinutes} rangeLabel={rangeLabel} query={ring} />
  }
  return <BucketRatePanel timeFilter={timeFilter} rangeLabel={rangeLabel} query={buckets} />
}

function statusOf(error: unknown): number | undefined {
  return (error as { response?: { status?: number } } | null)?.response?.status
}

function humanizeMinutes(minutes: number): string {
  if (minutes % 60 === 0) return `${minutes / 60} 小时`
  return `${minutes} 分钟`
}

/** 环模式：分钟级真值，5 个成对指标 + 分钟级折线图。 */
function RingRatePanel({
  minutes,
  rangeLabel,
  query,
}: {
  minutes: number
  rangeLabel: string
  query: ReturnType<typeof useRateRing>
}) {
  const { data: snapshot, isLoading, error } = query
  const unavailable = statusOf(error) === 503
  const series = useMemo(() => snapshot?.series ?? [], [snapshot])
  const hasTraffic = useMemo(
    () => series.some((s) => s.ingressCalls > 0 || s.upstreamAttempts > 0),
    [series],
  )
  const rows = useMemo<RateChartRow[]>(
    () =>
      series.map((s) => ({
        timeMs: s.minute * 60_000,
        ingressCalls: s.ingressCalls,
        upstreamAttempts: s.upstreamAttempts,
        tpmTotal: s.inputTokens + s.outputTokens + s.cacheWriteTokens + s.cacheReadTokens,
        tpmBillable: s.inputTokens + s.outputTokens + s.cacheWriteTokens,
      })),
    [series],
  )
  const windowMinutes = snapshot?.windowMinutes
  const partial = windowMinutes != null && windowMinutes < minutes * PARTIAL_WINDOW_RATIO

  return (
    <Card className="mt-4 mb-6">
      <CardContent className="p-4 sm:p-5">
        <PanelTitle resolutionText="按分钟（取上一个完整分钟）" rangeLabel={rangeLabel} />
        {unavailable ? (
          <EmptyNote text="速率采集层未启用（后端未注入速率环）" />
        ) : error ? (
          <EmptyNote text={`速率数据获取失败：${extractErrorMessage(error)}`} />
        ) : isLoading ? (
          <EmptyNote text="加载中…" />
        ) : (
          <>
            {partial && (
              <PartialWindowNote windowMinutes={windowMinutes ?? 0} requestedMinutes={minutes} />
            )}
            <div className="grid grid-cols-2 gap-3 md:grid-cols-5">
              <Metric
                icon={<Activity className="h-4 w-4" />}
                label="入口 RPM"
                hint="外部请求数/分钟，看真实流量"
                value={formatNumber(snapshot?.ingressRpm ?? 0)}
                sub={`峰值 ${formatNumber(snapshot?.peakIngressRpm ?? 0)}`}
              />
              <Metric
                icon={<ArrowUpFromLine className="h-4 w-4" />}
                label="上游 RPM"
                hint="provider 跳数/分钟（含重试与故障转移），看上游压力"
                value={formatNumber(snapshot?.upstreamRpm ?? 0)}
                sub={`峰值 ${formatNumber(snapshot?.peakUpstreamRpm ?? 0)}`}
              />
              <Metric
                icon={<Repeat className="h-4 w-4" />}
                label="重试放大"
                hint="上游跳数 ÷ 入口请求数。1.00 表示零重试"
                value={(snapshot?.retryAmplification ?? 0).toFixed(2)}
                sub={
                  (snapshot?.upstreamFailures ?? 0) > 0
                    ? `失败跳 ${formatNumber(snapshot?.upstreamFailures ?? 0)}`
                    : '无失败跳'
                }
              />
              <Metric
                icon={<Coins className="h-4 w-4" />}
                label="TPM 全口径"
                hint="含缓存读取的全部 token"
                value={formatNumber(snapshot?.tpmTotal ?? 0)}
                sub={`峰值 ${formatNumber(snapshot?.peakTpmTotal ?? 0)}`}
              />
              <Metric
                icon={<Coins className="h-4 w-4" />}
                label="TPM 计费口径"
                hint="不含缓存读取。与全口径的差就是缓存命中量"
                value={formatNumber(snapshot?.tpmBillable ?? 0)}
                sub={`峰值 ${formatNumber(snapshot?.peakTpmBillable ?? 0)}`}
              />
            </div>
            {hasTraffic ? (
              <div className="mt-4">
                <RateChart rows={rows} resolution="minute" />
              </div>
            ) : (
              <EmptyNote text={`最近 ${windowMinutes ?? minutes} 分钟无流量`} />
            )}
          </>
        )}
      </CardContent>
    </Card>
  )
}

interface BucketSummary {
  avgIngressPerMin: number
  peakIngressPerMin: number
  avgTpmTotalPerMin: number
  peakTpmTotalPerMin: number
  avgTpmBillablePerMin: number
  peakTpmBillablePerMin: number
}

const EMPTY_BUCKET_SUMMARY: BucketSummary = {
  avgIngressPerMin: 0,
  peakIngressPerMin: 0,
  avgTpmTotalPerMin: 0,
  peakTpmTotalPerMin: 0,
  avgTpmBillablePerMin: 0,
  peakTpmBillablePerMin: 0,
}

/** calls/token 均值速率 = 桶内总量 ÷ 桶时长（分钟）；峰值 = 单桶均值里最大的那个。 */
function summarizeBuckets(points: TimeSeriesPoint[], bucketMinutes: number): BucketSummary {
  if (points.length === 0) return EMPTY_BUCKET_SUMMARY
  const totalMinutes = points.length * bucketMinutes
  let totalCalls = 0
  let totalTpmTotal = 0
  let totalTpmBillable = 0
  let peakIngressPerMin = 0
  let peakTpmTotalPerMin = 0
  let peakTpmBillablePerMin = 0
  for (const p of points) {
    const tpmTotal = p.inputTokens + p.outputTokens + p.cacheCreationTokens + p.cacheReadTokens
    const tpmBillable = p.inputTokens + p.outputTokens + p.cacheCreationTokens
    totalCalls += p.calls
    totalTpmTotal += tpmTotal
    totalTpmBillable += tpmBillable
    peakIngressPerMin = Math.max(peakIngressPerMin, p.calls / bucketMinutes)
    peakTpmTotalPerMin = Math.max(peakTpmTotalPerMin, tpmTotal / bucketMinutes)
    peakTpmBillablePerMin = Math.max(peakTpmBillablePerMin, tpmBillable / bucketMinutes)
  }
  return {
    avgIngressPerMin: totalCalls / totalMinutes,
    peakIngressPerMin,
    avgTpmTotalPerMin: totalTpmTotal / totalMinutes,
    peakTpmTotalPerMin,
    avgTpmBillablePerMin: totalTpmBillable / totalMinutes,
    peakTpmBillablePerMin,
  }
}

/** 桶均值模式：速率环覆盖不到的窗口（7d/30d、自定义区间），数据来自历史日志重建的小时/天分桶。 */
function BucketRatePanel({
  timeFilter,
  rangeLabel,
  query,
}: {
  timeFilter: StatsTimeFilter
  rangeLabel: string
  query: ReturnType<typeof useRateBuckets>
}) {
  const { data, isLoading, error } = query
  const points = useMemo(() => data ?? [], [data])
  const bucketMinutes = timeFilter.granularity === 'day' ? 1440 : 60
  const resolution: RateChartResolution = timeFilter.granularity === 'day' ? 'day' : 'hour'
  const resolutionText = resolution === 'day' ? '按天均值' : '按小时均值'
  const rows = useMemo<RateChartRow[]>(
    () =>
      points.map((p) => {
        const tpmTotal = p.inputTokens + p.outputTokens + p.cacheCreationTokens + p.cacheReadTokens
        const tpmBillable = p.inputTokens + p.outputTokens + p.cacheCreationTokens
        return {
          timeMs: Date.parse(p.ts),
          ingressCalls: p.calls / bucketMinutes,
          upstreamAttempts: null,
          tpmTotal: tpmTotal / bucketMinutes,
          tpmBillable: tpmBillable / bucketMinutes,
        }
      }),
    [points, bucketMinutes],
  )
  const hasTraffic = useMemo(() => points.some((p) => p.calls > 0), [points])
  const summary = useMemo(() => summarizeBuckets(points, bucketMinutes), [points, bucketMinutes])

  return (
    <Card className="mt-4 mb-6">
      <CardContent className="p-4 sm:p-5">
        <PanelTitle resolutionText={resolutionText} rangeLabel={rangeLabel} />
        {error ? (
          <EmptyNote text={`速率数据获取失败：${extractErrorMessage(error)}`} />
        ) : isLoading ? (
          <EmptyNote text="加载中…" />
        ) : points.length === 0 ? (
          <EmptyNote text="窗口内无数据" />
        ) : (
          <>
            <BucketScopeNote resolutionText={resolutionText} />
            <div className="grid grid-cols-1 gap-3 sm:grid-cols-3">
              <Metric
                icon={<Activity className="h-4 w-4" />}
                label="入口请求/分钟"
                hint="均值 = 窗口内总调用 ÷ 窗口分钟数；不是分钟级真值"
                value={formatRate(summary.avgIngressPerMin)}
                sub={`峰值（单桶均值） ${formatRate(summary.peakIngressPerMin)}`}
              />
              <Metric
                icon={<Coins className="h-4 w-4" />}
                label="TPM 全口径/分钟"
                hint="含缓存读取，均值 = 窗口内总 token ÷ 窗口分钟数"
                value={formatRate(summary.avgTpmTotalPerMin)}
                sub={`峰值（单桶均值） ${formatRate(summary.peakTpmTotalPerMin)}`}
              />
              <Metric
                icon={<Coins className="h-4 w-4" />}
                label="TPM 计费口径/分钟"
                hint="不含缓存读取"
                value={formatRate(summary.avgTpmBillablePerMin)}
                sub={`峰值（单桶均值） ${formatRate(summary.peakTpmBillablePerMin)}`}
              />
            </div>
            {hasTraffic ? (
              <div className="mt-4">
                <RateChart rows={rows} resolution={resolution} />
              </div>
            ) : (
              <EmptyNote text="窗口内无流量" />
            )}
          </>
        )}
      </CardContent>
    </Card>
  )
}

function BucketScopeNote({ resolutionText }: { resolutionText: string }) {
  return (
    <p className="mb-3 rounded-md bg-amber-500/10 px-2.5 py-1.5 text-[11px] text-amber-600">
      窗口超过速率环覆盖的 24 小时，已回退到{resolutionText}——上游跳数 / 重试放大暂无该口径数据
    </p>
  )
}

function PanelTitle({ resolutionText, rangeLabel }: { resolutionText: string; rangeLabel: string }) {
  return (
    <div className="mb-3">
      <h2 className="text-base font-semibold tracking-tight">实时速率</h2>
      <p className="text-[12px] text-muted-foreground">
        {resolutionText} · {rangeLabel} · 跟随上方时间范围，不受入口 Key / 分组筛选影响
      </p>
    </div>
  )
}

function PartialWindowNote({
  windowMinutes,
  requestedMinutes,
}: {
  windowMinutes: number
  requestedMinutes: number
}) {
  return (
    <p className="mb-3 rounded-md bg-amber-500/10 px-2.5 py-1.5 text-[11px] text-amber-600">
      速率数据自服务重启后累计，当前 {windowMinutes} 分钟（已选{humanizeMinutes(requestedMinutes)}）——
      不是流量下降，只是还没攒够选定的窗口长度
    </p>
  )
}

function Metric({
  icon,
  label,
  hint,
  value,
  sub,
}: {
  icon: React.ReactNode
  label: string
  hint: string
  value: string
  sub: string
}) {
  return (
    <div className="rounded-lg border bg-card p-3" title={hint}>
      <div className="flex items-center gap-1.5 text-[12px] text-muted-foreground">
        {icon}
        <span>{label}</span>
      </div>
      <div className="mt-1 text-xl font-semibold tabular-nums">{value}</div>
      <div className="text-[11px] text-muted-foreground">{sub}</div>
    </div>
  )
}

function EmptyNote({ text }: { text: string }) {
  return (
    <div className="flex h-24 items-center justify-center text-[13px] text-muted-foreground">
      {text}
    </div>
  )
}
