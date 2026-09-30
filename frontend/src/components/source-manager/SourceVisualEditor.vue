<template>
  <div class="visual-editor">
    <div v-if="parseError" class="parse-error">
      JSON 暂时无法解析，请先在「JSON」标签页修正后再用可视化编辑。
    </div>

    <template v-else>
      <section v-for="sec in sections" :key="sec.title" class="field-section">
        <button class="section-head" type="button" @click="toggleSection(sec.title)">
          <svg
            class="chev"
            :class="{ open: expandedSections.has(sec.title) }"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            stroke-width="2"
          >
            <path d="m9 6 6 6-6 6" />
          </svg>
          <span class="section-title">{{ sec.title }}</span>
          <span v-if="filledCount(sec) > 0" class="section-count">{{ filledCount(sec) }}</span>
        </button>

        <div v-show="expandedSections.has(sec.title)" class="section-body">
          <template v-for="f in sec.fields" :key="f.key">
            <!-- 布尔开关：缺省即 true（enabled 家族），勾选时删除字段回到默认值 -->
            <label v-if="f.type === 'checkbox'" class="field-row checkbox-row">
              <span class="field-label">{{ f.label }}</span>
              <input
                type="checkbox"
                :checked="getVal(f.key) !== false"
                @change="onCheckbox(f.key, ($event.target as HTMLInputElement).checked)"
              />
            </label>

            <label v-else-if="f.type === 'select'" class="field-row">
              <span class="field-label">{{ f.label }}</span>
              <select
                class="field-input"
                :value="numVal(f.key)"
                @change="setVal(f.key, Number(($event.target as HTMLSelectElement).value))"
              >
                <option v-for="opt in f.options" :key="opt.value" :value="opt.value">
                  {{ opt.label }}
                </option>
              </select>
            </label>

            <label v-else-if="f.type === 'number'" class="field-row">
              <span class="field-label">{{ f.label }}</span>
              <input
                class="field-input"
                type="number"
                :value="numOrEmpty(f.key)"
                :placeholder="f.placeholder"
                @input="onNumber(f.key, ($event.target as HTMLInputElement).value)"
              />
            </label>

            <div v-else-if="f.type === 'textarea'" class="field-row textarea-row">
              <span class="field-label">{{ f.label }}</span>
              <textarea
                class="field-input"
                rows="2"
                :value="strVal(f.key)"
                :placeholder="f.placeholder"
                spellcheck="false"
                @input="setVal(f.key, ($event.target as HTMLTextAreaElement).value)"
              ></textarea>
            </div>

            <label v-else class="field-row">
              <span class="field-label">{{ f.label }}</span>
              <input
                class="field-input"
                type="text"
                :value="strVal(f.key)"
                :placeholder="f.placeholder"
                spellcheck="false"
                @input="setVal(f.key, ($event.target as HTMLInputElement).value)"
              />
            </label>
          </template>
        </div>
      </section>
    </template>
  </div>
</template>

<script setup lang="ts">
import { ref, watch } from 'vue'
import { useAppStore } from '../../stores/app'

type FieldType = 'text' | 'textarea' | 'number' | 'checkbox' | 'select'

type FieldDef = {
  key: string // 点路径，如 ruleSearch.name
  label: string
  type?: FieldType
  placeholder?: string
  options?: { value: number; label: string }[]
}

type SectionDef = {
  title: string
  fields: FieldDef[]
}

const props = defineProps<{
  editorText: string
}>()

const emit = defineEmits<{
  'update:editorText': [value: string]
}>()

const appStore = useAppStore()

