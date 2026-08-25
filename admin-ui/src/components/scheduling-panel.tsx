import { useEffect, useState } from 'react'
import { toast } from 'sonner'
import { Card, CardContent, CardHeader, CardTitle, CardDescription } from '@/components/ui/card'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Switch } from '@/components/ui/switch'
import { Badge } from '@/components/ui/badge'
import {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from '@/components/ui/tooltip'
import {
  useSchedulingConfig,
  useSetSchedulingConfig,
  useRunScheduling,
} from '@/hooks/use-credentials'
import { extractErrorMessage } from '@/lib/utils'
import type {
  SchedulingChangeReason,
  SchedulingConfig,
  SchedulingProfile,
  SchedulingRunResult,
} from '@/types/api'

const PROFILE_OPTIONS: { value: SchedulingProfile; label: string; hint: string }[] = [
  {
    value: 'manual',
    label: '手动',
    hint: '只运行额度守卫与首选层保护两条自动规则，手工设置的优先级不会被改动',
  },
  {
    value: 'throughput',
    label: '提升吞吐',
    hint: '把所有凭据的优先级拉平到 50，让流量在负载均衡「均衡」模式下打散到各账号',
  },
  {
    value: 'conserve',
    label: '节约额度',
    hint: '剩余额度最多的凭据排最前，几个账号一起匀速消耗，避免某个先见底',
  },
  {
    value: 'drain',
    label: '优先烧完',
    hint: '剩余额度最少的凭据排最前，优先烧完这部分残余额度，赶在重置前尽量用完',
  },
]

const REASON_LABELS: Record<SchedulingChangeReason, string> = {
  quotaDemote: '额度超阈值降级',
  quotaRestore: '用量回落恢复',
  topTierRefill: '补齐首选层',
  profileRebalance: '按取向铺排',
}

/** 表单态：数值字段用字符串承载，允许输入过程中的中间态（空串 / 前导符号）不被强行纠正 */
interface SchedulingForm {
  enabled: boolean
  demoteThresholdPct: string
  demoteTo: string
  minTopTier: string
  profile: SchedulingProfile
}

interface FormErrors {
  demoteThresholdPct?: string
  demoteTo?: string
  minTopTier?: string
}

function configToForm(config: SchedulingConfig): SchedulingForm {
  return {
    enabled: config.enabled,
    demoteThresholdPct: String(config.demoteThresholdPct),
    demoteTo: String(config.demoteTo),
    minTopTier: String(config.minTopTier),
    profile: config.profile,
  }
}

function validate(form: SchedulingForm): FormErrors {
  const errors: FormErrors = {}
  const threshold = Number(form.demoteThresholdPct)
  const demoteTo = Number(form.demoteTo)
  const minTopTier = Number(form.minTopTier)

  if (form.demoteThresholdPct.trim() === '' || !Number.isFinite(threshold) || threshold < 0 || threshold > 100) {
    errors.demoteThresholdPct = '需在 0-100 之间'
  }
  if (form.demoteTo.trim() === '' || !Number.isFinite(demoteTo) || demoteTo <= 50) {
    errors.demoteTo = '需大于 50（正常档基准）'
  }
  if (
    form.minTopTier.trim() === '' ||
    !Number.isFinite(minTopTier) ||
    !Number.isInteger(minTopTier) ||
    minTopTier < 1
  ) {
    errors.minTopTier = '需为 ≥1 的整数'
  }
  return errors
}

/**
 * 调度策略面板：把额度守卫 + 首选层保护两条自动规则，与四档调度取向暴露给运营手动配置。
 *
 * 表单态只在配置首次加载时从服务端同步一次（`form === null` 时），此后的后台刷新
 * 不会覆盖操作员正在编辑但还未保存的输入。
 */
export function SchedulingPanel() {
  const { data: config, isLoading } = useSchedulingConfig()
  const { mutate: saveConfig, isPending: saving } = useSetSchedulingConfig()
  const { mutate: runOnce, isPending: running } = useRunScheduling()

  const [form, setForm] = useState<SchedulingForm | null>(null)
  const [runResult, setRunResult] = useState<SchedulingRunResult | null>(null)

  useEffect(() => {
    if (config && form === null) {
      setForm(configToForm(config))
    }
  }, [config, form])

  const handleRun = () => {
    runOnce(undefined, {
      onSuccess: (result) => {
        setRunResult(result)
        if (result.applied === 0) toast.info('本轮无需调整')
        else toast.success(`已调整 ${result.applied} 个凭据的优先级`)
      },
      onError: (err) => toast.error(`执行失败: ${extractErrorMessage(err)}`),
    })
  }

  // 首次拉取配置完成前，仅渲染标题与占位文案；不提前渲染表单避免 null 态分支
  if (!form) {
    return (
      <Card className="mt-5 sm:mt-6">
        <CardHeader className="pb-3">
          <CardTitle>调度策略</CardTitle>
          <CardDescription>
            优先级约定：小于 50 优先消耗 · 50 正常（新建凭据默认）· 大于 50 退居二线（仍参与故障转移与负载均衡，但不是主力输出）
          </CardDescription>
        </CardHeader>
        <CardContent className="pt-0">
          <div className="text-sm text-muted-foreground">{isLoading ? '加载中…' : '暂无数据'}</div>
        </CardContent>
      </Card>
    )
  }

  const errors = validate(form)
  const hasErrors = Object.keys(errors).length > 0
  const fieldsDisabled = saving || !form.enabled

  const handleSave = () => {
    if (hasErrors) return
    const payload: SchedulingConfig = {
      enabled: form.enabled,
      demoteThresholdPct: Number(form.demoteThresholdPct),
      demoteTo: Number(form.demoteTo),
      minTopTier: Number(form.minTopTier),
      profile: form.profile,
    }
    saveConfig(payload, {
      onSuccess: (saved) => {
        setForm(configToForm(saved))
        toast.success('调度策略已保存')
      },
      onError: (err) => toast.error(`保存失败: ${extractErrorMessage(err)}`),
    })
  }

  return (
    <Card className="mt-5 sm:mt-6">
      <CardHeader className="pb-3">
        <CardTitle>调度策略</CardTitle>
        <CardDescription>
          优先级约定：小于 50 优先消耗 · 50 正常（新建凭据默认）· 大于 50 退居二线（仍参与故障转移与负载均衡，但不是主力输出）
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-5 pt-0">
        {/* 总开关 */}
        <div className="flex items-center justify-between gap-3 rounded-xl border border-border/60 bg-secondary/30 px-3.5 py-3">
          <div className="min-w-0">
            <div className="text-sm font-medium">{form.enabled ? '已启用' : '已关闭'}</div>
            <p className="mt-0.5 text-xs text-muted-foreground">
              关闭时不会自动改动任何优先级
            </p>
          </div>
          <Switch
            checked={form.enabled}
            disabled={saving}
            onCheckedChange={(v) => setForm((f) => (f ? { ...f, enabled: v } : f))}
          />
        </div>

        {/* 调度取向 */}
        <div className={form.enabled ? '' : 'opacity-60'}>
          <div className="mb-2 text-sm font-medium">调度取向</div>
          <TooltipProvider delayDuration={150}>
            <div className="grid grid-cols-2 gap-1.5 sm:flex sm:flex-wrap">
              {PROFILE_OPTIONS.map((opt) => (
                <Tooltip key={opt.value}>
                  <TooltipTrigger asChild>
                    <Button
                      type="button"
                      size="sm"
                      variant={form.profile === opt.value ? 'default' : 'outline'}
                      disabled={fieldsDisabled}
                      onClick={() =>
                        setForm((f) => (f ? { ...f, profile: opt.value } : f))
                      }
                    >
                      {opt.label}
                    </Button>
                  </TooltipTrigger>
                  <TooltipContent side="bottom">{opt.hint}</TooltipContent>
                </Tooltip>
              ))}
            </div>
          </TooltipProvider>
        </div>

        {/* 三个数值参数 */}
        <div className="grid grid-cols-1 gap-4 sm:grid-cols-3">
          <div className={form.enabled ? '' : 'opacity-60'}>
            <label className="text-sm font-medium" htmlFor="scheduling-threshold">
              降级阈值（%）
            </label>
            <Input
              id="scheduling-threshold"
              type="number"
              min={0}
              max={100}
              value={form.demoteThresholdPct}
              disabled={fieldsDisabled}
              onChange={(e) =>
                setForm((f) => (f ? { ...f, demoteThresholdPct: e.target.value } : f))
              }
              className="mt-1.5"
            />
            <p className="mt-1 text-xs text-muted-foreground">
              用量超过该阈值即触发额度守卫降级
            </p>
            {errors.demoteThresholdPct && (
              <p className="mt-1 text-xs text-destructive">{errors.demoteThresholdPct}</p>
            )}
          </div>

          <div className={form.enabled ? '' : 'opacity-60'}>
            <label className="text-sm font-medium" htmlFor="scheduling-demote-to">
              降级到优先级
            </label>
            <Input
              id="scheduling-demote-to"
              type="number"
              min={51}
              value={form.demoteTo}
              disabled={fieldsDisabled}
              onChange={(e) => setForm((f) => (f ? { ...f, demoteTo: e.target.value } : f))}
              className="mt-1.5"
            />
            <p className="mt-1 text-xs text-muted-foreground">
              触发降级后临时改到的优先级值，用量回落到重置周期后自动恢复原值
            </p>
            {errors.demoteTo && <p className="mt-1 text-xs text-destructive">{errors.demoteTo}</p>}
          </div>

          <div className={form.enabled ? '' : 'opacity-60'}>
            <label className="text-sm font-medium" htmlFor="scheduling-min-top-tier">
              首选层最少保留
            </label>
            <Input
              id="scheduling-min-top-tier"
              type="number"
              min={1}
              step={1}
              value={form.minTopTier}
              disabled={fieldsDisabled}
              onChange={(e) =>
                setForm((f) => (f ? { ...f, minTopTier: e.target.value } : f))
              }
              className="mt-1.5"
            />
            <p className="mt-1 text-xs text-muted-foreground">
              保证至少这么多个凭据共享最小（最优先）优先级；只有 1 个会导致粘性选择把流量全钉在一个账号上
            </p>
            {errors.minTopTier && (
              <p className="mt-1 text-xs text-destructive">{errors.minTopTier}</p>
            )}
          </div>
        </div>

        {/* 操作按钮 */}
        <div className="flex flex-wrap items-center gap-2 pt-1">
          <Button onClick={handleSave} disabled={saving || hasErrors}>
            {saving ? '保存中…' : '保存'}
          </Button>
          <Button variant="outline" onClick={handleRun} disabled={running}>
            {running ? '执行中…' : '立即执行一轮'}
          </Button>
          {hasErrors && (
            <span className="text-xs text-destructive">请先修正上方标红的字段再保存</span>
          )}
        </div>

        {/* 手动执行结果 */}
        {runResult && (
          <div className="rounded-xl border border-border/60 bg-secondary/20 px-3.5 py-3">
            {runResult.applied === 0 ? (
              <p className="text-sm text-muted-foreground">本轮无需调整</p>
            ) : (
              <>
                <div className="mb-2 flex items-center gap-2">
                  <span className="text-sm font-medium">本轮已调整</span>
                  <Badge variant="secondary">{runResult.applied}</Badge>
                </div>
                <ul className="space-y-1 text-xs text-muted-foreground">
                  {runResult.changes.map((c, i) => (
                    <li key={`${c.id}-${i}`} className="tabular-nums">
                      凭据 #{c.id}: {c.from} → {c.to}
                      <span className="ml-1.5 text-muted-foreground/70">
                        （{REASON_LABELS[c.reason]}）
                      </span>
                    </li>
                  ))}
                </ul>
              </>
            )}
          </div>
        )}
      </CardContent>
    </Card>
  )
}
