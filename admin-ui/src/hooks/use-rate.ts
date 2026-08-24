import { useCallback, useEffect, useRef, useState } from 'react'
import { getRate } from '@/api/stats'
import type { RateSnapshot } from '@/types/api'

/** 轮询间隔。后端速率按整分钟聚合，比这更快只会重复读同一个桶。 */
const POLL_MS = 15_000

/**
 * 分钟级 RPM / TPM。
 *
 * 刻意与 `useStats` 分开：速率要持续轮询，而概览/趋势不需要——合在一起会让整个
 * 仪表盘每 15 秒重新拉一遍。
 *
 * `unavailable` 与 `error` 分开返回：后端在采集层未注入时回 503，那是"没装"，
 * 与"请求失败"和"没流量"都不是一回事，前端要能分别显示。
 */
export function useRate() {
  const [snapshot, setSnapshot] = useState<RateSnapshot | null>(null)
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const [unavailable, setUnavailable] = useState(false)
  /** 首次加载才显示骨架屏；轮询时静默更新，避免每 15 秒闪一下。 */
  const loadedOnce = useRef(false)

  const reload = useCallback(async () => {
    try {
      const data = await getRate()
      setSnapshot(data)
      setUnavailable(false)
      setError(null)
    } catch (e) {
      const status = (e as { response?: { status?: number } }).response?.status
      if (status === 503) {
        setUnavailable(true)
        setError(null)
      } else {
        setError((e as Error).message)
      }
    } finally {
      loadedOnce.current = true
      setLoading(false)
    }
  }, [])

  useEffect(() => {
    void reload()
    const timer = setInterval(() => void reload(), POLL_MS)
    return () => clearInterval(timer)
  }, [reload])

  return {
    snapshot,
    /** 仅首次加载为 true，轮询期间保持 false。 */
    loading: loading && !loadedOnce.current,
    error,
    unavailable,
    reload,
  }
}
