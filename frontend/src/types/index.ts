// ─── API 统一返回 ───
export interface ApiResponse<T = unknown> {
  isSuccess: boolean
  errorMsg: string
  /** 后端是 Option<T>：isSuccess=false 时恒为 null（http.ts 失败分支已 reject，不会消费到） */
  data: T | null
}

// ─── 书籍 ───
export interface Book {
  /** 后端分配的稳定身份（`book_<n>`），不随换源变化；请求一律用它定位书籍 */
  bookId?: string
  /** 当前绑定的书源 id */
  bookSourceId?: number
  name: string
  author: string
  /** 上游地址（数据字段，爬虫取数/展示用；不再作为请求身份参数） */
  bookUrl: string
  /** 书源 URL（数据字段，展示/调试用） */
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
}

/** 书架视图注入前端合成字段后的 Book（后端不下发这些字段）。 */
export interface ShelfBook extends Book {
  /** 浏览器本地已缓存章节数（前端按 IndexedDB 统计注入） */
  browserCachedChapterCount?: number
}

// ─── 搜索结果 ───
/** 搜索结果的展示形态：originName/originGroup 是前端按命中书源就地合成的，
 *  后端响应里没有这两个字段（勿当线上字段依赖）。 */
export interface SearchBookDisplay extends SearchBook {
  originName?: string
  originGroup?: string
}

export interface SearchBook {
  /** 产出即登记的 bookId；详情/目录/正文/入架一律用它 */
  bookId?: string
  /** 命中书源的 id */
  bookSourceId?: number
  name: string
  author: string
  bookUrl: string
  origin: string
  coverUrl?: string
  intro?: string
  kind?: string
  lastChapter?: string
  updateTime?: string
  wordCount?: string
  /** 聚合结果里同一本书命中的各书源 id */
  bookSourceIds?: number[]
}

// ─── 章节 ───
export interface BookChapter {
  title: string
  url: string
  index: number
  /** 章节时间（书源 updateTime 规则；纯数字时间戳由 formatChapterTag 转日期） */
  tag?: string | null
  isVip?: boolean
  isPay?: boolean
  isVolume?: boolean
  /** 卷名（书源 volumeName 规则）：相同卷名的连续章节在目录里归为一组 */
  volume?: string | null
  /** 字数（书源 wordCount 规则） */
  wordCount?: string | null
}

// ─── 评论（章评 / 段评） ───
export interface ReviewReply {
  name: string
  content: string
  time: string
  /** 被回复者；为空表示直接回复评论本身 */
  replyTo: string
  /** 回复者是作者本人（书源没配规则时缺省 false） */
  author?: boolean
  /** 作者赞过这条回复 */
  authorDigg?: boolean
  /** 回复配图（已经过图片管道收口为 /reader3/image/<id>） */
  images?: string[]
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
  /** 评论者是作者本人（书源没配规则时缺省 false） */
  author?: boolean
  /** 作者赞过这条评论 */
  authorDigg?: boolean
  /** 评论配图。后端已挑好可渲染的格式，通常只有一个元素（`/reader3/image/<id>`） */
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
  /** 该段有作者评论/点赞（书源没配规则时缺省为 false） */
  authorCommented?: boolean
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
  /** 书源配了作者标记规则，可以走「只看作者」 */
  authorMarks?: boolean
  data: T
}

// ─── 书源 ───

/** 以下 rule* 接口与 src/model/rule.rs 一一对应（全部字段可选的规则文本）。 */
// 书源与规则类型由后端 ts-rs 生成（src/model/{book_source,rule}.rs，
// `cargo test` 时刷新，勿手改）：与线上一致。可视化编辑器的字段配置
// 键经 SourceKeyPath 对这些类型编译期校验（draft 本体是 JSON 树编辑的
// 动态容器，保持 Record<string, unknown> 是刻意的）。
export type { SearchRule } from './generated/SearchRule'
export type { SearchRule as ExploreRule } from './generated/SearchRule'
export type { BookInfoRule } from './generated/BookInfoRule'
export type { TocRule } from './generated/TocRule'
export type { ContentRule } from './generated/ContentRule'
export type { ReviewRule } from './generated/ReviewRule'
export type { ParaReviewRule } from './generated/ParaReviewRule'
export type { ContentImageRule } from './generated/ContentImageRule'
export type { BookSource } from './generated/BookSource'
export type { BookSourceSummary } from './generated/BookSourceSummary'

import type { BookSource as BookSourceType } from './generated/BookSource'
import type { SearchRule as SearchRuleType } from './generated/SearchRule'
import type { BookInfoRule as BookInfoRuleType } from './generated/BookInfoRule'
import type { TocRule as TocRuleType } from './generated/TocRule'
import type { ContentRule as ContentRuleType } from './generated/ContentRule'
import type { ReviewRule as ReviewRuleType } from './generated/ReviewRule'
import type { ParaReviewRule as ParaReviewRuleType } from './generated/ParaReviewRule'
import type { ContentImageRule as ContentImageRuleType } from './generated/ContentImageRule'

/** 书源可视化编辑器的合法字段路径：顶层键 + 各规则对象的「rule 名.字段名」。
 *  写错字段名会在编译期报错（此前 key 是纯 string，拼错静默落空）。 */
export type SourceKeyPath =
  | (keyof BookSourceType & string)
  | `ruleSearch.${keyof SearchRuleType & string}`
  | `ruleExplore.${keyof SearchRuleType & string}`
  | `ruleBookInfo.${keyof BookInfoRuleType & string}`
  | `ruleToc.${keyof TocRuleType & string}`
  | `ruleContent.${keyof ContentRuleType & string}`
  | `ruleReview.${keyof ReviewRuleType & string}`
  | `ruleParaReview.${keyof ParaReviewRuleType & string}`
  | `ruleContentImage.${keyof ContentImageRuleType & string}`

export interface BookSourceTestResult {
  bookSourceId?: number
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
  /// 登录/改密的 JSON 仍返回，供 API 客户端用 Authorization；浏览器不保存，
  /// 会话由 HttpOnly Cookie 承载
  accessToken?: string
  /// Access Token 有效秒数，前端据此安排主动续期
  accessTokenExpiresIn?: number
  createdAt?: number
}

// ─── 书签 ───
export interface Bookmark {
  /** 所属书籍的 bookId；身份 = bookId + chapterIndex + chapterPos + kind（一书可多条） */
  bookId: string
  time?: number
  bookName: string
  bookAuthor: string
  chapterIndex?: number
  /** 万分比章内进度（0–10000）：章书签恒 0，划线取收藏那一刻的章内进度 */
  chapterPos?: number
  chapterName?: string
  bookText?: string
  content?: string
  /** 类型：章书签 / 划线（选文字收藏）；缺省按章书签处理 */
  kind?: 'bookmark' | 'underline'
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

/**
 * 后端模型配置只回报「是否可用」的布尔状态——配置本体（地址/Key/模型名）
 * 只存在于服务端 env，从不下发到浏览器。
 */
export interface AiServerModelStatus {
  canUseServerModel: boolean
  textReady: boolean
  imageReady: boolean
  speechReady: boolean
  /** sidecar（AI 资料编排）是否就绪：提示用，失败不影响其他 AI 功能 */
  agentReady: boolean
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
  /** 所属书籍的 bookId（存储主键） */
  bookId?: string
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
