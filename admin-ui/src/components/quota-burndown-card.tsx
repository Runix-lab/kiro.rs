import { useMemo } from 'react'
import { Card, CardContent } from '@/components/ui/card'
import { Badge } from '@/components/ui/badge'
import { useTimeSeries } from '@/hooks/use-stats'
import { formatNumber } from '@/lib/utils'
import type { CredentialStatusItem } from '@/types/api'

/** 用最近 N 天的日桶估日烧速；N 天足够摊掉单日波动，又不至于把很久以前的低峰算进来。 */
const BURN_WINDOW_DAYS = 7

function isoDate(d: Date): string {
  return d.toISOString().slice(0, 10)
}

/**
 * 额度烧穿卡：把「池子还剩多少 / 每天烧多少 / 够不够撑到重置」压成一张进度条。
 *
 * 口径刻意保持粗糙好解释：日烧速优先取**昨天**（一个完整自然日，不受今天进行中
 * 的半天影响），昨天没数据才退到 7 日均值。剩余额度直接来自各凭据的上游余额快照，
 * 是权威值而非我们自己的累加。
 */
export function QuotaBurndownCard({ credentials }: { credentials: CredentialStatusItem[] }) {
  const timeFilter = useMemo(() => {
    const end = new Date()
    const start = new Date(end.getTime() - BURN_WINDOW_DAYS * 86400_000)
    return { startDate: isoDate(start), endDate: isoDate(end), granularity: 'day' as const }
  }, [])
  const { data: series } = useTimeSeries(timeFilter)

  const stats = useMemo(() => {
    const withBalance = credentials.filter((c) => c.balance)
    const remaining = withBalance.reduce((s, c) => s + (c.balance?.remaining ?? 0), 0)
    // 重置时间取各凭据里最早的一个：先到的那个会先断供
    const resetTs = withBalance
      .map((c) => c.balance?.nextResetAt)
      .filter((t): t is number => typeof t === 'number' && t > 0)
      .sort((a, b) => a - b)[0]

    const points = series ?? []
    const yesterday = isoDate(new Date(Date.now() - 86400_000))
    const yesterdayCredits = points.find((p) => p.ts.slice(0, 10) === yesterday)?.credits
    // 今天那一格还在累加，不能算进均值，否则会把日烧速拉低
    const today = isoDate(new Date())
    const completeDays = points.filter((p) => p.ts.slice(0, 10) !== today && p.credits > 0)
    const avgCredits = completeDays.length
      ? completeDays.reduce((s, p) => s + p.credits, 0) / completeDays.length
      : 0
    const burnPerDay = yesterdayCredits && yesterdayCredits > 0 ? yesterdayCredits : avgCredits
    const basis = yesterdayCredits && yesterdayCredits > 0 ? '昨日' : `${completeDays.length} 日均值`

    const runwayDays = burnPerDay > 0 ? remaining / burnPerDay : Infinity
    const daysToReset = resetTs ? Math.max(0, (resetTs * 1000 - Date.now()) / 86400_000) : null
    const shortfall =
      daysToReset != null && burnPerDay > 0 ? Math.max(0, burnPerDay * daysToReset - remaining) : 0

    return {
      remaining,
      burnPerDay,
      basis,
      avgCredits,
      runwayDays,
      daysToReset,
      shortfall,
      resetTs,
      covered: withBalance.length,
      totalCreds: credentials.length,
    }
  }, [credentials, series])

  if (!stats.covered) return null

  // 进度条：跑道占「撑到重置所需时长」的比例；够用就是满格
  const ratio =
    stats.daysToReset && stats.daysToReset > 0 && Number.isFinite(stats.runwayDays)
      ? Math.min(1, stats.runwayDays / stats.daysToReset)
      : 1
  const enough = stats.shortfall <= 0
  const barColor = enough ? 'bg-emerald-500' : ratio > 0.6 ? 'bg-amber-500' : 'bg-destructive'

  const runwayText = Number.isFinite(stats.runwayDays)
    ? `${stats.runwayDays.toFixed(1)} 天`
    : '无消耗'

  return (
    <Card className="mb-5 sm:mb-6">
      <CardContent className="p-3 sm:p-5">
        <div className="mb-2 flex flex-wrap items-baseline justify-between gap-2">
          <div className="flex items-baseline gap-2">
            <span className="text-[11px] font-medium text-muted-foreground sm:text-[13px]">
              额度可用时长
            </span>
            <span className="text-2xl font-semibold tracking-tight tabular-nums sm:text-3xl">
              {runwayText}
            </span>
            {enough ? (
              <Badge variant="success">够撑到重置</Badge>
            ) : (
              <Badge variant="destructive">
                缺 {formatNumber(Math.round(stats.shortfall))} credit
              </Badge>
            )}
          </div>
          <div className="text-[11px] text-muted-foreground">
            按{stats.basis}消耗 {formatNumber(Math.round(stats.burnPerDay))} credit/天
            {stats.avgCredits > 0 && stats.basis === '昨日' && (
              <span className="text-muted-foreground/70">
                {' '}
                · 7 日均 {formatNumber(Math.round(stats.avgCredits))}
              </span>
            )}
          </div>
        </div>

        <div className="h-2.5 w-full overflow-hidden rounded-full bg-muted">
          <div
            className={`h-full rounded-full transition-all ${barColor}`}
            style={{ width: `${Math.max(2, ratio * 100)}%` }}
          />
        </div>

        <div className="mt-2 flex flex-wrap items-center gap-x-3 gap-y-1 text-[11px] text-muted-foreground">
          <span>
            剩余{' '}
            <span className="font-medium tabular-nums text-foreground">
              {formatNumber(Math.round(stats.remaining))}
            </span>{' '}
            credit
          </span>
          {stats.daysToReset != null && (
            <span>
              · 距重置{' '}
              <span className="tabular-nums text-foreground">{stats.daysToReset.toFixed(1)}</span> 天
              {stats.resetTs && (
                <span className="text-muted-foreground/70">
                  {' '}
                  ({new Date(stats.resetTs * 1000).toLocaleDateString('zh-CN')})
                </span>
              )}
            </span>
          )}
          {!enough && stats.daysToReset != null && Number.isFinite(stats.runwayDays) && (
            <span className="text-destructive">
              · 预计{' '}
              {new Date(Date.now() + stats.runwayDays * 86400_000).toLocaleString('zh-CN', {
                month: '2-digit',
                day: '2-digit',
                hour: '2-digit',
                minute: '2-digit',
                hour12: false,
              })}{' '}
              耗尽
            </span>
          )}
          {stats.covered < stats.totalCreds && (
            <span className="text-muted-foreground/70">
              · 仅统计有余额数据的 {stats.covered}/{stats.totalCreds} 个凭据
            </span>
          )}
        </div>
      </CardContent>
    </Card>
  )
}
