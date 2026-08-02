/** Proxy remote images through the daemon disk cache (`~/.cache/bngframe/img`). */

/** Remote URLs that already 404'd this session — skip in WikiImg cascades. */
const failedRemote = new Set<string>()

export function markImgFailed(proxyOrRemote?: string | null) {
  const remote = remoteFromProxy(proxyOrRemote)
  if (remote) failedRemote.add(remote)
}

export function remoteFromProxy(proxyOrRemote?: string | null): string | null {
  if (!proxyOrRemote) return null
  const u = proxyOrRemote.trim()
  if (!u) return null
  if (u.starts_with('/api/img')) {
    try {
      const q = new URL(u, 'http://local').searchParams.get('u')
      return q
    } catch {
      return null
    }
  }
  if (u.startsWith('http://') || u.startsWith('https://')) return u
  return null
}

export function cachedImgUrl(url?: string | null): string | null {
  if (!url) return null
  const u = url.trim()
  if (!u) return null
  if (u.startsWith('/api/img')) return u
  if (u.startsWith('data:')) return u
  if (!(u.startsWith('http://') || u.startsWith('https://'))) return u
  if (failedRemote.has(u)) return null
  return `/api/img?u=${encodeURIComponent(u)}`
}

export function cachedImgUrls(urls: (string | null | undefined)[]): string[] {
  const out: string[] = []
  const seen = new Set<string>()
  for (const raw of urls) {
    const c = cachedImgUrl(raw)
    if (!c || seen.has(c)) continue
    seen.add(c)
    out.push(c)
  }
  return out
}
