/**
 * 会话代际：登出/清会话时自增。HTTP 拦截器在请求发出时记录代际，
 * 401 时只有代际未变才允许触发静默刷新——否则「登出瞬间返回的 401」
 * 会把刚清掉的 Cookie 又重新换回来。
 */

let epoch = 0

export function currentSessionEpoch() {
  return epoch
}

/** 登出/清会话入口调用：让所有在途请求的 401 直接失败，不再刷新。 */
export function invalidateSessionForRetry() {
  epoch += 1
}

/** 测试用：重置代际。 */
export function resetSessionEpochForTest() {
  epoch = 0
}
