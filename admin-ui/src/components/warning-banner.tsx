import { AlertTriangle } from 'lucide-react'
import { cn } from '@/lib/utils'

/**
 * 统一的警示条外壳：需要人工处理的信号都走这一个组件，读起来是一叠按严重度
 * 排列的警示，而不是每个弹窗各写一套。red = 最高危（数据看似正常实则失真），
 * amber = 已知的、有明确处理路径的缺口。
 *
 * 从「月度总账」弹窗抽出，「模型成本分析」复用同一套视觉语言渲染
 * missingDays / malformedLines 之类的信号。
 */
export function WarningBanner({
  tone,
  title,
  children,
}: {
  tone: 'red' | 'amber' | 'emerald'
  title: string
  children: React.ReactNode
}) {
  return (
    <div
      className={cn(
        'rounded-md border p-3 sm:p-4',
        tone === 'red'
          ? 'border-destructive/40 bg-destructive/5'
          : tone === 'emerald'
            ? 'border-emerald-500/40 bg-emerald-500/10'
            : 'border-amber-500/40 bg-amber-500/10',
      )}
    >
      <div
        className={cn(
          'mb-1.5 flex items-center gap-2',
          tone === 'red'
            ? 'text-destructive'
            : tone === 'emerald'
              ? 'text-emerald-600 dark:text-emerald-400'
              : 'text-amber-600 dark:text-amber-400',
        )}
      >
        <AlertTriangle className="h-4 w-4 shrink-0" />
        <h3 className="text-[13px] font-semibold">{title}</h3>
      </div>
      {children}
    </div>
  )
}
