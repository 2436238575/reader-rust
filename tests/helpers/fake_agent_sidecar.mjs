// 集成测试用的假 sidecar：说 agent sidecar 的 stdio NDJSON 协议，
// 按章节标题/书作者分流行为，验证 Rust 侧的帧泵、工具应答、取消与看门狗。
//
// 行为约定（测试通过书名/章节标题控制）：
// - 章节标题含「跳过」→ result {ok, skipped:true}
// - 章节标题含「搜索」→ 先发 search_read_content 工具调用（toIndex=99），
//   把服务端钳制后的 range 与命中数写进 summary
// - 章节标题含「卡住」→ 不回帧（供看门狗超时 / 取消测试挂起）
// - 其他章节 → 发 get_completed_chapter 工具调用，把正文长度累计进 summary
// - 作者名含「地图失败」→ redraw_map 返回错误；否则返回 b64 地图
import { createInterface } from 'node:readline'

const PROTOCOL = 1

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

    if (title.includes('搜索')) {
      const content = await callTool('search_read_content', {
        keyword: '暗号',
        fromIndex: 0,
        toIndex: 99,
      })
      summary += `;搜索范围:${content.range?.toIndex};命中:${(content.matches ?? []).length}`
      finishChapter(job, index, title)
      return
    }

    const content = await callTool('get_completed_chapter', { index })
    summary += `;第${index}章正文${content.totalLength}字`
    finishChapter(job, index, title)
  } catch (error) {
    send({ type: 'result', jobId: job.jobId, ok: false, error: String(error) })
  }
}

function finishChapter(job, index, title) {
  send({
    type: 'result',
    jobId: job.jobId,
    ok: true,
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

function callTool(name, args) {
  return new Promise((resolve, reject) => {
    const id = `call-${Date.now()}-${Math.random().toString(16).slice(2, 8)}`
    console.error(`DBG fake sending tool_call ${name}`)
    send({ type: 'tool_call', id, name, args })
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
        if (frame.content?.ok) resolve(frame.content)
        else reject(new Error(frame.content?.error ?? '工具执行失败'))
      }
    }
    rl.on('line', onLine)
  })
}

function send(frame) {
  process.stdout.write(`${JSON.stringify(frame)}\n`)
}
