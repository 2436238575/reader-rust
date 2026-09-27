/**
 * localStorage 写入的安全版本：配额写满（QuotaExceededError）或隐私模式下
 * setItem 会抛异常，阅读进度这类「尽力而为」的持久化不该因此炸掉主流程。
 * 返回是否写入成功，调用方据此决定是否降级。
 */
export function safeLocalSet(key: string, value: string): boolean {
  try {
    localStorage.setItem(key, value)
    return true
  } catch {
    return false
  }
}
