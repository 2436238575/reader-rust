/**
 * 章节编辑版 vs 上游基准的 diff（纯函数，无响应式状态）。
 *
 * 操作序列由后端进程内算好随正文一起返回；这里只做前端展示需要的
 * 数据补齐、段内对照与面板折叠。
 */

export interface ParagraphDiffOp {
  type: 'equal' | 'add' | 'del' | 'modify'
  /** 基准段落下标（equal/del/modify 有） */
  baseIndex?: number
  /** 编辑版段落下标（equal/add/modify 有） */
  editedIndex?: number
  baseText?: string
  editedText?: string
}

/** 正文按行切成段：剥 HTML 标签（基准可能是 HTML 正文）、去空行，与渲染段落同口径。 */
export function splitToParagraphs(text: string): string[] {
  if (!text) return []
  return text
    .replace(/<[^>]+>/g, '')
    .split(/\n/)
    .map((line) => line.trim())
    .filter((line) => line.length > 0)
}

/**
 * 后端为省流量不给 `equal` 行带文本（它们和编辑版正文完全一致）。
 * 这里按编辑版正文把文本补回来，面板的上下文行才显示得出来。
 */
export function hydrateDiffOps(ops: ParagraphDiffOp[], content: string): ParagraphDiffOp[] {
  const paragraphs = splitToParagraphs(content)
  return ops.map((op) => {
    if (op.editedIndex == null || op.editedText != null) return op
    const text = paragraphs[op.editedIndex]
    return text == null ? op : { ...op, editedText: text }
  })
}

export interface InlineDiff {
  prefix: string
  /** 基准里被去掉的中段（编辑版里不存在，只用来定位删除点） */
  removed: string
  /** 编辑版里多出来的中段 */
  added: string
  suffix: string
}

/** 段内 diff：公共前缀 + 公共后缀剥离，中段即变动区。 */
export function inlineDiff(baseText: string, editedText: string): InlineDiff {
  let start = 0
  const minLen = Math.min(baseText.length, editedText.length)
  while (start < minLen && baseText[start] === editedText[start]) start++
  let endBase = baseText.length
  let endEdited = editedText.length
  while (
    endBase > start &&
    endEdited > start &&
    baseText[endBase - 1] === editedText[endEdited - 1]
  ) {
    endBase--
    endEdited--
  }
  return {
    prefix: editedText.slice(0, start),
    removed: baseText.slice(start, endBase),
    added: editedText.slice(start, endEdited),
    suffix: editedText.slice(endEdited),
  }
}

export function diffStats(ops: ParagraphDiffOp[]): {
  added: number
  removed: number
  modified: number
} {
  let added = 0
  let removed = 0
  let modified = 0
  for (const op of ops) {
    if (op.type === 'add') added++
    else if (op.type === 'del') removed++
    else if (op.type === 'modify') modified++
  }
  return { added, removed, modified }
}

/* ─── 正文里点 diff 标记的就地预览 ─── */

export interface DiffPreviewModel {
  kind: 'add' | 'modify' | 'delete'
  /** 该段自身的原文（add 没有） */
  baseText?: string
  /** 该段自身的新文本（delete 没有） */
  editedText?: string
  /** 紧邻的删除块（大段删除 / 删除+修改混合时都会带） */
  deleted: string[]
  deletedTotal: number
}

/**
 * 从操作序列里取出某个段落的预览内容。
 *
 * `opIndex` 是段落自身对应的 add/modify 操作；`delStart` 是删除块里第一条
 * del 的下标（删除箭头挂在「删除块后第一个未变段」上，所以单独记）。
 */
export function buildDiffPreview(
  ops: ParagraphDiffOp[] | null,
  opIndex?: number | null,
  delStart?: number | null
): DiffPreviewModel | null {
  if (!ops?.length) return null
  const deleted: string[] = []
  if (delStart != null && delStart >= 0 && delStart < ops.length) {
    for (let i = delStart; i < ops.length && ops[i].type === 'del'; i += 1) {
      deleted.push(ops[i].baseText || '')
    }
  }
  const op = opIndex != null && opIndex >= 0 ? ops[opIndex] : undefined
  if (op?.type === 'add') {
    return {
      kind: 'add',
      editedText: op.editedText || '',
      deleted,
      deletedTotal: deleted.length,
    }
  }
  if (op?.type === 'modify') {
    return {
      kind: 'modify',
      baseText: op.baseText || '',
      editedText: op.editedText || '',
      deleted,
      deletedTotal: deleted.length,
    }
  }
  if (deleted.length) {
    return { kind: 'delete', deleted, deletedTotal: deleted.length }
  }
  return null
}

/* ─── diff 面板的折叠行序列 ─── */

export type DiffRow =
  | { kind: 'op'; op: ParagraphDiffOp }
  | { kind: 'collapse'; /** 被折叠的 op 起始下标与数量 */ opsStart: number; opsCount: number }

/**
 * 把操作序列摊成渲染行：连续未变段超过 上下context×2+1 时中间折叠成
 * collapse 行；`expanded` 记录某个折叠块已展开的数量（面板按「一次展开
 * 15 段」递增它，展开后剩余部分生成新的折叠行）。
 */
export function buildDiffRows(
  ops: ParagraphDiffOp[],
  options: { context?: number; expanded?: ReadonlyMap<number, number> } = {}
): DiffRow[] {
  const context = options.context ?? 3
  const expanded = options.expanded ?? new Map<number, number>()
  const rows: DiffRow[] = []

  let i = 0
  while (i < ops.length) {
    if (ops[i].type !== 'equal') {
      rows.push({ kind: 'op', op: ops[i] })
      i++
      continue
    }
    // 连续 equal 段
    let j = i
    while (j < ops.length && ops[j].type === 'equal') j++
    const runLength = j - i
    // 首尾的 equal run 只留 context 段（开头没有上文、结尾没有下文）
    const isFirst = i === 0
    const isLast = j === ops.length
    const keepHead = isFirst ? 0 : context
    const keepTail = isLast ? 0 : context
    if (runLength <= keepHead + keepTail + 1) {
      for (let k = i; k < j; k++) rows.push({ kind: 'op', op: ops[k] })
    } else {
      for (let k = i; k < i + keepHead; k++) rows.push({ kind: 'op', op: ops[k] })
      const collapseStart = i + keepHead
      const collapseCount = runLength - keepHead - keepTail
      const shown = Math.min(expanded.get(collapseStart) ?? 0, collapseCount)
      for (let k = collapseStart; k < collapseStart + shown; k++) {
        rows.push({ kind: 'op', op: ops[k] })
      }
      const remaining = collapseCount - shown
      if (remaining > 0) {
        rows.push({
          kind: 'collapse',
          opsStart: collapseStart + shown,
          opsCount: remaining,
        })
      }
      for (let k = j - keepTail; k < j; k++) rows.push({ kind: 'op', op: ops[k] })
    }
    i = j
  }
  return rows
}