// 字段口径：docs/reference/book-source-rules.md §2.1/2.2（含本项目扩展的章评/段评/配图）
const sections: SectionDef[] = [
  {
    title: '基础',
    fields: [
      { key: 'bookSourceName', label: '源名称', placeholder: '会显示在源列表' },
      {
        key: 'bookSourceUrl',
        label: '源域名',
        placeholder: '通常填写网站主页，例: https://www.qidian.com',
      },
      { key: 'bookSourceGroup', label: '源分组', placeholder: '描述源的特征信息' },
      {
        key: 'bookSourceType',
        label: '源类型',
        type: 'select',
        options: [
          { value: 0, label: '文本' },
          { value: 1, label: '音频' },
          { value: 2, label: '图片' },
          { value: 3, label: '文件' },
        ],
      },
      { key: 'bookSourceComment', label: '源注释', placeholder: '描述源作者和状态' },
      { key: 'loginUrl', label: '登录地址', placeholder: '填写网站登录网址，仅在需要登录的源有用' },
      { key: 'loginUi', label: '登录界面', placeholder: '自定义登录界面' },
      { key: 'loginCheckJs', label: '登录检测', placeholder: '登录检测 js' },
      { key: 'coverDecodeJs', label: '封面解密', placeholder: '封面解密 js' },
      {
        key: 'bookUrlPattern',
        label: '链接验证',
        placeholder: '书籍URL正则，当详情页URL与源URL的域名不一致时有效，用于添加网址',
      },
      { key: 'header', label: '请求头', type: 'textarea', placeholder: '客户端标识' },
      { key: 'variableComment', label: '变量说明', placeholder: '书源变量说明' },
      {
        key: 'concurrentRate',
        label: '并发率',
        placeholder: '并发率，如1000(访问间隔1000ms)或者1/1000(1000ms内访问1次)',
      },
      {
        key: 'jsLib',
        label: 'js库',
        type: 'textarea',
        placeholder: 'js库, 可填写js或者key-value object获取在线js文件',
      },
    ],
  },
  {
    title: '搜索',
    fields: [
      { key: 'searchUrl', label: '搜索地址', placeholder: '[域名可省略]/search.php@kw={{key}}' },
      { key: 'ruleSearch.checkKeyWord', label: '校验文字', placeholder: '校验关键字，强烈建议填写' },
      { key: 'ruleSearch.bookList', label: '列表规则', placeholder: '选择书籍节点 (规则结果为List)' },
      { key: 'ruleSearch.name', label: '书名规则' },
      { key: 'ruleSearch.author', label: '作者规则' },
      { key: 'ruleSearch.kind', label: '分类规则' },
      { key: 'ruleSearch.wordCount', label: '字数规则' },
      { key: 'ruleSearch.lastChapter', label: '最新章节' },
      { key: 'ruleSearch.intro', label: '简介规则' },
      { key: 'ruleSearch.coverUrl', label: '封面规则' },
      { key: 'ruleSearch.bookUrl', label: '详情地址' },
    ],
  },
  {
    title: '发现',
    fields: [
      { key: 'exploreUrl', label: '发现地址', placeholder: '分类列表，换行或&&分隔' },
      { key: 'ruleExplore.bookList', label: '列表规则', placeholder: '选择书籍节点 (规则结果为List)' },
      { key: 'ruleExplore.name', label: '书名规则' },
      { key: 'ruleExplore.author', label: '作者规则' },
      { key: 'ruleExplore.kind', label: '分类规则' },
      { key: 'ruleExplore.wordCount', label: '字数规则' },
      { key: 'ruleExplore.lastChapter', label: '最新章节' },
      { key: 'ruleExplore.intro', label: '简介规则' },
      { key: 'ruleExplore.coverUrl', label: '封面规则' },
      { key: 'ruleExplore.bookUrl', label: '详情地址' },
    ],
  },
  {
    title: '详情',
    fields: [
      { key: 'ruleBookInfo.init', label: '预处理' },
      { key: 'ruleBookInfo.name', label: '书名规则' },
      { key: 'ruleBookInfo.author', label: '作者规则' },
      { key: 'ruleBookInfo.kind', label: '分类规则' },
      { key: 'ruleBookInfo.wordCount', label: '字数规则' },
      { key: 'ruleBookInfo.lastChapter', label: '最新章节' },
      { key: 'ruleBookInfo.intro', label: '简介规则' },
      { key: 'ruleBookInfo.coverUrl', label: '封面规则' },
      { key: 'ruleBookInfo.tocUrl', label: '目录地址' },
      { key: 'ruleBookInfo.canReName', label: '修改书籍' },
      { key: 'ruleBookInfo.downloadUrls', label: '下载URL' },
    ],
  },
  {
    title: '目录',
    fields: [
      { key: 'ruleToc.preUpdateJs', label: '更新前JS' },
      { key: 'ruleToc.init', label: '预定位', placeholder: '本项目扩展：先把范围缩到列表容器' },
      { key: 'ruleToc.chapterList', label: '列表规则' },
      { key: 'ruleToc.chapterName', label: '章节名称' },
      { key: 'ruleToc.chapterUrl', label: '章节地址' },
      { key: 'ruleToc.formatJs', label: '标题处理' },
      { key: 'ruleToc.isVolume', label: '卷名标识' },
      { key: 'ruleToc.isVip', label: '收费标识' },
      { key: 'ruleToc.isPay', label: '购买标识' },
      { key: 'ruleToc.updateTime', label: '章节信息' },
      { key: 'ruleToc.nextTocUrl', label: '翻页规则' },
    ],
  },
  {
    title: '正文',
    fields: [
      { key: 'ruleContent.content', label: '正文规则', type: 'textarea' },
      { key: 'ruleContent.title', label: '标题规则' },
      { key: 'ruleContent.nextContentUrl', label: '翻页规则' },
      { key: 'ruleContent.webJs', label: '脚本注入' },
      { key: 'ruleContent.sourceRegex', label: '资源正则' },
      { key: 'ruleContent.replaceRegex', label: '替换规则', type: 'textarea' },
      { key: 'ruleContent.imageStyle', label: '图片样式' },
      { key: 'ruleContent.imageDecode', label: '图片解密' },
      { key: 'ruleContent.payAction', label: '购买操作' },
    ],
  },
  {
    title: '章评',
    fields: [
      { key: 'ruleReview.reviewUrl', label: '评论地址', placeholder: '对章节正文响应求值' },
      { key: 'ruleReview.listRule', label: '列表规则' },
      { key: 'ruleReview.idRule', label: '评论ID' },
      { key: 'ruleReview.nameRule', label: '昵称' },
      { key: 'ruleReview.avatarRule', label: '头像' },
      { key: 'ruleReview.contentRule', label: '内容' },
      { key: 'ruleReview.postTimeRule', label: '时间' },
      { key: 'ruleReview.diggRule', label: '点赞数' },
      { key: 'ruleReview.replyCountRule', label: '回复数' },
      { key: 'ruleReview.totalRule', label: '总数' },
      { key: 'ruleReview.hasMoreRule', label: '是否更多' },
      { key: 'ruleReview.replyToRule', label: '回复对象' },
      { key: 'ruleReview.authorRule', label: '作者标记' },
      { key: 'ruleReview.authorDiggRule', label: '作者点赞' },
      { key: 'ruleReview.imageRule', label: '配图' },
      { key: 'ruleReview.replyListRule', label: '回复列表' },
      { key: 'ruleReview.replyNameRule', label: '回复昵称' },
      { key: 'ruleReview.replyContentRule', label: '回复内容' },
      { key: 'ruleReview.replyPostTimeRule', label: '回复时间' },
      { key: 'ruleReview.replyAuthorRule', label: '回复作者标记' },
      { key: 'ruleReview.replyAuthorDiggRule', label: '回复作者点赞' },
    ],
  },
  {
    title: '段评',
    fields: [
      { key: 'ruleParaReview.indexUrl', label: '索引地址', placeholder: '对章节正文响应求值' },
      { key: 'ruleParaReview.indexListRule', label: '索引列表' },
      { key: 'ruleParaReview.indexCountRule', label: '段评数' },
      { key: 'ruleParaReview.indexAuthorCommentedRule', label: '作者评论标记' },
      { key: 'ruleParaReview.reviewUrl', label: '评论地址' },
      { key: 'ruleParaReview.listRule', label: '列表规则' },
      { key: 'ruleParaReview.idRule', label: '评论ID' },
      { key: 'ruleParaReview.nameRule', label: '昵称' },
      { key: 'ruleParaReview.avatarRule', label: '头像' },
      { key: 'ruleParaReview.contentRule', label: '内容' },
      { key: 'ruleParaReview.postTimeRule', label: '时间' },
      { key: 'ruleParaReview.diggRule', label: '点赞数' },
      { key: 'ruleParaReview.totalRule', label: '总数' },
      { key: 'ruleParaReview.hasMoreRule', label: '是否更多' },
      { key: 'ruleParaReview.replyCountRule', label: '回复数' },
      { key: 'ruleParaReview.replyToRule', label: '回复对象' },
      { key: 'ruleParaReview.authorRule', label: '作者标记' },
      { key: 'ruleParaReview.authorDiggRule', label: '作者点赞' },
      { key: 'ruleParaReview.imageRule', label: '配图' },
      { key: 'ruleParaReview.replyListRule', label: '回复列表' },
      { key: 'ruleParaReview.replyNameRule', label: '回复昵称' },
      { key: 'ruleParaReview.replyContentRule', label: '回复内容' },
      { key: 'ruleParaReview.replyPostTimeRule', label: '回复时间' },
      { key: 'ruleParaReview.replyAuthorRule', label: '回复作者标记' },
      { key: 'ruleParaReview.replyAuthorDiggRule', label: '回复作者点赞' },
    ],
  },
  {
    title: '章节配图',
    fields: [
      { key: 'ruleContentImage.imageUrl', label: '配图地址', placeholder: '对章节正文响应求值' },
      { key: 'ruleContentImage.listRule', label: '列表规则' },
      { key: 'ruleContentImage.urlRule', label: '图片地址' },
      { key: 'ruleContentImage.captionRule', label: '说明文字' },
      { key: 'ruleContentImage.paraIndexRule', label: '插入位置' },
      { key: 'ruleContentImage.widthRule', label: '宽度' },
      { key: 'ruleContentImage.heightRule', label: '高度' },
    ],
  },
  {
    title: '其他',
    fields: [
      { key: 'enabled', label: '启用搜索', type: 'checkbox' },
      { key: 'enabledExplore', label: '启用发现', type: 'checkbox' },
      { key: 'enabledCookieJar', label: 'CookieJar', type: 'checkbox' },
      { key: 'weight', label: '搜索权重', type: 'number' },
      { key: 'customOrder', label: '排序编号', type: 'number' },
    ],
  },
]

