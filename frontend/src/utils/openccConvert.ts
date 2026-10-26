// 简繁转换（OpenCC）的按需加载与转换：从 reader store 拆出的纯逻辑模块（reader store 拆分专项）。
// 两个方向各自按需加载（cn2t=简→繁、t2cn=繁→简，字典互不重叠），
// 不用 full 包就是为了避免把没在用的那份字典也拉下来。
// 转换器以模块级 ref 持有：加载完成后，依赖它的 computed（displayContent）会自动重算。

import { ref } from 'vue'

export type ChineseConvertMode = 'simplified' | 'traditional'

const s2tConverter = ref<((text: string) => string) | null>(null)
const t2sConverter = ref<((text: string) => string) | null>(null)
let s2tLoading: Promise<void> | null = null
let t2sLoading: Promise<void> | null = null

export function ensureChineseConverter(mode: ChineseConvertMode): Promise<void> {
  if (mode === 'traditional') {
    if (s2tConverter.value || s2tLoading) return s2tLoading ?? Promise.resolve()
    // 只引 cn2t 子路径（而非 full 包）：词级字典只带简→繁一份，按需动态加载
    s2tLoading = import('opencc-js/cn2t')
      .then((module) => {
        s2tConverter.value = module.Converter({ from: 'cn', to: 'tw' })
      })
      .catch(() => {
        s2tConverter.value = null
      })
      .finally(() => {
        s2tLoading = null
      })
    return s2tLoading
  }
  if (t2sConverter.value || t2sLoading) return t2sLoading ?? Promise.resolve()
  t2sLoading = import('opencc-js/t2cn')
    .then((module) => {
      t2sConverter.value = module.Converter({ from: 'tw', to: 'cn' })
    })
    .catch(() => {
      t2sConverter.value = null
    })
    .finally(() => {
      t2sLoading = null
    })
  return t2sLoading
}

export function convertChineseText(text: string, mode: ChineseConvertMode): string {
  if (!text) return text
  // 按当前模式取对应方向的转换器；切换模式后转换器若还在加载，
  // 先显示原文——converter 是 ref，加载完成时 displayContent 会自动重算
  const converter = mode === 'traditional' ? s2tConverter.value : t2sConverter.value
  return converter ? converter(text) : text
}
