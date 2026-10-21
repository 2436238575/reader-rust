import { describe, expect, it } from 'vitest'
import {
  buildDiffRows,
  diffParagraphs,
  diffStats,
  inlineDiff,
  splitToParagraphs,
  type ParagraphDiffOp,
} from './chapterDiff'

describe('splitToParagraphs', () => {
  it('按行切段、去空行、剥 HTML 标签', () => {
    expect(splitToParagraphs('第一段\n\n  \n第二段  \n')).toEqual(['第一段', '第二段'])
    expect(splitToParagraphs('<p>甲</p>\n<p>乙</p>')).toEqual(['甲', '乙'])
    expect(splitToParagraphs('')).toEqual([])
  })
})

describe('diffParagraphs', () => {
  const types = (ops: ParagraphDiffOp[]) => ops.map((op) => op.type)

  it('完全相同：全 equal', () => {
    const ops = diffParagraphs(['a', 'b'], ['a', 'b'])
    expect(types(ops)).toEqual(['equal', 'equal'])
  })

  it('纯新增/纯删除', () => {
    expect(types(diffParagraphs(['a'], ['a', 'b', 'c']))).toEqual(['equal', 'add', 'add'])
    expect(types(diffParagraphs(['a', 'b', 'c'], ['a']))).toEqual(['equal', 'del', 'del'])
  })

  it('同位置删+加合并为 modify，内容进 op', () => {
    const ops = diffParagraphs(['a', '旧段', 'c'], ['a', '新段', 'c'])
    expect(types(ops)).toEqual(['equal', 'modify', 'equal'])
    expect(ops[1].baseText).toBe('旧段')
    expect(ops[1].editedText).toBe('新段')
    expect(ops[1].baseIndex).toBe(1)
    expect(ops[1].editedIndex).toBe(1)
  })

  it('一对多：删一加二合并为 modify + add（改一段、新增一段）', () => {
    const ops = diffParagraphs(['a', 'x', 'd'], ['a', 'y', 'z', 'd'])
    expect(types(ops)).toEqual(['equal', 'modify', 'add', 'equal'])
    expect(ops[1].baseText).toBe('x')
    expect(ops[1].editedText).toBe('y')
    expect(ops[2].editedText).toBe('z')
  })

  it('中间整块替换（多段重新对齐）', () => {
    const ops = diffParagraphs(['头', '甲', '乙', '尾'], ['头', '丙', '乙', '尾'])
    expect(types(ops)).toEqual(['equal', 'modify', 'equal', 'equal'])
  })

  it('空对空 / 单边为空', () => {
    expect(diffParagraphs([], [])).toEqual([])
    expect(types(diffParagraphs([], ['a']))).toEqual(['add'])
    expect(types(diffParagraphs(['a'], []))).toEqual(['del'])
  })
})

describe('inlineDiff', () => {
  it('公共前后缀剥离出中段', () => {
    const d = inlineDiff('他走进了那座古庙，灯还亮着。', '他走出了那座古庙，灯还亮着。')
    expect(d.prefix).toBe('他走')
    expect(d.removed).toBe('进')
    expect(d.added).toBe('出')
    expect(d.suffix).toBe('了那座古庙，灯还亮着。')
  })

  it('纯前缀增加 / 纯尾部删除', () => {
    expect(inlineDiff('abc', 'abcdef')).toEqual({
      prefix: 'abc',
      removed: '',
      added: 'def',
      suffix: '',
    })
    const d = inlineDiff('abcdef', 'abc')
    expect(d.removed).toBe('def')
    expect(d.added).toBe('')
  })

  it('完全不同', () => {
    const d = inlineDiff('aaa', 'bbb')
    expect(d.prefix).toBe('')
    expect(d.suffix).toBe('')
    expect(d.removed).toBe('aaa')
    expect(d.added).toBe('bbb')
  })
})

describe('diffStats', () => {
  it('分类计数', () => {
    const stats = diffStats([
      { type: 'equal' },
      { type: 'add' },
      { type: 'add' },
      { type: 'del' },
      { type: 'modify' },
    ])
    expect(stats).toEqual({ added: 2, removed: 1, modified: 1 })
  })
})

describe('buildDiffRows', () => {
  const opsOf = (types: string[]): ParagraphDiffOp[] =>
    types.map((t, i) => ({ type: t as ParagraphDiffOp['type'], baseIndex: i, editedIndex: i }))

  it('短 equal 段不折叠', () => {
    const rows = buildDiffRows(opsOf(['equal', 'modify', 'equal']))
    expect(rows.every((r) => r.kind === 'op')).toBe(true)
    expect(rows).toHaveLength(3)
  })

  it('长 equal 段折叠中间，变更区上下各留 3 段', () => {
    const ops = opsOf([
      'modify',
      ...Array.from({ length: 20 }, () => 'equal'),
      'add',
      ...Array.from({ length: 20 }, () => 'equal'),
    ])
    const rows = buildDiffRows(ops)
    const collapses = rows.filter((r) => r.kind === 'collapse')
    expect(collapses).toHaveLength(2)
    // 中间 run（上下都有变更）：头 3 + 折叠 14 + 尾 3
    expect(collapses[0]).toMatchObject({ opsStart: 4, opsCount: 14 })
    // 结尾 run（到序列末尾）：只留头 3，其余 17 段折叠
    expect(collapses[1]).toMatchObject({ opsCount: 17 })
  })

  it('折叠块一次展开 15 段，剩余继续折叠', () => {
    const ops = opsOf(['modify', ...Array.from({ length: 40 }, () => 'equal'), 'modify'])
    // 40 段 run：头 3 + 折叠 34（opsStart=4）+ 尾 3；展开 15 后剩余 19
    const rows = buildDiffRows(ops, { expanded: new Map([[4, 15]]) })
    const collapse = rows.find((r) => r.kind === 'collapse')
    expect(collapse).toMatchObject({ opsStart: 19, opsCount: 19 })
    // 展开的 15 段平铺成行：2 变更 + 头 3 + 15 + 尾 3
    const opRows = rows.filter((r) => r.kind === 'op')
    expect(opRows).toHaveLength(2 + 3 + 15 + 3)
  })

  it('全部展开后不再有折叠行', () => {
    const ops = opsOf(['modify', ...Array.from({ length: 10 }, () => 'equal'), 'modify'])
    // 10 段 run：头 3 + 折叠 4（opsStart=4）+ 尾 3；展开 4 即全展开
    const rows = buildDiffRows(ops, { expanded: new Map([[4, 4]]) })
    expect(rows.every((r) => r.kind === 'op')).toBe(true)
    expect(rows).toHaveLength(12)
  })
})