const draft = ref<Record<string, unknown>>({})
const parseError = ref(false)
const expandedSections = ref<Set<string>>(new Set(['基础']))

watch(
  () => props.editorText,
  (text) => {
    let parsed: unknown
    try {
      parsed = JSON.parse(text)
    } catch {
      parseError.value = true
      return
    }
    if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) {
      parseError.value = true
      return
    }
    parseError.value = false
    // 自己发出的事件绕回时内容不变，跳过重赋值（避免输入框光标被重置）
    if (JSON.stringify(parsed) === JSON.stringify(draft.value)) return
    draft.value = parsed as Record<string, unknown>
    // 走到这里一定是外部变更（选书源/JSON 页编辑/格式化）：把有内容的分组自动展开
    expandedSections.value = new Set(
      sections.filter((sec) => sec.title === '基础' || filledCount(sec) > 0).map((s) => s.title)
    )
  },
  { immediate: true }
)

function isPlainObject(value: unknown): value is Record<string, unknown> {
  return !!value && typeof value === 'object' && !Array.isArray(value)
}

function getVal(path: string): unknown {
  let cur: unknown = draft.value
  for (const key of path.split('.')) {
    if (!isPlainObject(cur)) return undefined
    cur = cur[key]
  }
  return cur
}

function strVal(path: string): string {
  const v = getVal(path)
  return typeof v === 'string' ? v : v == null ? '' : String(v)
}

