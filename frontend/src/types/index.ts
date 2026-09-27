// ─── API 统一返回 ───
export interface ApiResponse<T = unknown> {
  isSuccess: boolean
  errorMsg: string
  data: T
}

// ─── 书籍 ───
export interface Book {
  name: string
  author: string
  bookUrl: string
  origin: string
  originName?: string
  coverUrl?: string
  tocUrl?: string
  charset?: string
  customCoverUrl?: string
  canUpdate?: boolean
  durChapterIndex?: number
  durChapterPos?: number
  durChapterTime?: number
  durChapterTitle?: string
  intro?: string
  latestChapterTitle?: string
  lastCheckTime?: number
  totalChapterNum?: number
  type?: number
  group?: number
  wordCount?: string
  infoHtml?: string
  tocHtml?: string
  kind?: string
  updateTime?: string
  cachedChapterCount?: number
  browserCachedChapterCount?: number
}

// ─── 搜索结果 ───
export interface SearchBook {
  name: string
  author: string
  bookUrl: string
  origin: string
  originName?: string
  originGroup?: string
  coverUrl?: string
  intro?: string
  kind?: string
  lastChapter?: string
  updateTime?: string
  wordCount?: string
  bookSourceUrls?: string[]
}

// ─── 章节 ───
export interface BookChapter {
  title: string
  url: string
  index: number
}

// ─── 评论（章评 / 段评） ───
export interface ReviewReply {
  name: string
  content: string
  time: string
  /** 被回复者；为空表示直接回复评论本身 */
  replyTo: string
}

export interface ReviewItem {
  id: string
  name: string
  avatar: string
  content: string
  time: string
  digg: number
  replyCount: number
  replies: ReviewReply[]
  /** 评论配图。站点常给同一张图的多个变体（HEIC + JPEG），由前端挑可渲染的 */
  images: string[]
}

export interface ReviewPage {
  total: number
  hasMore: boolean
  page: number
  items: ReviewItem[]
}

/** 段评概览里的一段：段号 + 该段评论条数 */
export interface ParaReviewCount {
  paraIndex: number
  count: number
  /** 段落原文（截断）。段号会因替换规则/繁简转换漂移，用它兜底定位 */
  text: string
}

export interface ParaReviewIndex {
  paras: ParaReviewCount[]
}

/** 书源支不支持评论由后端判定，前端据此决定是否渲染入口 */
export interface ReviewResponse<T> {
  enabled: boolean
  /**
   * 书源的评论 URL 模板用到了 `{{sort}}`，即排序由站点自己做。
   * 为 false 时「最新」只能对已加载的条目重排。
   */
  serverSort: boolean
  data: T
}

// ─── 书源 ───
export interface BookSource {
  bookSourceName: string
  bookSourceGroup?: string
  bookSourceUrl: string
  bookSourceType?: number
  enabled?: boolean
  enabledExplore?: boolean
  enabledCookieJar?: boolean
  customOrder?: number
  weight?: number
  searchUrl?: string
  exploreUrl?: string
  header?: string
  loginUrl?: string
  loginCheckJs?: string
  loadWithBaseUrl?: boolean
  singleUrl?: boolean
  ruleSearch?: Record<string, unknown>
  ruleExplore?: Record<string, unknown>
  ruleBookInfo?: Record<string, unknown>
  ruleToc?: Record<string, unknown>
  ruleContent?: Record<string, unknown>
  ruleReview?: Record<string, unknown>
  ruleParaReview?: Record<string, unknown>
}

export interface BookSourceTestResult {
  bookSourceName: string
  bookSourceUrl: string
  valid: boolean
  searchOk: boolean
  exploreOk: boolean
  keyword: string
  exploreUrl?: string
  searchError?: string
  exploreError?: string
  markedInvalid: boolean
  group?: string
}

export interface BookSourceTestResponse {
  total: number
  valid: number
  invalid: number
  markedInvalid: number
  results: BookSourceTestResult[]
}

