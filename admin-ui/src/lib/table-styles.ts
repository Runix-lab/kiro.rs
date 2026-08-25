/**
 * 数据表格的共享样式 token。
 *
 * 「模型用量与折扣」「用量与承载」两张表结构几乎一致（首列文本 + 若干右对齐数值列），
 * 之前在两个文件里各写一套几乎相同的 Tailwind 字符串，容易出现间距/对齐不一致
 * （例如页眉与页体列宽不匹配）。抽成常量后两处共用同一套值，改一处两处同步生效。
 *
 * 数值列统一带 `whitespace-nowrap`：表格外层已有 `overflow-x-auto` + `min-w-[...]`
 * 兜底横向滚动，但数值一旦意外换行会破坏 `tabular-nums` 对齐，宁可多滚动也不能让
 * 数字断行。
 */

/** 表头 · 首列（左对齐文本，如"模型" / "入口 Key"） */
export const TH_LABEL = 'pb-2.5 text-left font-medium whitespace-nowrap'
/** 表头 · 数值列（右对齐） */
export const TH_NUM = 'pb-2.5 pl-3 text-right font-medium whitespace-nowrap'
/** 表头 · 中间的左对齐文本列（如"主用模型"） */
export const TH_TEXT = 'pb-2.5 pl-3 text-left font-medium whitespace-nowrap'

/** 表体 · 首列（可能较长，需要 truncate + title 才能不丢失信息） */
export const TD_LABEL = 'max-w-[280px] truncate py-2.5 pr-4 font-medium'
/** 表体 · 数值列（右对齐，禁止换行） */
export const TD_NUM = 'py-2.5 pl-3 text-right tabular-nums whitespace-nowrap'
/** 表体 · 强调数值列（如折扣，末列右对齐加粗） */
export const TD_NUM_STRONG = 'py-2.5 pl-3 text-right font-semibold tabular-nums whitespace-nowrap'
