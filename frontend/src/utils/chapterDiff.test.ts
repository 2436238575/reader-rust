import { describe, expect, it } from 'vitest'
import {
  buildDiffPreview,
  buildDiffRows,
  diffStats,
  hydrateDiffOps,
  inlineDiff,
  inlinePartsFor,
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

describe('hydrateDiffOps', () => {
  it('用编辑版正文补齐 equal 行文本，已有文本不覆盖', () => {
    const ops = hydrateDiffOps(
      [
        { type: 'equal', baseIndex: 0, editedIndex: 0 },
        { type: 'modify', baseIndex: 1, editedIndex: 1, baseText: '旧', editedText: '新' },
        { type: 'del', baseIndex: 2, baseText: '删掉' },
      ],
      '第一段\n第二段\n'
    )
    expect(ops[0].editedText).toBe('第一段')
    expect(ops[1].editedText).toBe('新')
    expect(ops[2].editedText).toBeUndefined()
  })
})

describe('buildDiffPreview', () => {
  it('modify：带原文、新文与段内新增片段', () => {
    const ops: ParagraphDiffOp[] = [
      { type: 'equal', baseIndex: 0, editedIndex: 0 },
      { type: 'modify', baseIndex: 1, editedIndex: 1, baseText: '旧句子', editedText: '新句子' },
    ]
    const model = buildDiffPreview(ops, 1, null)
    expect(model).toMatchObject({ kind: 'modify', baseText: '旧句子', editedText: '新句子' })
    expect(model!.deletedTotal).toBe(0)
  })

  it('add：只带新文本', () => {
    const ops: ParagraphDiffOp[] = [{ type: 'add', editedIndex: 0, editedText: '新增' }]
    expect(buildDiffPreview(ops, 0, null)).toMatchObject({
      kind: 'add',
      editedText: '新增',
    })
  })

  it('删除块：从 delStart 起收集连续 del；只有删除时 kind=delete', () => {
    const ops: ParagraphDiffOp[] = [
      { type: 'equal', baseIndex: 0, editedIndex: 0 },
      { type: 'del', baseIndex: 1, baseText: '删一' },
      { type: 'del', baseIndex: 2, baseText: '删二' },
      { type: 'equal', baseIndex: 3, editedIndex: 1 },
    ]
    const model = buildDiffPreview(ops, 3, 1)
    expect(model).toMatchObject({ kind: 'delete', deleted: ['删一', '删二'], deletedTotal: 2 })
  })

  it('没有可展示内容时返回 null', () => {
    expect(buildDiffPreview(null, 0, null)).toBeNull()
    expect(buildDiffPreview([{ type: 'equal', baseIndex: 0, editedIndex: 0 }], 0, null)).toBeNull()
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

describe('inlinePartsFor', () => {
  it('段内改动：删除侧强调 removed，新增侧强调 added', () => {
    const op = { baseText: '他走进了那座古庙，灯还亮着。', editedText: '他走出了那座古庙，灯还亮着。' }
    expect(inlinePartsFor('del', op)).toEqual([
      { text: '他走', changed: false },
      { text: '进', changed: true },
      { text: '了那座古庙，灯还亮着。', changed: false },
    ])
    expect(inlinePartsFor('add', op)).toEqual([
      { text: '他走', changed: false },
      { text: '出', changed: true },
      { text: '了那座古庙，灯还亮着。', changed: false },
    ])
  })

  it('纯删除行没有配对段可参照，整段都是变动', () => {
    expect(inlinePartsFor('del', { baseText: '整段被删掉的内容' })).toEqual([
      { text: '整段被删掉的内容', changed: true },
    ])
  })

  it('纯新增行整段都是变动', () => {
    expect(inlinePartsFor('add', { editedText: '整段新加的内容' })).toEqual([
      { text: '整段新加的内容', changed: true },
    ])
  })

  it('两行完全相同时只有公共片段，没有变动', () => {
    expect(inlinePartsFor('del', { baseText: '没变化', editedText: '没变化' })).toEqual([
      { text: '没变化', changed: false },
    ])
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
