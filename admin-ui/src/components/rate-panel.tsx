import { useMemo } from 'react'
import { Card, CardContent } from '@/components/ui/card'
import { Activity, ArrowUpFromLine, Coins, Repeat } from 'lucide-react'
import { useRate } from '@/hooks/use-rate'
import { formatNumber } from '@/lib/utils'
import { RateChart } from '@/components/charts/rate-chart'

/**
 * 实时速率面板：分钟级 RPM / TPM。
 *
 * 数字成对显示是刻意的：
 * - 入口 RPM 与上游 RPM 的差就是重试放大（一次外部请求故障转移三次 = 入口 1、上游 3）
 * - 全口径 TPM 与计费口径 TPM 的差就是缓存读取量，生产实测能差几十倍
 *
 * 只看其中一个数会得出错误结论，所以两两并排 + 直接给出放大倍数。
 *
 * 本面板不受页面上的时间范围 / 分组筛选影响：速率环是固定 120 分钟窗口、只存标量，
 * 没有 by_key / by_model 维度可筛。
 */
export function RatePanel() {
  const { snapshot, loading, error, unavailable } = useRate()
  const series = useMemo(() => snapshot?.series ?? [], [snapshot])
  const hasTraffic = useMemo(
    () => series.some((s) => s.ingressCalls > 0 || s.upstreamAttempts > 0),
    [series],
  )

  return (
    <Card className="mt-4">
      <CardContent className="p-4">
        <PanelTitle windowMinutes={snapshot?.windowMinutes} />
        {unavailable ? (
          <EmptyNote text="速率采集层未启用（后端未注入速率环）" />
        ) : error ? (
          <EmptyNote text={`速率数据获取失败：${error}`} />
        ) : loading ? (
          <EmptyNote text="加载中…" />
        ) : (
          <>
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
                <RateChart data={series} />
              </div>
            ) : (
              <EmptyNote text={`最近 ${snapshot?.windowMinutes ?? 120} 分钟无流量`} />
            )}
          </>
        )}
      </CardContent>
    </Card>
  )
}

function PanelTitle({ windowMinutes }: { windowMinutes?: number }) {
  return (
    <div className="mb-3">
      <h2 className="text-base font-semibold tracking-tight">实时速率</h2>
      <p className="text-[12px] text-muted-foreground">
        取上一个完整分钟 · 窗口 {windowMinutes ?? 120} 分钟 · 不受上方时间与分组筛选影响
      </p>
    </div>
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
