import { useEffect, useState } from 'react'
import { AlertTriangle } from 'lucide-react'
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
import { useConfirm } from '@/components/ui/confirm-dialog'
import {
  useSchedulingConfig,
  useSetSchedulingConfig,
  useRunScheduling,
  useSchedulingPresets,
  useThroughputEstimate,
  useApplyMaxThroughput,
} from '@/hooks/use-credentials'
import { cn, extractErrorMessage } from '@/lib/utils'
import { TH_LABEL, TH_NUM, TD_LABEL, TD_NUM, TD_NUM_STRONG } from '@/lib/table-styles'
import type {
  MaxThroughputParams,
  MaxThroughputResult,
  SchedulingChangeReason,
  SchedulingConfig,
  SchedulingProfile,
  SchedulingProfilePreset,
  SchedulingRunResult,
} from '@/types/api'

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
  throughputBurnBelowPct: string
  throughputReserveAtPct: string
  profile: SchedulingProfile
}

interface FormErrors {
  demoteThresholdPct?: string
  demoteTo?: string
  minTopTier?: string
  throughputBurnBelowPct?: string
  throughputReserveAtPct?: string
}

function configToForm(config: SchedulingConfig): SchedulingForm {
  return {
    enabled: config.enabled,
    demoteThresholdPct: String(config.demoteThresholdPct),
    demoteTo: String(config.demoteTo),
    minTopTier: String(config.minTopTier),
    throughputBurnBelowPct: String(config.throughputBurnBelowPct),
    throughputReserveAtPct: String(config.throughputReserveAtPct),
    profile: config.profile,
  }
}

function formToPayload(form: SchedulingForm): SchedulingConfig {
  return {
    enabled: form.enabled,
    demoteThresholdPct: Number(form.demoteThresholdPct),
    demoteTo: Number(form.demoteTo),
    minTopTier: Number(form.minTopTier),
    throughputBurnBelowPct: Number(form.throughputBurnBelowPct),
    throughputReserveAtPct: Number(form.throughputReserveAtPct),
    profile: form.profile,
  }
}

function validate(form: SchedulingForm): FormErrors {
  const errors: FormErrors = {}
  const threshold = Number(form.demoteThresholdPct)
  const demoteTo = Number(form.demoteTo)
  const minTopTier = Number(form.minTopTier)
  const burnBelow = Number(form.throughputBurnBelowPct)
  const reserveAt = Number(form.throughputReserveAtPct)

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
  if (
    form.throughputBurnBelowPct.trim() === '' ||
    !Number.isFinite(burnBelow) ||
    burnBelow < 0 ||
    burnBelow > 100
  ) {
    errors.throughputBurnBelowPct = '需在 0-100 之间'
  }
  if (
    form.throughputReserveAtPct.trim() === '' ||
    !Number.isFinite(reserveAt) ||
    reserveAt < 0 ||
    reserveAt > 100
  ) {
    errors.throughputReserveAtPct = '需在 0-100 之间'
  }
  return errors
}

function isThroughputLike(profile: SchedulingProfile): boolean {
  return profile === 'throughput' || profile === 'highConcurrency'
}

/** 当前值是否已偏离所选取向的推荐值——偏离即代表这不再是「取向」的默认样子 */
function fieldChanged(formValue: string, presetValue: number | undefined): boolean {
  if (presetValue === undefined) return false
  const n = Number(formValue)
  return Number.isFinite(n) && n !== presetValue
}

function ChangedBadge({ changed }: { changed: boolean }) {
  if (!changed) return null
  return (
    <Badge variant="warning" className="ml-1.5 align-middle">
      已改
    </Badge>
  )
}

function CaveatBox({ children }: { children: React.ReactNode }) {
  return (
    <div className="flex items-start gap-2 rounded-md border border-amber-500/40 bg-amber-500/10 p-3 text-[13px] text-amber-700 dark:text-amber-400">
      <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0" />
      <div className="space-y-1">{children}</div>
    </div>
  )
}

function formatCooldownSecs(secs: number): string {
  if (secs >= 60 && secs % 60 === 0) return `${secs / 60} 分钟`
  return `${secs} 秒`
}

