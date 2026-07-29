import { useEffect, useState } from 'react'
import { cachedImgUrls } from '../imgCache'

/** Tries URLs in order (fandom → wiki fallback → optional extras). */
export function WikiImg({
  urls,
  alt = '',
  className,
  referrerPolicy = 'no-referrer',
}: {
  urls: (string | null | undefined)[]
  alt?: string
  className?: string
  referrerPolicy?: 'no-referrer' | 'origin' | 'strict-origin-when-cross-origin'
}) {
  const list = cachedImgUrls(urls)
  const [idx, setIdx] = useState(0)
  useEffect(() => {
    setIdx(0)
  }, [list.join('|')])
  if (!list.length || idx >= list.length) {
    return <div className={className ? `${className} placeholder` : 'placeholder'}>?</div>
  }
  return (
    <img
      className={className}
      src={list[idx]}
      alt={alt}
      loading="lazy"
      referrerPolicy={referrerPolicy}
      onError={() => setIdx((i) => i + 1)}
    />
  )
}
