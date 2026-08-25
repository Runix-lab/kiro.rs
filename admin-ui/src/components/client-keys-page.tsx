import { useState } from 'react'
import { toast } from 'sonner'
import {
  Plus, KeyRound, Trash2, Copy, Eye, EyeOff, Power, RotateCcw, Pencil, RefreshCw, Coins,
} from 'lucide-react'
import { Card, CardContent } from '@/components/ui/card'
import { Button } from '@/components/ui/button'
import { Badge } from '@/components/ui/badge'
import { Input } from '@/components/ui/input'
import {
  DropdownMenu, DropdownMenuTrigger, DropdownMenuContent, DropdownMenuItem,
} from '@/components/ui/dropdown-menu'
import {
  Dialog, DialogContent, DialogHeader, DialogTitle, DialogFooter, DialogDescription,
} from '@/components/ui/dialog'
import {
  useClientKeys, useCreateClientKey, useDeleteClientKey,
  useSetClientKeyDisabled, useResetClientKeyStats, useUpdateClientKey,
  useRotateClientKey,
} from '@/hooks/use-client-keys'
import { useGroupOptions } from '@/hooks/use-groups'
import { GroupSingleSelect } from '@/components/group-select'
import type { ClientKeyItem, CreateClientKeyResponse, UpdateClientKeyRequest } from '@/types/api'
import { extractErrorMessage, formatDiscount, formatUsd } from '@/lib/utils'
import { useConfirm } from '@/components/ui/confirm-dialog'

type PricingMode = 'perCredit' | 'discount' | 'clear'

function formatTokens(n: number): string {
  if (n >= 1_000_000) return (n / 1_000_000).toFixed(2) + 'M'
  if (n >= 1_000) return (n / 1_000).toFixed(1) + 'K'
  return n.toString()
}

function formatRelative(ts?: string): string {
  if (!ts) return '从未使用'
  const t = new Date(ts).getTime()
  const diff = Date.now() - t
  if (diff < 60_000) return '刚刚'
  if (diff < 3600_000) return `${Math.floor(diff / 60_000)} 分钟前`
  if (diff < 86400_000) return `${Math.floor(diff / 3600_000)} 小时前`
  return `${Math.floor(diff / 86400_000)} 天前`
}

/** 「对客定价」列的紧凑展示：单价 / 折扣 / 未定价三选一。 */
function PricingCell({ item }: { item: ClientKeyItem }) {
  if (item.billingPricePerCredit) {
    return (
      <span className="text-[12px] tabular-nums">{formatUsd(item.billingPricePerCredit)}/credit</span>
    )
  }
  if (item.billingDiscount) {
    return <span className="text-[12px] tabular-nums">{formatDiscount(item.billingDiscount)}</span>
  }
  return <span className="text-[12px] text-muted-foreground">未定价</span>
}