/** 把 max-throughput 接口 applied[]/failed[] 里的 setting 键翻成运营看得懂的名字 */
function settingLabel(setting: string): string {
  const labels: Record<string, string> = {
    loadBalancingMode: '负载均衡模式',
    accountRpmLimitEnabled: '单账号 RPM 上限',
    accountRpmLimit: '单账号 RPM 上限',
    accountThrottleCooldownSecs: '限流冷却',
    selfHeal: '自愈治理',
    'scheduling.profile': '调度取向',
  }
  return labels[setting] ?? setting
}

/** 把 applied[]/failed[] 里的 value（任意 JSON）渲染成一行人话 */
function formatSettingValue(
  setting: string,
  value: unknown,
  presets: SchedulingProfilePreset[],
): string {
  switch (setting) {
    case 'scheduling.profile':
      if (typeof value === 'string') {
        return presets.find((p) => p.profile === value)?.label ?? value
      }
      break
    case 'loadBalancingMode':
      if (value === 'priority') return '优先级模式'
      if (value === 'balanced') return '均衡负载模式'
      break
    case 'accountRpmLimitEnabled':
      return value ? '开启' : '关闭'
    case 'accountThrottleCooldownSecs':
      if (typeof value === 'number') return formatCooldownSecs(value)
      break
    case 'accountRpmLimit':
      if (value && typeof value === 'object') {
        const v = value as Record<string, unknown>
        return `每号 ${v.perCredential}/分 · 共 ${v.enterpriseCredentials} 个企业凭据 · 池上限 ${v.poolCeiling}/分`
      }
      break
    case 'selfHeal':
      if (value && typeof value === 'object') {
        const v = value as Record<string, unknown>
        return `${v.enabled ? '开启' : '关闭'} · 间隔 ${v.minIntervalSecs} 秒`
      }
      break
  }
  if (value === null || value === undefined) return '—'
  if (typeof value === 'boolean') return value ? '开启' : '关闭'
  if (typeof value === 'string' || typeof value === 'number') return String(value)
  return JSON.stringify(value)
}

/**
 * 调度策略面板：调度取向是唯一的入口——选中一个取向后，下面的阈值、运行时设置
 * 摘要、吞吐预估都跟着联动展示；数值仍然可编辑（取向只是起点，不是锁死），
 * 一旦编辑偏离所选取向的推荐值就标「已改」。
 *
 * 「保存」只写 SchedulingConfig 表（阈值 + 取向）；负载均衡模式 / RPM 上限 /
 * 限流冷却这些运行时设置只有「一键应用整套配置」才会真正改动——这正是运营
 * 反馈过的联动缺口：点了「提升吞吐」，priority 模式没跟着切成 balanced，
 * 流量还是全糊在一个账号上。
 *
 * 表单态只在配置首次加载时从服务端同步一次（`form === null` 时），此后的后台刷新
 * 不会覆盖操作员正在编辑但还未保存的输入。
 */
