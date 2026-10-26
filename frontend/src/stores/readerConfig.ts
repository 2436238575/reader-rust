// 阅读配置、主题与正文呈现管线：从 reader store 拆出的子 store（reader store 拆分专项）。
// 覆盖：ReadConfig 的读写（localStorage `readConfig`）、主题/夜间模式、替换规则、
// 简繁转换，以及三者叠加出的 displayContent/processContentForDisplay。
//
// 依赖方向：本 store 依赖 reader 核心（content/book/预加载池）与 app 主题；
// reader 不反向依赖本 store。updateConfig 里清预加载池是 config → reader 的
// 单向写入；正文管线（readerContent）与评论（readerExtras）读 config 同理单向。

import { defineStore } from 'pinia'
import { computed, reactive, ref, watch } from 'vue'
import { useAppStore } from './app'
import { useReaderStore } from './reader'
import { safeLocalSet } from '../utils/storage'
import { getReplaceRules } from '../api/replaceRule'
import type { ReplaceRule } from '../types'
import { themePresets } from '../utils/readerTheme'
import { convertChineseText, ensureChineseConverter } from '../utils/openccConvert'
import { applyReplaceRulesToText } from '../utils/replaceRules'

/* ─── Reading config type ─── */
export interface ReadConfig {
  fontSize: number
  fontWeight: number
  fontFamily: string
  lineHeight: number
  paragraphSpacing: number
  firstLineIndent: boolean
  fontColor: string
  pageWidth: number
  pageMode: 'auto' | 'mobile'
  readMethod: '上下滑动' | '左右翻页' | '双页翻页' | '上下滚动' | '上下滚动2'
  animateDuration: number
  autoPageMode: 'pixel' | 'paragraph'
  scrollPixel: number
  pageSpeed: number
  clickAction: 'next' | 'auto' | 'none'
  selectAction: 'popup' | 'contextmenu' | 'ignore'
  chineseMode: 'simplified' | 'traditional'
  specialMode: 'normal' | 'simple'
  enablePreload: boolean
  /** 显示章节配图（书源配图规则取回的图，以及正文 HTML 里的 <img>） */
  showChapterImages: boolean
  /** 已编辑章节的正文 diff 标记（绿/蓝竖线、红箭头、段内虚线） */
  showDiffMarks: boolean
  /** 正文版本：编辑版优先，或只读上游原版（只对编辑过的章节有区别） */
  contentVersion: 'edited' | 'upstream'
  /** 评论里回复的配图形态：缩略图直接显示，或番茄式「查看图片」文字链 */
  replyImageStyle: 'thumbnail' | 'link'
}

const defaultConfig: ReadConfig = {
  fontSize: 18,
  fontWeight: 400,
  fontFamily: 'system',
  lineHeight: 1.8,
  paragraphSpacing: 0.2,
  firstLineIndent: true,
  fontColor: '',
  pageWidth: 800,
  pageMode: 'auto',
  readMethod: '上下滑动',
  animateDuration: 300,
  autoPageMode: 'pixel',
  scrollPixel: 1,
  pageSpeed: 1000,
  clickAction: 'auto',
  selectAction: 'ignore',
  chineseMode: 'simplified',
  specialMode: 'normal',
  enablePreload: true,
  showChapterImages: true,
  showDiffMarks: true,
  contentVersion: 'edited',
  replyImageStyle: 'thumbnail',
}

function loadConfig(): ReadConfig {
  try {
    const saved = localStorage.getItem('readConfig')
    if (saved) return { ...defaultConfig, ...JSON.parse(saved) }
  } catch {
    /* ignore */
  }
  return { ...defaultConfig }
}

export const useReaderConfigStore = defineStore('readerConfig', () => {
  const appStore = useAppStore()
  const readerStore = useReaderStore()

  /* ─── Reading config ─── */
  const config = reactive<ReadConfig>(loadConfig())

  function saveConfig() {
    safeLocalSet('readConfig', JSON.stringify(config))
  }

  function updateConfig<K extends keyof ReadConfig>(key: K, value: ReadConfig[K]) {
    config[key] = value
    if (key === 'enablePreload' && !value) {
      readerStore.preloadedContent.clear()
    }
    // 预载内容是按旧版本抓的：切版本后作废，否则翻到邻章会拿到另一版正文
    if (key === 'contentVersion') {
      readerStore.preloadedContent.clear()
    }
    saveConfig()
  }

  function resetConfig() {
    Object.assign(config, defaultConfig)
    saveConfig()
  }

  /* ─── Theme ─── */
  const themeIndex = ref(parseInt(localStorage.getItem('reader-themeIndex') || '0'))
  const isNight = computed({
    get: () => appStore.theme === 'dark',
    set: (value: boolean) => {
      appStore.setTheme(value ? 'dark' : 'light')
      safeLocalSet('reader-isNight', String(value))
    },
  })

  const currentTheme = computed(() => {
    if (isNight.value) return themePresets[themePresets.length - 1]
    return themePresets[themeIndex.value] || themePresets[0]
  })

  function setThemeIndex(idx: number) {
    themeIndex.value = idx
    isNight.value = false
    safeLocalSet('reader-themeIndex', String(idx))
    safeLocalSet('reader-isNight', 'false')
  }

  function toggleNight() {
    isNight.value = !isNight.value
  }

  /* ─── Replace Rules ─── */
  const replaceRules = ref<ReplaceRule[]>([])

  async function fetchReplaceRules() {
    try {
      replaceRules.value = await getReplaceRules()
    } catch {
      /* ignore */
    }
  }

  /* ─── 正文呈现：替换规则 → 简繁转换 ─── */
  /* scope 匹配与按序应用的纯逻辑在 utils/replaceRules；
     opencc 字典的按需加载与缓存在 utils/openccConvert（拆分专项） */
  function applyReplaceRules(text: string) {
    return applyReplaceRulesToText(text, replaceRules.value, readerStore.book)
  }

  function convertContent(text: string) {
    return convertChineseText(text, config.chineseMode)
  }

  function processContentForDisplay(text: string) {
    return convertContent(applyReplaceRules(text))
  }

  const displayContent = computed(() => {
    return processContentForDisplay(readerStore.content)
  })

  watch(
    () => config.chineseMode,
    (mode) => {
      if (mode === 'traditional') {
        void ensureChineseConverter('traditional')
      }
    },
    { immediate: true }
  )

  return {
    config,
    updateConfig,
    resetConfig,
    saveConfig,
    themeIndex,
    isNight,
    currentTheme,
    setThemeIndex,
    toggleNight,
    replaceRules,
    fetchReplaceRules,
    displayContent,
    processContentForDisplay,
  }
})
