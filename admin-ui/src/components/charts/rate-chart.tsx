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
import type { MinuteSample } from '@/types/api'
import { tooltipCursorStyle } from './tooltip-style'
import { formatNumber } from '@/lib/utils'

interface Props {
  data: MinuteSample[]
}

/** 与 time-series-chart 同一套色板，避免同页两图配色不一致。 */
const COLORS = {
  ingress: '#3b82f6',
  upstream: '#f59e0b',
  tpmTotal: '#06b6d4',
  tpmBillable: '#10b981',
} as const

interface Row {
  label: string
  ingressCalls: number
  upstreamAttempts: number
  tpmTotal: number
  tpmBillable: number
}

function toRow(s: MinuteSample): Row {
  // 后端给的是 Unix 分钟数，×60000 才是毫秒。
  const d = new Date(s.minute * 60_000)
  const hh = String(d.getHours()).padStart(2, '0')
  const mm = String(d.getMinutes()).padStart(2, '0')
  return {
    label: `${hh}:${mm}`,
    ingressCalls: s.ingressCalls,
    upstreamAttempts: s.upstreamAttempts,
    tpmTotal: s.inputTokens + s.outputTokens + s.cacheWriteTokens + s.cacheReadTokens,
    tpmBillable: s.inputTokens + s.outputTokens + s.cacheWriteTokens,
  }
}

/**
 * 分钟级速率曲线：入口/上游 RPM 走左轴，TPM 走右轴。
 *
 * 两个 Y 轴是必须的——RPM 是个位到几十，TPM 动辄上万，同轴会把 RPM 压成一条平线。
 */
export const RateChart = memo(function RateChart({ data }: Props) {
  const rows = useMemo(() => data.map(toRow), [data])
  // 120 个点全打标签会糊成一片，抽稀到 ~8 个。
  const interval = useMemo(() => Math.max(0, Math.floor(rows.length / 8) - 1), [rows.length])

  return (
    <div className="h-[420px] w-full">
      <ResponsiveContainer width="100%" height="100%">
        <LineChart data={rows} margin={{ top: 16, right: 8, left: 0, bottom: 8 }}>
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
            tickFormatter={(v: number) => formatNumber(v)}
            width={56}
            allowDecimals={false}
            label={{ value: 'RPM', angle: -90, position: 'insideLeft', fontSize: 11, offset: 8 }}
          />
          <YAxis
            yAxisId="tpm"
            orientation="right"
            tick={{ fontSize: 12 }}
            className="fill-muted-foreground"
            tickFormatter={(v: number) => formatNumber(v)}
            width={68}
            allowDecimals={false}
            label={{ value: 'TPM', angle: 90, position: 'insideRight', fontSize: 11, offset: 8 }}
          />
          <Tooltip
            cursor={tooltipCursorStyle}
            formatter={(v: number) => formatNumber(v)}
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
          />
          <Line
            yAxisId="rpm"
            type="monotone"
            dataKey="upstreamAttempts"
            name="上游跳数"
            stroke={COLORS.upstream}
            strokeWidth={2}
            dot={false}
          />
          <Line
            yAxisId="tpm"
            type="monotone"
            dataKey="tpmTotal"
            name="TPM 全口径"
            stroke={COLORS.tpmTotal}
            strokeWidth={1.5}
            dot={false}
          />
          <Line
            yAxisId="tpm"
            type="monotone"
            dataKey="tpmBillable"
            name="TPM 计费"
            stroke={COLORS.tpmBillable}
            strokeWidth={1.5}
            dot={false}
          />
        </LineChart>
      </ResponsiveContainer>
    </div>
  )
})
