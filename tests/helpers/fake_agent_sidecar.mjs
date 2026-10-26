// 集成测试用的假 sidecar：说 agent sidecar 的 stdio NDJSON 协议（v2），
// 按章节标题/书作者分流行为，验证 Rust 侧的帧泵、工具应答、取消与看门狗。
//
// 行为约定（测试通过书名/章节标题控制）：
// - 章节标题含「跳过」→ result {ok, skipped:true}
// - 章节标题含「搜索」→ 先发 search_read_content 工具调用（toIndex=99），
//   把服务端钳制后的 range 与命中数写进 summary
// - 章节标题含「越权」→ 先发一个 jobId 不匹配的 tool_call（协议 v2 归属校验），
//   把宿主应答的错误内容写进 summary，再走正常流程（验证任务不中断）
// - 章节标题含「卡住」→ 不回帧（供看门狗超时 / 取消测试挂起）
// - 章节标题含「闪退」→ 往 stderr 写一行后 process.exit(1)（供 EOF 错误路径测试）
// - 其他章节 → 发 get_completed_chapter 工具调用，把正文长度累计进 summary
// - 作者名含「地图失败」→ redraw_map 返回错误；否则返回 b64 地图
import { createInterface } from 'node:readline'

const PROTOCOL = 2

// 环境变量白名单回归哨兵：宿主 spawn 必须 env_clear + 白名单透传，
// 子进程不应见到任何服务端凭据（Rust 侧测试设置哨兵后断言 false）
const envLeak = process.env.JWT_SECRET !== undefined || process.env.ADMIN_PASSWORD !== undefined

process.stdout.write(`${JSON.stringify({ type: 'hello', protocol: PROTOCOL, sidecar: 'fake-test' })}\n`)

const rl = createInterface({ input: process.stdin, terminal: false })

let summary = ''

rl.on('line', (line) => {
  if (!line.trim()) return
  let frame
  try {
    frame = JSON.parse(line)
  } catch {
    return
  }
  if (frame.type === 'tool_result') return // 工具应答由 awaitToolResult 分支读取
  if (frame.type !== 'run') return
  console.error(`DBG fake got run frame: kind=${frame.job?.kind} title=${frame.job?.chapter?.title}`)
  handleRun(frame.job ?? {})
})

