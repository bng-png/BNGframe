import { useEffect, useMemo, useRef, useState } from 'react'
import { api } from '../api'

/**
 * Warm WFM prices for visible items one-by-one (mastery-style).
 * Items may already show SQLite-cached platinum; the daemon skips the network
 * when the quote is fresh (<60m) and only refetches stale rows.
 */
export function useWarmVisiblePrices(
  visibleUrlNames: string[],
  hasPrice: (urlName: string) => boolean,
  onPriced: (urlName: string, platinum: number) => void,
): { done: number; total: number } | null {
  const onPricedRef = useRef(onPriced)
  onPricedRef.current = onPriced
  const hasPriceRef = useRef(hasPrice)
  hasPriceRef.current = hasPrice
  const attempted = useRef(new Set<string>())
  const [progress, setProgress] = useState<{ done: number; total: number } | null>(null)

  const queueKey = useMemo(() => {
    const seen = new Set<string>()
    const urls: string[] = []
    for (const u of visibleUrlNames) {
      if (!u || seen.has(u) || attempted.current.has(u) || hasPriceRef.current(u)) continue
      seen.add(u)
      urls.push(u)
    }
    return urls.join('\0')
  }, [visibleUrlNames])

  useEffect(() => {
    const urls = queueKey ? queueKey.split('\0').filter(Boolean) : []
    if (!urls.length) {
      setProgress(null)
      return
    }

    let cancelled = false
    setProgress({ done: 0, total: urls.length })

    ;(async () => {
      for (let i = 0; i < urls.length; i++) {
        if (cancelled) return
        const url = urls[i]
        attempted.current.add(url)
        try {
          const p = await api.priceItem(url)
          if (cancelled) return
          if (typeof p.platinum === 'number' && p.platinum > 0) {
            onPricedRef.current(url, p.platinum)
          }
        } catch {
          /* ignore single failures — will retry next session */
        }
        if (!cancelled) setProgress({ done: i + 1, total: urls.length })
      }
      if (!cancelled) setProgress(null)
    })()

    return () => {
      cancelled = true
    }
  }, [queueKey])

  return progress
}