export function ClientKeysPage() {
  const { data, isLoading } = useClientKeys()
  // 已注册分组列表（来自 groups.json 注册表，与凭据的 groups 字段解耦）
  const groupOptions = useGroupOptions()
  const createKey = useCreateClientKey()
  const deleteKey = useDeleteClientKey()
  const setDisabled = useSetClientKeyDisabled()
  const resetStats = useResetClientKeyStats()
  const updateKey = useUpdateClientKey()
  const rotateKey = useRotateClientKey()
  const confirm = useConfirm()

  const [createOpen, setCreateOpen] = useState(false)
  const [createName, setCreateName] = useState('')
  const [createDesc, setCreateDesc] = useState('')
  const [createGroup, setCreateGroup] = useState('')
  const [createdKey, setCreatedKey] = useState<CreateClientKeyResponse | null>(null)
  const [showCreatedPlain, setShowCreatedPlain] = useState(true)

  const [editOpen, setEditOpen] = useState(false)
  const [editTarget, setEditTarget] = useState<ClientKeyItem | null>(null)
  const [editName, setEditName] = useState('')
  const [editDesc, setEditDesc] = useState('')
  const [editGroup, setEditGroup] = useState('')

  const [pricingOpen, setPricingOpen] = useState(false)
  const [pricingTarget, setPricingTarget] = useState<ClientKeyItem | null>(null)
  const [pricingMode, setPricingMode] = useState<PricingMode>('perCredit')
  const [pricingPerCredit, setPricingPerCredit] = useState('')
  const [pricingDiscount, setPricingDiscount] = useState('')

  const handleCreate = async (e: React.FormEvent) => {
    e.preventDefault()
    const name = createName.trim()
    if (!name) {
      toast.error('请填写名称')
      return
    }
    try {
      const res = await createKey.mutateAsync({
        name,
        description: createDesc.trim() || undefined,
        group: createGroup.trim() || undefined,
      })
      setCreatedKey(res)
      setCreateOpen(false)
      setCreateName('')
      setCreateDesc('')
      setCreateGroup('')
      setShowCreatedPlain(true)
    } catch (err) {
      toast.error('创建失败：' + extractErrorMessage(err))
    }
  }

  const handleDelete = async (item: ClientKeyItem) => {
    if (
      !(await confirm({
        title: '确认删除 Key',
        description: `确认删除 Key "${item.name}"？此操作无法撤销。`,
        confirmText: '确认删除',
        destructive: true,
      }))
    )
      return
    try {
      await deleteKey.mutateAsync(item.id)
      toast.success(`已删除 Key #${item.id}`)
    } catch (err) {
      toast.error('删除失败：' + extractErrorMessage(err))
    }
  }

  const handleToggleDisabled = async (item: ClientKeyItem) => {
    try {
      await setDisabled.mutateAsync({ id: item.id, disabled: !item.disabled })
      toast.success(item.disabled ? '已启用' : '已禁用')
    } catch (err) {
      toast.error('操作失败：' + extractErrorMessage(err))
    }
  }

  const handleReset = async (item: ClientKeyItem) => {
    if (
      !(await confirm({
        title: '重置统计',
        description: `重置 Key "${item.name}" 的累计统计？`,
        confirmText: '重置',
      }))
    )
      return
    try {
      await resetStats.mutateAsync(item.id)
      toast.success('统计已重置')
    } catch (err) {
      toast.error('重置失败：' + extractErrorMessage(err))
    }
  }

  const handleRotate = async (item: ClientKeyItem) => {
    const systemHint = item.isSystem
      ? '这是系统密钥（config.json apiKey），重新生成后会同步更新 config.json 的 apiKey。'
      : ''
    if (
      !(await confirm({
        title: '重新生成 Key',
        description: `重新生成 Key "${item.name}"？旧明文将立即失效，使用旧明文的下游需要换上新明文才能继续调用。Key 的名称、描述、绑定分组与累计统计保留不变。${systemHint ? ' ' + systemHint : ''}`,
        confirmText: '重新生成',
        destructive: true,
      }))
    )
      return
    try {
      const res = await rotateKey.mutateAsync(item.id)
      setCreatedKey(res)
      setShowCreatedPlain(true)
      // 系统密钥轮换后本地存储的 apiKey 已失效，提示用户用新明文重新登录
      if (item.isSystem) {
        toast.info('系统密钥已更新，若你正用该密钥登录管理面板，请用新明文重新登录')
      }
    } catch (err) {
      toast.error('重新生成失败：' + extractErrorMessage(err))
    }
  }

  const startEdit = (item: ClientKeyItem) => {
    setEditTarget(item)
    setEditName(item.name)
    setEditDesc(item.description ?? '')
    setEditGroup(item.group ?? '')
    setEditOpen(true)
  }

  const handleEditSave = async (e: React.FormEvent) => {
    e.preventDefault()
    if (!editTarget) return
    try {
      await updateKey.mutateAsync({
        id: editTarget.id,
        req: { name: editName.trim(), description: editDesc.trim(), group: editGroup.trim() },
      })
      toast.success('已更新')
      setEditOpen(false)
    } catch (err) {
      toast.error('更新失败：' + extractErrorMessage(err))
    }
  }

  const startPricing = (item: ClientKeyItem) => {
    setPricingTarget(item)
    if (item.billingPricePerCredit) {
      setPricingMode('perCredit')
      setPricingPerCredit(String(item.billingPricePerCredit))
      setPricingDiscount('')
    } else if (item.billingDiscount) {
      setPricingMode('discount')
      setPricingDiscount(String(item.billingDiscount))
      setPricingPerCredit('')
    } else {
      setPricingMode('perCredit')
      setPricingPerCredit('')
      setPricingDiscount('')
    }
    setPricingOpen(true)
  }

  const handleSavePricing = async (e: React.FormEvent) => {
    e.preventDefault()
    if (!pricingTarget) return
    const req: UpdateClientKeyRequest = {}
    if (pricingMode === 'perCredit') {
      const v = Number(pricingPerCredit)
      if (!pricingPerCredit || !(v > 0)) {
        toast.error('请填写有效的单价')
        return
      }
      req.billingPricePerCredit = v
      req.billingDiscount = 0 // 切换口径时清掉另一种，避免后端两个字段都非空
    } else if (pricingMode === 'discount') {
      const v = Number(pricingDiscount)
      if (!pricingDiscount || !(v > 0)) {
        toast.error('请填写有效的折扣系数')
        return
      }
      req.billingDiscount = v
      req.billingPricePerCredit = 0
    } else {
      req.billingPricePerCredit = 0
      req.billingDiscount = 0
    }
    try {
      await updateKey.mutateAsync({ id: pricingTarget.id, req })
      toast.success(pricingMode === 'clear' ? '已清除定价' : '定价已更新')
      setPricingOpen(false)
    } catch (err) {
      toast.error('更新失败：' + extractErrorMessage(err))
    }
  }

  const copyText = async (text: string) => {
    try {
      await navigator.clipboard.writeText(text)
      toast.success('已复制')
    } catch {
      toast.error('复制失败')
    }
  }

  return (
    <div>
      <div className="mb-6 flex items-end justify-between gap-4">
        <div>
          <h1 className="text-[28px] font-semibold tracking-tight leading-tight">客户端 Key</h1>
          <p className="mt-1 text-sm text-muted-foreground">
            分发给下游用户/项目的访问密钥。每把 Key 独立计数与禁用，泄露后只需替换一把。
          </p>
        </div>
        <Button onClick={() => setCreateOpen(true)} size="sm">
          <Plus className="h-3.5 w-3.5" />新建 Key
        </Button>
      </div>

      {isLoading ? (
        <Card>
          <CardContent className="py-16 text-center text-sm text-muted-foreground">
            加载中…
          </CardContent>
        </Card>
      ) : !data || data.keys.length === 0 ? (
        <Card>
          <CardContent className="py-16 text-center">
            <div className="mx-auto mb-3 flex h-12 w-12 items-center justify-center rounded-2xl bg-secondary text-muted-foreground">
              <KeyRound className="h-5 w-5" />
            </div>
            <p className="text-sm text-muted-foreground">还没有客户端 Key，点击右上角"新建 Key"开始</p>
          </CardContent>
        </Card>
      ) : (
        <Card>
          <CardContent className="overflow-x-auto p-0">
            <table className="w-full min-w-[1040px] text-sm">
              <thead className="text-[12px] text-muted-foreground border-b border-border/60">
                <tr className="whitespace-nowrap">
                  <th className="text-left font-medium px-4 py-3">ID</th>
                  <th className="text-left font-medium px-4 py-3">名称</th>
                  <th className="text-left font-medium px-4 py-3">Key</th>
                  <th className="text-left font-medium px-4 py-3">分组</th>
                  <th className="text-left font-medium px-4 py-3">对客定价</th>
                  <th className="text-left font-medium px-4 py-3">状态</th>
                  <th className="text-right font-medium px-4 py-3">总调用</th>
                  <th className="text-right font-medium px-4 py-3">输入</th>
                  <th className="text-right font-medium px-4 py-3">输出</th>
                  <th className="text-left font-medium px-4 py-3">最后使用</th>
                  <th className="sticky right-0 z-20 min-w-[11.75rem] border-l border-border/60 bg-card px-4 py-3 text-right font-medium">
                    操作
                  </th>
                </tr>
              </thead>
              <tbody>
                {data.keys.map((k) => (
                  <tr key={k.id} className="border-t border-border/40 whitespace-nowrap">
                    <td className="px-4 py-3 font-mono text-[12px] text-muted-foreground tabular-nums">
                      #{k.id}
                    </td>
                    <td className="px-4 py-3">
                      <div className="flex items-center gap-1.5">
                        <span className="max-w-[220px] truncate font-medium">{k.name}</span>
                        {k.isSystem && (
                          <Badge variant="secondary" title="由 config.json apiKey 同步，不可删除、可轮换">
                            系统
                          </Badge>
                        )}
                      </div>
                      {k.description && (
                        <div className="max-w-[220px] truncate text-[11px] text-muted-foreground">
                          {k.description}
                        </div>
                      )}
                    </td>
                    <td className="px-4 py-3">
                      <DropdownMenu>
                        <DropdownMenuTrigger asChild>
                          <button
                            type="button"
                            className="rounded px-1 py-0.5 font-mono text-[12px] text-muted-foreground hover:bg-accent/60 focus:outline-none focus-visible:ring-1 focus-visible:ring-ring"
                            title="点击展开 Key 操作"
                          >
                            {k.maskedKey}
                          </button>
                        </DropdownMenuTrigger>
                        <DropdownMenuContent align="start">
                          <DropdownMenuItem onSelect={() => handleRotate(k)}>
                            <RefreshCw className="h-3.5 w-3.5" />
                            重新生成 Key（旧 Key 立即失效）
                          </DropdownMenuItem>
                        </DropdownMenuContent>
                      </DropdownMenu>
                    </td>
                    <td className="px-4 py-3">
                      {k.group ? (
                        <Badge variant="outline">{k.group}</Badge>
                      ) : (
                        <span className="text-[12px] text-muted-foreground">全部账号</span>
                      )}
                    </td>
                    <td className="px-4 py-3">
                      <PricingCell item={k} />
                    </td>
                    <td className="px-4 py-3">
                      {k.disabled ? (
                        <Badge variant="destructive">已禁用</Badge>
                      ) : (
                        <Badge variant="success">启用</Badge>
                      )}
                    </td>
                    <td className="px-4 py-3 text-right tabular-nums">{k.totalCalls}</td>
                    <td className="px-4 py-3 text-right tabular-nums">{formatTokens(k.totalInputTokens)}</td>
                    <td className="px-4 py-3 text-right tabular-nums">{formatTokens(k.totalOutputTokens)}</td>
                    <td className="px-4 py-3 text-[12px] text-muted-foreground">
                      {formatRelative(k.lastUsedAt)}
                    </td>
                    <td className="sticky right-0 z-10 min-w-[11.75rem] border-l border-border/60 bg-card px-4 py-3">
                      <div className="flex items-center justify-end gap-1">
                        <Button
                          size="icon"
                          variant="ghost"
                          className="h-7 w-7"
                          onClick={() => startEdit(k)}
                          title="改名"
                        >
                          <Pencil className="h-3.5 w-3.5" />
                        </Button>
                        <Button
                          size="icon"
                          variant="ghost"
                          className="h-7 w-7"
                          onClick={() => startPricing(k)}
                          title="对客定价"
                        >
                          <Coins className="h-3.5 w-3.5" />
                        </Button>
                        <Button
                          size="icon"
                          variant="ghost"
                          className="h-7 w-7"
                          onClick={() => handleToggleDisabled(k)}
                          title={k.disabled ? '启用' : '禁用'}
                        >
                          <Power className={`h-3.5 w-3.5 ${k.disabled ? 'text-emerald-500' : 'text-amber-500'}`} />
                        </Button>
                        <Button
                          size="icon"
                          variant="ghost"
                          className="h-7 w-7"
                          onClick={() => handleReset(k)}
                          title="重置统计"
                        >
                          <RotateCcw className="h-3.5 w-3.5" />
                        </Button>
                        {!k.isSystem && (
                          <Button
                            size="icon"
                            variant="ghost"
                            className="h-7 w-7"
                            onClick={() => handleDelete(k)}
                            title="删除"
                          >
                            <Trash2 className="h-3.5 w-3.5 text-destructive" />
                          </Button>
                        )}
                      </div>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </CardContent>
        </Card>
      )}

      {/* 新建对话框 */}
      <Dialog open={createOpen} onOpenChange={(o) => !createKey.isPending && setCreateOpen(o)}>
        <DialogContent className="sm:max-w-md">
          <DialogHeader>
            <DialogTitle>新建客户端 Key</DialogTitle>
            <DialogDescription>
              创建后明文 Key 仅显示一次，请立即复制保存到安全位置。
            </DialogDescription>
          </DialogHeader>
          <form onSubmit={handleCreate} className="space-y-3 py-2">
            <div>
              <label className="text-[12px] text-muted-foreground">名称 *</label>
              <Input
                placeholder="VS Code 本机 / 团队 A 等"
                value={createName}
                onChange={(e) => setCreateName(e.target.value)}
                disabled={createKey.isPending}
                autoFocus
              />
            </div>
            <div>
              <label className="text-[12px] text-muted-foreground">描述（可选）</label>
              <Input
                placeholder="可选备注，如绑定的项目、负责人等"
                value={createDesc}
                onChange={(e) => setCreateDesc(e.target.value)}
                disabled={createKey.isPending}
              />
            </div>
            <div>
              <label className="text-[12px] text-muted-foreground">绑定分组（可选）</label>
              <GroupSingleSelect
                value={createGroup}
                options={groupOptions}
                onChange={setCreateGroup}
                disabled={createKey.isPending}
                noneLabel="（不绑定，可用全部账号）"
              />
              <p className="mt-1 text-[11px] text-muted-foreground">
                绑定后该 Key 仅会使用含此分组的账号（严格隔离，分组内无可用账号时请求会失败）。
              </p>
            </div>
            <DialogFooter>
              <Button type="button" variant="outline" onClick={() => setCreateOpen(false)} disabled={createKey.isPending}>
                取消
              </Button>
              <Button type="submit" disabled={createKey.isPending || !createName.trim()}>
                {createKey.isPending ? '创建中…' : '创建'}
              </Button>
            </DialogFooter>
          </form>
        </DialogContent>
      </Dialog>

      {/* 创建后明文展示对话框 */}
      <Dialog open={!!createdKey} onOpenChange={(o) => { if (!o) setCreatedKey(null) }}>
        <DialogContent className="sm:max-w-md">
          <DialogHeader>
            <DialogTitle className="flex items-center gap-2">
              <KeyRound className="h-4 w-4 text-emerald-500" />
              Key 已生成
            </DialogTitle>
            <DialogDescription>
              这是 Key "{createdKey?.name}" 的明文。<strong>关闭对话框后将无法再查看</strong>，请立即复制。
            </DialogDescription>
          </DialogHeader>
          <div className="space-y-3">
            <div className="relative">
              <Input
                readOnly
                type={showCreatedPlain ? 'text' : 'password'}
                value={createdKey?.key ?? ''}
                className="pr-20 font-mono text-[13px]"
              />
              <div className="absolute inset-y-0 right-0 flex items-center pr-1">
                <Button
                  type="button"
                  size="icon"
                  variant="ghost"
                  className="h-7 w-7"
                  onClick={() => setShowCreatedPlain((v) => !v)}
                  title={showCreatedPlain ? '隐藏' : '显示'}
                >
                  {showCreatedPlain ? <EyeOff className="h-3.5 w-3.5" /> : <Eye className="h-3.5 w-3.5" />}
                </Button>
                <Button
                  type="button"
                  size="icon"
                  variant="ghost"
                  className="h-7 w-7"
                  onClick={() => createdKey && copyText(createdKey.key)}
                  title="复制"
                >
                  <Copy className="h-3.5 w-3.5" />
                </Button>
              </div>
            </div>
            <p className="text-[11px] text-muted-foreground">
              客户端调用 <code>/v1/messages</code> 时，把它放在 <code>x-api-key</code> 或 <code>Authorization: Bearer</code> 头中。
            </p>
          </div>
          <DialogFooter>
            <Button onClick={() => setCreatedKey(null)}>我已保存好</Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {/* 编辑对话框 */}
      <Dialog open={editOpen} onOpenChange={(o) => !updateKey.isPending && setEditOpen(o)}>
        <DialogContent className="sm:max-w-md">
          <DialogHeader>
            <DialogTitle>编辑 Key</DialogTitle>
            <DialogDescription>修改名称与描述（不影响 Key 值与统计）</DialogDescription>
          </DialogHeader>
          <form onSubmit={handleEditSave} className="space-y-3 py-2">
            <div>
              <label className="text-[12px] text-muted-foreground">名称</label>
              <Input value={editName} onChange={(e) => setEditName(e.target.value)} />
            </div>
            <div>
              <label className="text-[12px] text-muted-foreground">描述</label>
              <Input value={editDesc} onChange={(e) => setEditDesc(e.target.value)} />
            </div>
            <div>
              <label className="text-[12px] text-muted-foreground">绑定分组</label>
              <GroupSingleSelect
                value={editGroup}
                options={groupOptions}
                onChange={setEditGroup}
                disabled={updateKey.isPending}
                noneLabel="（不绑定，可用全部账号）"
              />
              <p className="mt-1 text-[11px] text-muted-foreground">
                绑定后仅调度该分组内账号（严格隔离）。选「不绑定」表示解除绑定。
              </p>
            </div>
            <DialogFooter>
              <Button type="button" variant="outline" onClick={() => setEditOpen(false)}>取消</Button>
              <Button type="submit" disabled={updateKey.isPending}>
                {updateKey.isPending ? '保存中…' : '保存'}
              </Button>
            </DialogFooter>
          </form>
        </DialogContent>
      </Dialog>

      {/* 对客定价对话框 */}
      <Dialog open={pricingOpen} onOpenChange={(o) => !updateKey.isPending && setPricingOpen(o)}>
        <DialogContent className="sm:max-w-md">
          <DialogHeader>
            <DialogTitle>设置对客定价</DialogTitle>
            <DialogDescription>
              为 Key "{pricingTarget?.name}" 设置计费口径，用于「月度账单」页计算应收与毛利。
            </DialogDescription>
          </DialogHeader>
          <form onSubmit={handleSavePricing} className="space-y-3 py-2">
            <div className="grid grid-cols-3 gap-1 rounded-md border border-border/60 p-0.5">
              <Button
                type="button"
                size="sm"
                variant={pricingMode === 'perCredit' ? 'default' : 'ghost'}
                className="h-8 rounded-md px-2 text-[12px]"
                onClick={() => setPricingMode('perCredit')}
              >
                按 credit 单价
              </Button>
              <Button
                type="button"
                size="sm"
                variant={pricingMode === 'discount' ? 'default' : 'ghost'}
                className="h-8 rounded-md px-2 text-[12px]"
                onClick={() => setPricingMode('discount')}
              >
                按官方价折扣
              </Button>
              <Button
                type="button"
                size="sm"
                variant={pricingMode === 'clear' ? 'default' : 'ghost'}
                className="h-8 rounded-md px-2 text-[12px]"
                onClick={() => setPricingMode('clear')}
              >
                清除定价
              </Button>
            </div>

            {pricingMode === 'perCredit' && (
              <div>
                <label className="text-[12px] text-muted-foreground">单价（$ / credit）</label>
                <Input
                  type="number"
                  step="0.0001"
                  min="0"
                  placeholder="如 0.02"
                  value={pricingPerCredit}
                  onChange={(e) => setPricingPerCredit(e.target.value)}
                  disabled={updateKey.isPending}
                  autoFocus
                />
                <p className="mt-1 text-[11px] text-muted-foreground">
                  按实际计费量（credit）直接乘单价，口径可信。
                </p>
              </div>
            )}
            {pricingMode === 'discount' && (
              <div>
                <label className="text-[12px] text-muted-foreground">
                  折扣系数（0–1，如 0.3 = 三折）
                </label>
                <Input
                  type="number"
                  step="0.01"
                  min="0"
                  max="1"
                  placeholder="如 0.3"
                  value={pricingDiscount}
                  onChange={(e) => setPricingDiscount(e.target.value)}
                  disabled={updateKey.isPending}
                  autoFocus
                />
                <p className="mt-1 text-[11px] text-amber-600">
                  依赖官方牌价的本地估算，非上游直接下发，此口径下的应收/毛利为参考值。
                </p>
              </div>
            )}
            {pricingMode === 'clear' && (
              <p className="text-[12px] text-muted-foreground">
                将清除该 Key 的单价与折扣设置，账单页会把它列入「未定价」并提示处理。
              </p>
            )}

            <DialogFooter>
              <Button type="button" variant="outline" onClick={() => setPricingOpen(false)} disabled={updateKey.isPending}>
                取消
              </Button>
              <Button type="submit" disabled={updateKey.isPending}>
                {updateKey.isPending ? '保存中…' : '保存'}
              </Button>
            </DialogFooter>
          </form>
        </DialogContent>
      </Dialog>
    </div>
  )
}