async function handleRun(job) {
  try {
    if (job.kind === 'redraw_map') {
      const author = String(job.book?.author ?? '')
      if (author.includes('地图失败')) {
        send({ type: 'result', jobId: job.jobId, ok: false, error: '图片模型未配置' })
      } else {
        send({
          type: 'result',
          jobId: job.jobId,
          ok: true,
          map: { b64Json: Buffer.from('fake-png').toString('base64') },
          mapPrompt: String(job.memory?.map?.prompt || '兜底提示词'),
        })
      }
      return
    }

    const chapter = job.chapter ?? {}
    const index = chapter.index ?? 0
    const title = String(chapter.title ?? '')

    if (title.includes('跳过')) {
      send({ type: 'result', jobId: job.jobId, ok: true, skipped: true })
      return
    }

    if (title.includes('卡住')) {
      // 永不回复：供看门狗超时与取消测试使用（进程保持可被 kill）
      return
    }

    if (title.includes('闪退')) {
      // 模拟 sidecar 崩溃：stderr 留痕后 EOF（测试 EOF 错误路径不 panic、任务槽能释放）
      console.error('FAKE_CRASH: simulated sidecar crash')
      process.exit(1)
    }

    if (title.includes('串帧')) {
      // 模拟迟到帧：先发一个 jobId 不匹配的 result（典型是上一章 redraw 超时
      // 降级后迟到的 result，没有 memory 字段），再走正常流程——Rust 侧应
      // 按 jobId 跳过迟到帧，本章仍以真实 result 成功
      send({ type: 'result', jobId: `stale-${job.jobId}`, ok: true, mapPrompt: '迟到的地图帧' })
    }

    if (title.includes('超长行')) {
      // 失控 sidecar：吐一行超过 64MB 上限的“NDJSON”。宿主应在累积阶段
      // 报「单行超过上限」并中止任务，而不是把整行读进内存
      const junk = Buffer.alloc(64 * 1024 * 1024 + 1024, 0x78)
      process.stdout.write(junk)
      process.stdout.write('\n')
      return
    }

    if (title.includes('搜索')) {
      const content = await callTool('search_read_content', {
        keyword: '暗号',
        fromIndex: 0,
        toIndex: 99,
      }, job.jobId)
      summary += `;搜索范围:${content.range?.toIndex};命中:${(content.matches ?? []).length}`
      finishChapter(job, index, title)
      return
    }

    if (title.includes('越权')) {
      // 协议 v2 归属校验：发一个 jobId 不匹配的 tool_call，宿主应回带错误
      // 内容的 tool_result（content.ok=false）并记 warn，任务不中断——
      // 本分支随后仍走正常的取文流程完成本章
      const rogueId = `call-rogue-${Date.now()}`
      send({ type: 'tool_call', id: rogueId, jobId: `bogus-${job.jobId}`, name: 'get_completed_chapter', args: { index } })
      const rejected = await awaitToolResult(rogueId)
      summary += `;越权拒绝:${rejected?.ok === false ? (rejected.error ?? 'ok=false') : '未被拒绝'}`
    }

    const content = await callTool('get_completed_chapter', { index }, job.jobId)
    summary += `;第${index}章正文${content.totalLength}字`
    finishChapter(job, index, title)
  } catch (error) {
    send({ type: 'result', jobId: job.jobId, ok: false, error: String(error) })
  }
}

function finishChapter(job, index, title) {
  // envLeak 同时进 result 顶层与 summary：summary 会被持久化，
  // Rust 侧集成测试通过 getAiBookMemory 观察到它
  summary += `;envLeak:${envLeak}`
  send({
    type: 'result',
    jobId: job.jobId,
    ok: true,
    envLeak,
    memory: {
      bookUrl: job.book?.bookUrl ?? '',
      bookName: job.book?.name ?? '',
      author: job.book?.author ?? '',
      enabled: true,
      summary,
      worldview: [],
      characters: [],
      relationships: [],
      locations: [],
      processedChapterIndex: index,
      processedChapterTitle: title,
      mapDirty: false,
    },
    shouldRegenerateMap: false,
  })
}

function callTool(name, args, jobId) {
  const id = `call-${Date.now()}-${Math.random().toString(16).slice(2, 8)}`
  console.error(`DBG fake sending tool_call ${name}`)
  // 协议 v2：tool_call 帧携带当前 job 的 jobId 供宿主校验归属
  send({ type: 'tool_call', id, jobId, name, args })
  return awaitToolResult(id, jobId).then((content) => {
    if (content?.ok) return content
    throw new Error(content?.error ?? '工具执行失败')
  })
}

function awaitToolResult(id, expectedJobId) {
  return new Promise((resolve, reject) => {
    const timeout = setTimeout(() => reject(new Error('工具应答超时')), 30_000)
    const onLine = (line) => {
      let frame
      try {
        frame = JSON.parse(line)
      } catch {
        return
      }
      if (frame.type === 'tool_result' && frame.id === id) {
        rl.off('line', onLine)
        clearTimeout(timeout)
        // 协议 v2：宿主的 tool_result 必须带回当前任务的 jobId
        if (expectedJobId !== undefined && frame.jobId !== expectedJobId) {
          reject(new Error(`tool_result jobId 不匹配：期望 ${expectedJobId}，收到 ${frame.jobId}`))
          return
        }
        resolve(frame.content)
      }
    }
    rl.on('line', onLine)
  })
}

function send(frame) {
  process.stdout.write(`${JSON.stringify(frame)}\n`)
}
