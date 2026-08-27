import { useEffect, useMemo, useState } from 'react'
import { HardDrive, Info } from 'lucide-react'
import { toast } from 'sonner'
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
} from '@/components/ui/dialog'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Switch } from '@/components/ui/switch'
import { Checkbox } from '@/components/ui/checkbox'
import { Tooltip, TooltipContent, TooltipProvider, TooltipTrigger } from '@/components/ui/tooltip'
import { usePromptCaptureConfig, useSetPromptCaptureConfig } from '@/hooks/use-credentials'
import { useModelCostAnalysis } from '@/hooks/use-stats'
import { currentMonthValue } from '@/components/month-picker'
import { WarningBanner } from '@/components/warning-banner'
import {
  beijingDayOfMonth,
  extractErrorMessage,
  formatBeijingDateTime,
  formatBytes,
  formatNumber,
} from '@/lib/utils'

const MIN_RETENTION_DAYS = 1
const MAX_RETENTION_DAYS = 365

function Stat({
  label,
  value,
  caption,
}: {
  label: React.ReactNode
  value: string
  caption?: string
}) {
  return (
    <div>
      <div className="text-[11px] font-medium text-muted-foreground sm:text-[13px]">{label}</div>
      <div className="mt-1.5 text-xl font-semibold tracking-tight tabular-nums sm:mt-2 sm:text-2xl">
        {value}
      </div>
      {caption && <div className="mt-1 text-[11px] text-muted-foreground">{caption}</div>}
    </div>
  )
}

/**
 * 从后端 `sizing` 文案里粗略抠出"月请求量 → gzip 后体积"的基线比例。
 *
 * 这段文案是给人读的自然语言，不是结构化字段——抠数字纯属兜底：只有在
 * 留存还从未产生过任何记录（`stats.count === 0`）时才会用到它来估算存储
 * 投影。抠不出来就返回 null，调用方据此只展示原文，不编造数字。
 */
function parseSizingBaseline(
  sizing: string,
): { requestsPerMonth: number; gzipBytes: number } | null {
  const reqMatch = sizing.match(/([\d.]+)\s*万次请求/)
  const gzipMatch = sizing.match(/gzip[^0-9]*([\d.]+)\s*GB/i)
  if (!reqMatch || !gzipMatch) return null
  const requestsPerMonth = parseFloat(reqMatch[1]) * 10000
  const gzipBytes = parseFloat(gzipMatch[1]) * 1e9
  if (!Number.isFinite(requestsPerMonth) || requestsPerMonth <= 0) return null
  if (!Number.isFinite(gzipBytes) || gzipBytes <= 0) return null
  return { requestsPerMonth, gzipBytes }
}

interface PromptCaptureDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
}

/**
 * 「原始请求体留存」设置弹窗：开关 + 保留天数 + 容量统计 + 30 天存储投影。
 *
 * 三条不能省的语境，操作员看不到就会踩坑：
 * 1. `traces.db` 从来不存请求体——开启这个开关是从这一刻起才开始记录，
 *    此前已发生的历史请求没有原文可查，这一点必须在开关旁边说清楚。
 * 2. 开关是热切的：PUT 成功即生效，不需要重启（后端返回的 message 会说明具体行为）；
 *    这条提示在保存成功后要用后端原话显眼地弹出来，不能只在 note 里带一句。
 * 3. 留存的是客户未脱敏的原始请求内容，属于有合同分量的决定——开关本身
 *    不直接触发保存，要看到红色警示 + 勾选确认后「保存」按钮才会真的提交。
 */