function numVal(path: string): number {
  const v = getVal(path)
  return typeof v === 'number' && Number.isFinite(v) ? v : 0
}

function numOrEmpty(path: string): number | '' {
  const v = getVal(path)
  return typeof v === 'number' && Number.isFinite(v) ? v : ''
}

function setVal(path: string, value: unknown) {
  const keys = path.split('.')
  let obj: Record<string, unknown> = draft.value
  for (let i = 0; i < keys.length - 1; i++) {
    const cur = obj[keys[i]]
    if (cur === undefined || cur === null) {
      obj[keys[i]] = {}
    } else if (!isPlainObject(cur)) {
      // 旧格式里规则对象可能是字符串等非对象值：不覆写，提示去 JSON 页处理
      appStore.showToast(`「${keys[i]}」当前不是规则对象，请到 JSON 标签页修改`, 'warning')
      return
    }
    obj = obj[keys[i]] as Record<string, unknown>
  }
  const last = keys[keys.length - 1]
  if (value === '' || value === undefined || value === null) {
    delete obj[last]
  } else {
    obj[last] = value
  }
  // 子字段清空后规则对象已空：连同对象一起删掉，保持 JSON 干净
  if (keys.length > 1) {
    const parent = getVal(keys.slice(0, -1).join('.'))
    if (isPlainObject(parent) && Object.keys(parent).length === 0) {
      const pKeys = keys.slice(0, -1)
      let p: Record<string, unknown> = draft.value
      for (let i = 0; i < pKeys.length - 1; i++) p = p[pKeys[i]] as Record<string, unknown>
      delete p[pKeys[pKeys.length - 1]]
    }
  }
  emit('update:editorText', JSON.stringify(draft.value, null, 2))
}