// ─── 分组 ───
export interface BookGroup {
  groupId: number
  groupName: string
  orderNo?: number
}

// ─── 用户 ───
export interface UserInfo {
  username: string
  lastLoginAt?: number
  /// 仅在登录与改密码的响应中出现；服务端不再回吐任何凭据
  accessToken?: string
  createdAt?: number
}


// ─── 书签 ───
export interface Bookmark {
  time?: number
  bookName: string
  bookAuthor: string
  chapterIndex?: number
  chapterPos?: number
  chapterName?: string
  bookText?: string
  content?: string
}

// ─── 净化规则 ───
export interface ReplaceRule {
  id: number
  name: string
  group?: string
  pattern: string
  replacement: string
  scope?: string
  isEnabled: boolean
  isRegex: boolean
  order: number
}

// ─── AI 设定集 ───
export interface AiBookConfig {
  modelSource: 'browser' | 'server'
  textBaseUrl: string
  textApiKey: string
  textModel: string
  textUseFullUrl: boolean
  imageBaseUrl: string
  imageApiKey: string
  imageModel: string
  imageSize: string
  imageUseFullUrl: boolean
  useBackendProxy: boolean
}

export interface AiModelEndpointConfig {
  enabled: boolean
  baseUrl: string
  apiKey: string
  model: string
  useFullUrl: boolean
}

export interface AiImageModelConfig extends AiModelEndpointConfig {
  imageSize: string
}

export interface AiSpeechModelConfig extends AiModelEndpointConfig {
  voice: string
  responseFormat: string
}

export interface AiServerModelConfig {
  text: AiModelEndpointConfig
  image: AiImageModelConfig
  speech: AiSpeechModelConfig
}

export interface AiServerModelConfigResponse {
  config: AiServerModelConfig
  canUseServerModel: boolean
  isAdmin: boolean
}

export interface AiBookNote {
  title: string
  content: string
  category?: string
  confidence?: string
  importance?: string
}

export interface AiBookCharacter {
  name: string
  aliases?: string[]
  status: string
  faction?: string
  location?: string
  description?: string
  lastSeenChapter?: string
  importance?: string
}

export interface AiBookRelationship {
  source: string
  target: string
  relation: string
  status?: string
  description?: string
  importance?: string
}

export interface AiBookLocation {
  name: string
  kind?: string
  parentName?: string
  description: string
  status?: string
  relatedCharacters?: string[]
  firstSeenChapter?: string
  importance?: string
}

export interface AiBookMap {
  imageUrl?: string
  prompt?: string
  updatedAt?: number
  sourceChapterIndex?: number
  fallback?: 'relationship-graph'
  fallbackReason?: string
}

export interface AiBookMemory {
  bookUrl: string
  bookName?: string
  author?: string
  enabled: boolean
  processedChapterIndex?: number
  processedChapterTitle?: string
  updatedAt: number
  summary?: string
  worldview: AiBookNote[]
  characters: AiBookCharacter[]
  relationships: AiBookRelationship[]
  locations: AiBookLocation[]
  map?: AiBookMap | null
  mapDirty?: boolean
  lastError?: string
}

export interface AiBookModelUpdate {
  memory: AiBookMemory
  shouldRegenerateMap: boolean
  mapPrompt?: string
}

/** 章节配图：来自书源的配图规则（正文 HTML 里内嵌的 <img> 不走这里） */
export interface ChapterImage {
  url: string
  /** 图片说明文字（番茄是「配图（画师：奈月Oo）」） */
  caption: string
  /**
   * 插入位置：正文按 `\n` 切分后的行号（从 0 开始，插在该行之前）。
   * 为空表示站点没给位置，排在章末。
   */
  paraIndex: number | null
  /** 原始尺寸，用来给图片占位；站点没给就是 0 */
  width: number
  height: number
}

/** 一章的配图；`enabled: false` 表示书源没声明配图规则 */
export interface ChapterImages {
  enabled: boolean
  images: ChapterImage[]
}