export function PromptCaptureDialog({ open, onOpenChange }: PromptCaptureDialogProps) {
  const { data: config, isLoading } = usePromptCaptureConfig()
  const { mutate: save, isPending: saving } = useSetPromptCaptureConfig()

  const [initialized, setInitialized] = useState(false)
  const [pendingEnabled, setPendingEnabled] = useState(false)
  const [retentionInput, setRetentionInput] = useState('')
  const [confirmChecked, setConfirmChecked] = useState(false)
  const [restartNotice, setRestartNotice] = useState<string | null>(null)

  useEffect(() => {
    if (!open) {
      setInitialized(false)
      setConfirmChecked(false)
      setRestartNotice(null)
      return
    }
    if (!initialized && config) {
      setPendingEnabled(config.enabled)
      setRetentionInput(String(config.retentionDays))
      setInitialized(true)
    }
  }, [open, config, initialized])

  const month = useMemo(() => currentMonthValue(), [])
  const { data: costData } = useModelCostAnalysis(month)
  const monthCalls = useMemo(
    () => costData?.models.reduce((s, m) => s + m.calls, 0) ?? null,
    [costData],
  )

  const projection = useMemo(() => {
    if (!config || monthCalls == null || monthCalls <= 0) return null
    const dayOfMonth = Math.max(1, beijingDayOfMonth())
    const projectedRequests30d = (monthCalls / dayOfMonth) * 30

    let bytesPerRequest: number | null = null
    let basis: 'measured' | 'baseline' = 'measured'
    if (config.stats.count > 0 && config.stats.fileBytes > 0) {
      bytesPerRequest = config.stats.fileBytes / config.stats.count
    } else {
      const baseline = parseSizingBaseline(config.sizing)
      if (baseline) {
        bytesPerRequest = baseline.gzipBytes / baseline.requestsPerMonth
        basis = 'baseline'
      }
    }
    if (bytesPerRequest == null) return null
    return {
      basis,
      projectedRequests30d,
      projectedBytes30d: bytesPerRequest * projectedRequests30d,
    }
  }, [config, monthCalls])

  const handleToggle = (v: boolean) => {
    setPendingEnabled(v)
    if (v) setConfirmChecked(false)
  }

  const handleSave = () => {
    const days = parseInt(retentionInput, 10)
    if (Number.isNaN(days) || days < MIN_RETENTION_DAYS || days > MAX_RETENTION_DAYS) {
      toast.error(`保留天数需在 ${MIN_RETENTION_DAYS}-${MAX_RETENTION_DAYS} 之间`)
      return
    }
    if (pendingEnabled && !confirmChecked) {
      toast.error('请先勾选确认对客条款覆盖数据留存')
      return
    }
    save(
      { enabled: pendingEnabled, retentionDays: days },
      {
        onSuccess: (res) => {
          setRestartNotice(res.message)
          toast.success('已保存')
        },
        onError: (err) => toast.error(`保存失败：${extractErrorMessage(err)}`),
      },
    )
  }

  const compressionRatio =
    config && config.stats.rawBytes > 0 && config.stats.fileBytes > 0
      ? config.stats.rawBytes / config.stats.fileBytes
      : null

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-2xl max-h-[85vh] overflow-y-auto">
        <TooltipProvider delayDuration={150}>
          <DialogHeader>
            <DialogTitle className="flex items-center gap-2">
              <HardDrive className="h-4 w-4" />
              原始请求体留存
            </DialogTitle>
            <DialogDescription>
              控制是否把客户请求的原始 JSON 记录到独立的 prompts.db，之后可在「请求日志」的展开详情里查看完整请求内容。
            </DialogDescription>
          </DialogHeader>

          <div className="rounded-md border border-border/60 bg-secondary/20 p-3 text-[12px] leading-relaxed text-muted-foreground">
            <span className="font-medium text-foreground">traces.db 从来没有存过请求体。</span>
            {' '}
            开启这个开关，只从保存生效的那一刻起才开始记录新产生的请求——此前已经发生的请求，原文无法找回。
          </div>

          {isLoading ? (
            <div className="flex h-16 items-center justify-center text-[13px] text-muted-foreground">
              加载中…
            </div>
          ) : config ? (
            <>
              <div className="space-y-3 rounded-md border border-border/60 p-4">
                <div className="flex items-center justify-between gap-3">
                  <div className="min-w-0">
                    <div className="text-sm font-medium">
                      {pendingEnabled ? '开启' : '关闭'}
                    </div>
                    <div className="text-[12px] leading-snug text-muted-foreground">
                      {pendingEnabled
                        ? '记录每次请求的原始 JSON 请求体（不含任何请求头）'
                        : '不记录请求体，仅保留链路元数据（时间/模型/耗时/计费等）'}
                    </div>
                  </div>
                  <Switch checked={pendingEnabled} disabled={saving} onCheckedChange={handleToggle} />
                </div>

                <div className="flex items-center gap-2">
                  <span className="shrink-0 text-[12px] text-muted-foreground">保留天数</span>
                  <Input
                    type="number"
                    min={MIN_RETENTION_DAYS}
                    max={MAX_RETENTION_DAYS}
                    value={retentionInput}
                    onChange={(e) => setRetentionInput(e.target.value)}
                    disabled={saving}
                    className="h-8 w-24 text-xs"
                  />
                  <span className="text-[12px] text-muted-foreground">
                    天（{MIN_RETENTION_DAYS}-{MAX_RETENTION_DAYS}）
                  </span>
                </div>
              </div>

              {pendingEnabled && (
                <WarningBanner tone="red" title="留存的是客户的原始请求内容，未脱敏">
                  <p className="mb-2 text-[11px] leading-relaxed text-muted-foreground">
                    开启前请确认对客条款覆盖数据留存。
                  </p>
                  <label className="flex items-start gap-2 text-[12px]">
                    <Checkbox
                      checked={confirmChecked}
                      disabled={saving}
                      onCheckedChange={(v) => setConfirmChecked(v === true)}
                      className="mt-0.5"
                    />
                    <span>我已确认对客条款覆盖数据留存，可以保存本次改动</span>
                  </label>
                </WarningBanner>
              )}

              {/* 后端已改成热切，PUT 成功即生效。仍读后端返回的 message
                  而不是写死文案：生效方式是后端的实现细节，它变了这里应该跟着变。 */}
              {restartNotice && (
                <WarningBanner tone="emerald" title="已生效">
                  <p className="text-[12px] leading-relaxed">{restartNotice}</p>
                </WarningBanner>
              )}

              <div className="flex justify-end">
                <Button size="sm" onClick={handleSave} disabled={saving}>
                  {saving ? '保存中…' : '保存'}
                </Button>
              </div>

              <div className="grid grid-cols-2 gap-4 border-t border-border/40 pt-4 sm:grid-cols-4">
                <Stat label="记录数" value={formatNumber(config.stats.count)} />
                <Stat label="未压缩合计" value={formatBytes(config.stats.rawBytes)} />
                <Stat label="库文件实际大小" value={formatBytes(config.stats.fileBytes)} />
                <Stat
                  label={
                    <span className="inline-flex items-center gap-1">
                      压缩比
                      <Tooltip>
                        <TooltipTrigger asChild>
                          <Info className="h-3 w-3 cursor-help text-muted-foreground" />
                        </TooltipTrigger>
                        <TooltipContent>
                          未压缩合计 ÷ 库文件实际大小。这个比值越高，说明请求体里重复内容（系统提示词、代码上下文等）越多，
                          也是全量留存在磁盘成本上负担得起的原因。
                        </TooltipContent>
                      </Tooltip>
                    </span>
                  }
                  value={compressionRatio != null ? `${compressionRatio.toFixed(1)}×` : '—'}
                />
              </div>

              <div className="text-[12px] text-muted-foreground">
                最早记录时间：
                {config.stats.oldestTsEpoch != null
                  ? `${formatBeijingDateTime(config.stats.oldestTsEpoch * 1000)}（北京时间）`
                  : '暂无记录'}
              </div>

              <div className="rounded-md border border-border/60 bg-secondary/10 p-3 text-[12px] leading-relaxed">
                <div className="mb-1 font-medium">存储投影</div>
                {projection ? (
                  <p>
                    按本月至今日均请求量线性外推 30 天（约 {formatNumber(projection.projectedRequests30d)} 次请求），
                    预计留存体积约{' '}
                    <span className="font-semibold tabular-nums">
                      {formatBytes(projection.projectedBytes30d)}
                    </span>
                    {projection.basis === 'measured' ? '（按当前实测压缩率估算）' : '（按后端基线换算，粗略参考）'}。
                  </p>
                ) : (
                  <p className="text-muted-foreground">暂无足够数据估算精确投影，参考基线：{config.sizing}</p>
                )}
                {projection && <p className="mt-1 text-muted-foreground">基线：{config.sizing}</p>}
              </div>

              <p className="text-[11px] text-muted-foreground">{config.note}</p>
            </>
          ) : null}
        </TooltipProvider>
      </DialogContent>
    </Dialog>
  )
}
