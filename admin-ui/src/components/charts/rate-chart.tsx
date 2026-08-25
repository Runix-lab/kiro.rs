import { memo, useMemo } from 'react'
import {
  CartesianGrid,
  Legend,
  Line,
  LineChart,
  ResponsiveContainer,
  Tooltip,
  XAxis,
  YAxis,
} from 'recharts'
import { tooltipCursorStyle } from './tooltip-style'
import { formatBeijingHM, formatBeijingMd, formatBeijingMdHm, formatRate } from '@/lib/utils'

/** 与 time-series-chart 同一套色板，避免同页两图配色不一致。 */
const COLORS = {
  ingress: '#3b82f6',
  upstream: '#f59e0b',
  tpmTotal: '#06b6d4',
  tpmBillable: '#10b981',
} as const

/**
 * 图表的时间分辨率：
 * - `minute`：数据来自速率环，逐分钟真实值，横轴 `HH:mm`
 * - `hour` / `day`：环覆盖不到（7d/30d 或跨天自定义区间）时的回退，数据来自
 *   `/stats/timeseries` 按桶换算出的均值速率，横轴分别是 `MM-DD HH:mm` / `MM-DD`
 */
export type RateChartResolution = 'minute' | 'hour' | 'day'

export interface RateChartRow {
  /** epoch 毫秒（已是绝对时刻，画图时按北京时间格式化） */
  timeMs: number
  ingressCalls: number
  /** 桶均值模式（hour/day）没有上游跳数数据，恒为 null——不能拿 0 充数，0 会被误读成"零重试"。 */
  upstreamAttempts: number | null
  tpmTotal: number
  tpmBillable: number
}

interface Props {
  rows: RateChartRow[]
  resolution: RateChartResolution
}

interface ChartRow {
  label: string
  ingressCalls: number
  upstreamAttempts: number | null
  tpmTotal: number
  tpmBillable: number
}

function labelFor(ms: number, resolution: RateChartResolution): string {
  if (resolution === 'minute') return formatBeijingHM(ms)
  if (resolution === 'hour') return formatBeijingMdHm(ms)
  return formatBeijingMd(ms)
}

function toRow(r: RateChartRow, resolution: RateChartResolution): ChartRow {
  return {
    label: labelFor(r.timeMs, resolution),
    ingressCalls: r.ingressCalls,
    upstreamAttempts: r.upstreamAttempts,
    tpmTotal: r.tpmTotal,
    tpmBillable: r.tpmBillable,
  }
}

/**
 * 速率曲线：入口/上游 RPM 走左轴，TPM 走右轴，画出整段选中窗口（而不是只有最后
 * 一个点）——这是本图表存在的意义：跟页面顶部的时间范围选择器保持一致。
 *
 * 两个 Y 轴是必须的——RPM 是个位到几十，TPM 动辄上万，同轴会把 RPM 压成一条平线。
 *
 * `resolution === 'minute'` 时画分钟级真实速率；`'hour'` / `'day'` 时画的是桶均值
 * 换算出的速率，此时没有上游跳数数据，对应的线和图例整条不渲染（而不是画一条假的
 * 常数线）。
 */
export const RateChart = memo(function RateChart({ rows, resolution }: Props) {
  const chartRows = useMemo(() => rows.map((r) => toRow(r, resolution)), [rows, resolution])
  const hasUpstream = resolution === 'minute'
  // 点数可能到 1440（24h 分钟级），全打标签会糊成一片，抽稀到 ~10 个。
  const interval = useMemo(
    () => Math.max(0, Math.ceil(chartRows.length / 10) - 1),
    [chartRows.length],
  )
  const rpmAxisLabel = resolution === 'minute' ? 'RPM' : 'RPM(均值)'
  const tpmAxisLabel = resolution === 'minute' ? 'TPM' : 'TPM(均值)'

  return (
    <div className="h-[420px] w-full">
      <ResponsiveContainer width="100%" height="100%">
        <LineChart data={chartRows} margin={{ top: 16, right: 8, left: 0, bottom: 8 }}>
          <CartesianGrid strokeDasharray="3 3" className="stroke-border/50" />
          <XAxis
            dataKey="label"
            tick={{ fontSize: 12 }}
            className="fill-muted-foreground"
            interval={interval}
          />
          <YAxis
            yAxisId="rpm"
            tick={{ fontSize: 12 }}
            className="fill-muted-foreground"
            tickFormatter={(v: number) => formatRate(v)}
            width={56}
            label={{
              value: rpmAxisLabel,
              angle: -90,
              position: 'insideLeft',
              fontSize: 11,
              offset: 8,
            }}
          />
          <YAxis
            yAxisId="tpm"
            orientation="right"
            tick={{ fontSize: 12 }}
            className="fill-muted-foreground"
            tickFormatter={(v: number) => formatRate(v)}
            width={68}
            label={{
              value: tpmAxisLabel,
              angle: 90,
              position: 'insideRight',
              fontSize: 11,
              offset: 8,
            }}
          />
          <Tooltip
            cursor={tooltipCursorStyle}
            formatter={(v: number) => formatRate(v)}
            contentStyle={{
              background: 'rgba(20,20,20,0.94)',
              border: '1px solid rgba(255,255,255,0.08)',
              borderRadius: 10,
              fontSize: 12,
              color: '#fff',
            }}
            labelStyle={{ color: 'rgba(255,255,255,0.85)', fontWeight: 500, marginBottom: 4 }}
            itemStyle={{ color: '#fff', padding: '2px 0' }}
          />
          <Legend wrapperStyle={{ fontSize: 12 }} />
          <Line
            yAxisId="rpm"
            type="monotone"
            dataKey="ingressCalls"
            name="入口请求"
            stroke={COLORS.ingress}
            strokeWidth={2}
            dot={false}
            isAnimationActive={false}
          />
          {hasUpstream && (
            <Line
              yAxisId="rpm"
              type="monotone"
              dataKey="upstreamAttempts"
              name="上游跳数"
              stroke={COLORS.upstream}
              strokeWidth={2}
              dot={false}
              isAnimationActive={false}
            />
          )}
          <Line
            yAxisId="tpm"
            type="monotone"
            dataKey="tpmTotal"
            name="TPM 全口径"
            stroke={COLORS.tpmTotal}
            strokeWidth={1.5}
            dot={false}
            isAnimationActive={false}
          />
          <Line
            yAxisId="tpm"
            type="monotone"
            dataKey="tpmBillable"
            name="TPM 计费"
            stroke={COLORS.tpmBillable}
            strokeWidth={1.5}
            dot={false}
            isAnimationActive={false}
          />
        </LineChart>
      </ResponsiveContainer>
    </div>
  )
})
