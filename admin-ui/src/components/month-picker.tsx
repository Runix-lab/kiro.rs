import { useMemo } from 'react'
import { Calendar } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'

export function currentMonthValue(): string {
  const d = new Date()
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}`
}

export function shiftMonth(month: string, delta: number): string {
  const [y, m] = month.split('-').map(Number)
  const d = new Date(y, m - 1 + delta, 1)
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}`
}

/**
 * 月份选择器：`<input type="month">` + 本月/上月快捷按钮。
 *
 * 从「月度总账」弹窗抽出的共享控件——「模型成本分析」需要同一套月份选择交互，
 * 复制一份会制造两处独立漂移的风险（间距、快捷按钮文案、边界处理各改各的）。
 * 抽成单一事实源后，所有按月份驱动的弹窗共用同一份切换逻辑与样式。
 */
export function MonthPicker({
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
