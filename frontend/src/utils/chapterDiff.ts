/**
 * 章节编辑版 vs 上游基准的 diff（纯函数，无响应式状态）。
 *
 * 两级：段落级 LCS（段 = 非空行，与正文渲染的段落口径一致），
 * 段内用公共前后缀剥离出变动中段（中文按字，够用且快）。
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

/** 段级 LCS（DP + 回溯）；del 紧跟 add 的一对合并为 modify。 */
export function diffParagraphs(base: string[], edited: string[]): ParagraphDiffOp[] {
  const m = base.length
  const n = edited.length
  if (!m && !n) return []
  // dp[i][j] = LCS(base[i:], edited[j:]) 的长度
  const dp = new Uint32Array((m + 1) * (n + 1))
  const at = (i: number, j: number) => i * (n + 1) + j
  for (let i = m - 1; i >= 0; i--) {
    for (let j = n - 1; j >= 0; j--) {
      dp[at(i, j)] =
        base[i] === edited[j]
          ? dp[at(i + 1, j + 1)] + 1
          : Math.max(dp[at(i + 1, j)], dp[at(i, j + 1)])
    }
  }
  const ops: ParagraphDiffOp[] = []
  let i = 0
  let j = 0
  while (i < m && j < n) {
    if (base[i] === edited[j]) {
      ops.push({ type: 'equal', baseIndex: i, editedIndex: j, baseText: base[i], editedText: edited[j] })
      i++
      j++
    } else if (dp[at(i + 1, j)] >= dp[at(i, j + 1)]) {
      ops.push({ type: 'del', baseIndex: i, baseText: base[i] })
      i++
    } else {
      ops.push({ type: 'add', editedIndex: j, editedText: edited[j] })
      j++
    }
  }
  while (i < m) {
    ops.push({ type: 'del', baseIndex: i, baseText: base[i] })
    i++
  }
  while (j < n) {
    ops.push({ type: 'add', editedIndex: j, editedText: edited[j] })
    j++
  }
  // 同位置的「删一段 + 加一段」合并为 modify（一对一；一对多不强行撮合）
  const merged: ParagraphDiffOp[] = []
  for (const op of ops) {
    const last = merged[merged.length - 1]
    if (last?.type === 'del' && op.type === 'add') {
      merged[merged.length - 1] = {
        type: 'modify',
        baseIndex: last.baseIndex,
        editedIndex: op.editedIndex,
        baseText: last.baseText,
        editedText: op.editedText,
      }
    } else {
      merged.push(op)
    }
  }
  return merged
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
  while (endBase > start && endEdited > start && baseText[endBase - 1] === editedText[endEdited - 1]) {
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

export function diffStats(ops: ParagraphDiffOp[]): { added: number; removed: number; modified: number } {
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