function onCheckbox(path: string, checked: boolean) {
  // enabled 家族缺省即 true：勾选恢复默认（删字段），取消勾选才显式写 false
  setVal(path, checked ? undefined : false)
}

function onNumber(path: string, raw: string) {
  if (raw.trim() === '') {
    setVal(path, undefined)
    return
  }
  const num = Number(raw)
  if (Number.isFinite(num)) setVal(path, Math.trunc(num))
}

function filledCount(sec: SectionDef): number {
  return sec.fields.reduce((n, f) => {
    const v = getVal(f.key)
    return v !== undefined && v !== null && v !== '' ? n + 1 : n
  }, 0)
}

function toggleSection(title: string) {
  if (expandedSections.value.has(title)) {
    expandedSections.value.delete(title)
  } else {
    expandedSections.value.add(title)
  }
}
</script>

<style scoped>
.visual-editor {
  display: flex;
  flex-direction: column;
  gap: 10px;
}

.parse-error {
  padding: var(--space-4);
  border-radius: var(--radius-md);
  background: rgba(201, 127, 58, 0.12);
  border: 1px solid rgba(201, 127, 58, 0.18);
  color: var(--color-text-secondary);
  font-size: var(--text-sm);
}

.field-section {
  border: 1px solid var(--color-border-light);
  border-radius: var(--radius-lg);
  background: var(--color-bg);
  overflow: hidden;
}

.section-head {
  width: 100%;
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 10px 12px;
  text-align: left;
  color: var(--color-text);
  font-size: var(--text-sm);
  font-weight: 600;
}

.section-head:hover {
  background: var(--color-bg-hover);
}

.chev {
  width: 14px;
  height: 14px;
  color: var(--color-text-tertiary);
  transition: transform var(--duration-fast) var(--ease-out);
}

.chev.open {
  transform: rotate(90deg);
}

.section-title {
  flex: 1;
}

.section-count {
  padding: 1px 8px;
  border-radius: var(--radius-full);
  background: var(--color-primary-bg);
  color: var(--color-primary-dark);
  font-size: var(--text-xs);
  font-weight: 500;
}

.section-body {
  padding: 4px 12px 12px;
  display: flex;
  flex-direction: column;
  gap: 8px;
  border-top: 1px solid var(--color-divider);
}

.field-row {
  display: flex;
  align-items: center;
  gap: 10px;
  margin-top: 8px;
}

.field-label {
  width: 64px;
  flex-shrink: 0;
  color: var(--color-text-tertiary);
  font-size: var(--text-xs);
}

.field-input {
  flex: 1;
  min-width: 0;
  min-height: 32px;
  padding: 0 10px;
  border-radius: var(--radius-md);
  border: 1px solid var(--color-border);
  background: var(--color-bg-elevated);
  color: inherit;
  font-size: var(--text-xs);
}

.field-input:focus {
  outline: none;
  border-color: var(--color-primary);
}

.field-input::placeholder {
  color: var(--color-text-tertiary);
  opacity: 0.7;
}

.textarea-row {
  align-items: flex-start;
}

.textarea-row .field-label {
  padding-top: 8px;
}

textarea.field-input {
  padding: 7px 10px;
  line-height: 1.5;
  resize: vertical;
  font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
}

.checkbox-row input[type='checkbox'] {
  width: 16px;
  height: 16px;
  accent-color: var(--color-primary);
}

@media (max-width: 560px) {
  .field-row {
    align-items: flex-start;
    flex-direction: column;
    gap: 4px;
  }

  .field-input {
    width: 100%;
  }

  .checkbox-row {
    flex-direction: row;
    align-items: center;
  }
}
</style>