export function SchedulingPanel() {
  const { data: config, isLoading } = useSchedulingConfig()
  const { data: presetsResp, isLoading: presetsLoading } = useSchedulingPresets()
  const { mutate: saveConfig, mutateAsync: saveConfigAsync, isPending: saving } =
    useSetSchedulingConfig()
  const { mutate: runOnce, isPending: running } = useRunScheduling()
  const { mutateAsync: applyMaxThroughputAsync, isPending: applying } = useApplyMaxThroughput()
  const confirm = useConfirm()

  const [form, setForm] = useState<SchedulingForm | null>(null)
  const [runResult, setRunResult] = useState<SchedulingRunResult | null>(null)
  const [maxThroughputResult, setMaxThroughputResult] = useState<MaxThroughputResult | null>(null)
  const [targetRpm, setTargetRpm] = useState('')

  const throughputMode = form ? isThroughputLike(form.profile) : false
  const { data: estimateResp, isLoading: estimateLoading } = useThroughputEstimate(throughputMode)

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

  const presets = presetsResp?.presets ?? []

  // 首次拉取配置/预设完成前，仅渲染标题与占位文案；不提前渲染表单避免 null 态分支
  if (!form || presetsResp === undefined) {
    return (
      <Card className="mt-5 sm:mt-6">
        <CardHeader className="pb-3">
          <CardTitle>调度策略</CardTitle>
          <CardDescription>
            优先级约定：小于 50 优先消耗 · 50 正常（新建凭据默认）· 大于 50 退居二线（仍参与故障转移与负载均衡，但不是主力输出）
          </CardDescription>
        </CardHeader>
        <CardContent className="pt-0">
          <div className="text-sm text-muted-foreground">
            {isLoading || presetsLoading ? '加载中…' : '暂无数据'}
          </div>
        </CardContent>
      </Card>
    )
  }

  const errors = validate(form)
  const hasErrors = Object.keys(errors).length > 0
  const fieldsDisabled = saving || applying || !form.enabled
  const matchedPreset = presets.find((p) => p.profile === form.profile)

  const handleSelectProfile = (opt: SchedulingProfilePreset) => {
    setForm((f) =>
      f
        ? {
            ...f,
            profile: opt.profile,
            demoteThresholdPct: String(opt.demoteThresholdPct),
            demoteTo: String(opt.demoteTo),
            minTopTier: String(opt.minTopTier),
            throughputBurnBelowPct: String(opt.throughputBurnBelowPct),
            throughputReserveAtPct: String(opt.throughputReserveAtPct),
          }
        : f,
    )
  }

  const handleSave = () => {
    if (hasErrors) return
    const payload = formToPayload(form)
    saveConfig(payload, {
      onSuccess: (saved) => {
        setForm(configToForm(saved))
        toast.success('调度策略已保存')
      },
      onError: (err) => toast.error(`保存失败: ${extractErrorMessage(err)}`),
    })
  }

  const buildApplyDescription = (preview: MaxThroughputResult, willSaveThresholds: boolean) => (
    <div className="space-y-2">
      {willSaveThresholds && (
        <p className="text-muted-foreground">会先保存当前显示的阈值，再切换以下运行时设置：</p>
      )}
      <ul className="space-y-1 text-sm">
        {preview.applied.map((item, i) => (
          <li key={`${item.setting}-${i}`} className="flex items-baseline justify-between gap-3">
            <span className="text-muted-foreground">{settingLabel(item.setting)}</span>
            <span className="font-medium tabular-nums">
              {formatSettingValue(item.setting, item.value, presets)}
            </span>
          </li>
        ))}
      </ul>
      <p className="text-xs text-muted-foreground">{preview.note}</p>
    </div>
  )

  const handleApply = async () => {
    if (hasErrors) {
      toast.error('请先修正标红的字段再应用')
      return
    }
    const rpmTrim = targetRpm.trim()
    let rpm: number | undefined
    if (rpmTrim !== '') {
      rpm = Number(rpmTrim)
      if (!Number.isFinite(rpm) || rpm <= 0) {
        toast.error('目标 RPM 需为正数')
        return
      }
    }
    const baseParams: MaxThroughputParams = {
      profile: form.profile === 'highConcurrency' ? 'highConcurrency' : undefined,
      targetRpm: rpm,
    }

    let preview: MaxThroughputResult
    try {
      preview = await applyMaxThroughputAsync({ ...baseParams, dryRun: true })
    } catch (err) {
      toast.error(`预检失败：${extractErrorMessage(err)}`)
      return
    }

    const ok = await confirm({
      title: '确认应用整套配置？',
      description: buildApplyDescription(preview, true),
      confirmText: '确认应用',
    })
    if (!ok) return

    try {
      await saveConfigAsync(formToPayload(form))
      const result = await applyMaxThroughputAsync({ ...baseParams, dryRun: false })
      setMaxThroughputResult(result)
      setForm((f) => (f ? { ...f, enabled: true } : f))
      if (result.failed.length > 0) {
        toast.error(`已应用，但有 ${result.failed.length} 项失败，见下方详情`)
      } else {
        toast.success('已应用整套配置')
      }
    } catch (err) {
      toast.error(`应用失败：${extractErrorMessage(err)}`)
    }
  }

  const handleRestore = async () => {
    let preview: MaxThroughputResult
    try {
      preview = await applyMaxThroughputAsync({ restore: true, dryRun: true })
    } catch (err) {
      toast.error(`预检失败：${extractErrorMessage(err)}`)
      return
    }

    const ok = await confirm({
      title: '恢复默认（风险控制模式）？',
      description: buildApplyDescription(preview, false),
      confirmText: '恢复默认',
      destructive: true,
    })
    if (!ok) return

    try {
      const result = await applyMaxThroughputAsync({ restore: true, dryRun: false })
      setMaxThroughputResult(result)
      setForm((f) => (f ? { ...f, profile: 'manual', enabled: true } : f))
      toast.success('已恢复默认（风险控制模式）')
    } catch (err) {
      toast.error(`恢复失败：${extractErrorMessage(err)}`)
    }
  }

  const estimate = estimateResp?.estimate

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

        {/* 调度取向：唯一入口，选中后下面全部联动 */}
        <div className={form.enabled ? '' : 'opacity-60'}>
          <div className="mb-2 text-sm font-medium">调度取向</div>
          <TooltipProvider delayDuration={150}>
            <div className="grid grid-cols-2 gap-1.5 sm:flex sm:flex-wrap">
              {presets.map((opt) => (
                <Tooltip key={opt.profile}>
                  <TooltipTrigger asChild>
                    <Button
                      type="button"
                      size="sm"
                      variant={form.profile === opt.profile ? 'default' : 'outline'}
                      disabled={fieldsDisabled}
                      onClick={() => handleSelectProfile(opt)}
                    >
                      {opt.label}
                    </Button>
                  </TooltipTrigger>
                  <TooltipContent side="bottom">{opt.summary}</TooltipContent>
                </Tooltip>
              ))}
            </div>
          </TooltipProvider>
        </div>

        {/* 数值参数：取向的推荐值可编辑，偏离即标「已改」 */}
        <div
          className={cn(
            'grid grid-cols-1 gap-4 sm:grid-cols-3',
            !form.enabled && 'opacity-60',
          )}
        >
          <div>
            <label className="text-sm font-medium" htmlFor="scheduling-threshold">
              降级阈值（%）
              <ChangedBadge
                changed={fieldChanged(form.demoteThresholdPct, matchedPreset?.demoteThresholdPct)}
              />
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

          <div>
            <label className="text-sm font-medium" htmlFor="scheduling-demote-to">
              降级到优先级
              <ChangedBadge changed={fieldChanged(form.demoteTo, matchedPreset?.demoteTo)} />
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

          <div>
            <label className="text-sm font-medium" htmlFor="scheduling-min-top-tier">
              首选层最少保留
              <ChangedBadge changed={fieldChanged(form.minTopTier, matchedPreset?.minTopTier)} />
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

          {throughputMode && (
            <>
              <div>
                <label className="text-sm font-medium" htmlFor="scheduling-burn-below">
                  前排档阈值（%）
                  <ChangedBadge
                    changed={fieldChanged(
                      form.throughputBurnBelowPct,
                      matchedPreset?.throughputBurnBelowPct,
                    )}
                  />
                </label>
                <Input
                  id="scheduling-burn-below"
                  type="number"
                  min={0}
                  max={100}
                  value={form.throughputBurnBelowPct}
                  disabled={fieldsDisabled}
                  onChange={(e) =>
                    setForm((f) => (f ? { ...f, throughputBurnBelowPct: e.target.value } : f))
                  }
                  className="mt-1.5"
                />
                <p className="mt-1 text-xs text-muted-foreground">
                  用量低于该值的凭据进前排，尽情烧
                </p>
                {errors.throughputBurnBelowPct && (
                  <p className="mt-1 text-xs text-destructive">{errors.throughputBurnBelowPct}</p>
                )}
              </div>

              <div>
                <label className="text-sm font-medium" htmlFor="scheduling-reserve-at">
                  溢出储备阈值（%）
                  <ChangedBadge
                    changed={fieldChanged(
                      form.throughputReserveAtPct,
                      matchedPreset?.throughputReserveAtPct,
                    )}
                  />
                </label>
                <Input
                  id="scheduling-reserve-at"
                  type="number"
                  min={0}
                  max={100}
                  value={form.throughputReserveAtPct}
                  disabled={fieldsDisabled}
                  onChange={(e) =>
                    setForm((f) => (f ? { ...f, throughputReserveAtPct: e.target.value } : f))
                  }
                  className="mt-1.5"
                />
                <p className="mt-1 text-xs text-muted-foreground">
                  用量达到该值的凭据退到溢出储备档，只接前排 429 之后的溢出
                </p>
                {errors.throughputReserveAtPct && (
                  <p className="mt-1 text-xs text-destructive">{errors.throughputReserveAtPct}</p>
                )}
              </div>
            </>
          )}
        </div>

        {/* 运行时设置摘要：这个取向也要求负载均衡模式 / RPM 上限 / 限流冷却 / 429 换号
            跟着改，只是「保存」按钮不会碰这几项——这正是联动缺口所在，只读展示 + 用
            「一键应用」才能真正落地。 */}
        {matchedPreset && (
          <div>
            <div className="mb-2 text-sm font-medium">
              「{matchedPreset.label}」还要求这些运行时设置
            </div>
            <p className="mb-2 text-sm text-muted-foreground">{matchedPreset.summary}</p>
            <div className="overflow-x-auto rounded-xl border border-border/60">
              <table className="w-full min-w-[560px] text-sm">
                <thead>
                  <tr className="border-b border-border/60 text-muted-foreground">
                    <th className={TH_LABEL}>设置项</th>
                    <th className={TH_LABEL}>负载均衡模式</th>
                    <th className={TH_LABEL}>单账号 RPM 上限</th>
                    <th className={TH_NUM}>限流冷却</th>
                    <th className={TH_LABEL}>普通限流是否换号</th>
                  </tr>
                </thead>
                <tbody>
                  <tr>
                    <td className={TD_LABEL}>该取向要求的值</td>
                    <td className="py-2.5 pl-3">
                      {matchedPreset.loadBalancingMode === 'priority' ? '优先级模式' : '均衡负载模式'}
                    </td>
                    <td className="py-2.5 pl-3">
                      {matchedPreset.accountRpmLimitEnabled ? '开启' : '关闭'}
                    </td>
                    <td className={TD_NUM}>{formatCooldownSecs(matchedPreset.throttleCooldownSecs)}</td>
                    <td className="py-2.5 pl-3">
                      {matchedPreset.spillOnRateLimit ? '是（换号）' : '否（原地重试）'}
                    </td>
                  </tr>
                </tbody>
              </table>
            </div>
            <div className="mt-2">
              <CaveatBox>{matchedPreset.caveat}</CaveatBox>
            </div>
          </div>
        )}

        {/* 吞吐预估：只在提升吞吐 / 高并发时展示——打开这两种取向前先看代价 */}
        {throughputMode && (
          <div>
            <div className="mb-2 text-sm font-medium">吞吐预估</div>
            {estimateLoading || !estimate ? (
              <div className="text-sm text-muted-foreground">预估加载中…</div>
            ) : (
              <>
                <div className="overflow-x-auto rounded-xl border border-border/60">
                  <table className="w-full min-w-[640px] text-sm">
                    <thead>
                      <tr className="border-b border-border/60 text-muted-foreground">
                        <th className={TH_LABEL}>分档 / 并发</th>
                        <th className={TH_NUM}>前排</th>
                        <th className={TH_NUM}>中间</th>
                        <th className={TH_NUM}>溢出储备</th>
                        <th className={TH_NUM}>当前并发</th>
                        <th className={TH_NUM}>预估并发</th>
                        <th className={TH_NUM}>可持续 TPM</th>
                      </tr>
                    </thead>
                    <tbody>
                      <tr>
                        <td className={TD_LABEL}>凭据数 / 请求并发</td>
                        <td className={TD_NUM}>{estimate.frontTier}</td>
                        <td className={TD_NUM}>{estimate.midTier}</td>
                        <td className={TD_NUM}>{estimate.reserveTier}</td>
                        <td className={TD_NUM}>{estimate.currentConcurrency}</td>
                        <td className={TD_NUM_STRONG}>
                          {estimate.estimatedConcurrency}
                          <span className="ml-1 font-normal text-muted-foreground">
                            （×{estimate.concurrencyGain.toFixed(1)}）
                          </span>
                        </td>
                        <td className={TD_NUM}>
                          {estimate.sustainableTpm != null
                            ? estimate.sustainableTpm.toLocaleString('en-US')
                            : '—'}
                        </td>
                      </tr>
                    </tbody>
                  </table>
                </div>
                <p className="mt-2 text-xs tabular-nums text-muted-foreground">
                  可用额度 {estimate.usableCredits.toFixed(1)} credit
                  {estimate.runwayHours != null &&
                    ` · 按当前烧速还能撑 ${estimate.runwayHours.toFixed(1)} 小时`}
                  {estimate.hoursToReset != null &&
                    ` · 距额度重置 ${estimate.hoursToReset.toFixed(1)} 小时`}
                </p>
                {estimate.notes.length > 0 && (
                  <div className="mt-2">
                    <CaveatBox>
                      {estimate.notes.map((note, i) => (
                        <p key={i}>{note}</p>
                      ))}
                    </CaveatBox>
                  </div>
                )}
                {estimateResp?.caveat && (
                  <p className="mt-1.5 text-xs italic text-muted-foreground">
                    {estimateResp.caveat}
                  </p>
                )}
              </>
            )}
          </div>
        )}

        {/* 一键应用：吞吐类取向专属的「整套配置」入口，除阈值外还会真正切换
            负载均衡模式 / RPM 上限 / 限流冷却——不点这个，选取向只是改了展示。 */}
        {throughputMode && (
          <div className="rounded-xl border border-border/60 bg-secondary/20 p-3.5">
            <div className="mb-2 text-sm font-medium">一键应用整套配置</div>
            <p className="mb-3 text-xs text-muted-foreground">
              会先保存上面显示的阈值，再把负载均衡模式、单账号 RPM 上限、限流冷却一并切到「
              {matchedPreset?.label}」要求的值，并立即执行一轮调度。执行前会先弹出确认，列出全部改动。
            </p>
            <div className="flex flex-wrap items-end gap-2">
              <div className="w-40">
                <label className="text-xs font-medium text-muted-foreground" htmlFor="scheduling-target-rpm">
                  目标 RPM（可选）
                </label>
                <Input
                  id="scheduling-target-rpm"
                  type="number"
                  min={1}
                  placeholder="不填则关闭 RPM 上限"
                  value={targetRpm}
                  disabled={applying}
                  onChange={(e) => setTargetRpm(e.target.value)}
                  className="mt-1"
                />
              </div>
              <Button onClick={handleApply} disabled={applying || hasErrors}>
                {applying ? '应用中…' : '一键应用整套配置'}
              </Button>
              <Button variant="outline" onClick={handleRestore} disabled={applying}>
                恢复默认
              </Button>
            </div>

            {maxThroughputResult && (
              <div className="mt-3 rounded-lg border border-border/60 bg-background/60 px-3 py-2.5">
                <div className="mb-1.5 flex items-center gap-2">
                  <span className="text-sm font-medium">
                    {maxThroughputResult.mode === 'maxThroughput' ? '已切到最高吞吐' : '已恢复风险控制模式'}
                  </span>
                  <Badge variant="secondary">{maxThroughputResult.priorityChanges} 个凭据调优先级</Badge>
                  {maxThroughputResult.failed.length > 0 && (
                    <Badge variant="destructive">{maxThroughputResult.failed.length} 项失败</Badge>
                  )}
                </div>
                <ul className="space-y-1 text-xs text-muted-foreground">
                  {maxThroughputResult.applied.map((item, i) => (
                    <li key={`${item.setting}-${i}`} className="tabular-nums">
                      {settingLabel(item.setting)}: {formatSettingValue(item.setting, item.value, presets)}
                    </li>
                  ))}
                </ul>
                {maxThroughputResult.failed.length > 0 && (
                  <ul className="mt-1.5 space-y-1 text-xs text-destructive">
                    {maxThroughputResult.failed.map((item, i) => (
                      <li key={`${item.setting}-${i}`}>
                        {settingLabel(item.setting)}: {item.error}
                      </li>
                    ))}
                  </ul>
                )}
                <p className="mt-1.5 text-xs text-muted-foreground">{maxThroughputResult.note}</p>
              </div>
            )}
          </div>
        )}

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
