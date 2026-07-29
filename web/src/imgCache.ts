/** Proxy remote images through the daemon disk cache (`~/.cache/bngframe/img`). */
export function cachedImgUrl(url?: string | null): string | null {
  if (!url) return null
  const u = url.trim()
  if (!u) return null
  if (u.startsWith('/api/img')) return u
  if (u.startsWith('data:')) return u
  if (!(u.startsWith('http://') || u.startsWith('https://'))) return u
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
