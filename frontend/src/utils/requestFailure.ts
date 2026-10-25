/**
 * 把请求失败翻译成用户可读的原因。
 *
 * axios 层约定（见 `api/http.ts`）：后端给了响应（4xx/5xx）时错误对象带
 * `status`，message 是后端的 errorMsg（用户可读，如「无法连接上游站点」）；
 * 拿不到响应（断网、服务器不可达、超时）时没有 `status`，message 是英文的
 * "Network Error" 之类，不能直接摆给用户。
 */
export function describeRequestFailure(error: unknown, isOnline: boolean): string {
  if (!isOnline) return '离线中'
  const err = error as (Error & { status?: number }) | null | undefined
  if (typeof err?.status === 'number' && err.message) return err.message
  return '无法连接服务器'
}
