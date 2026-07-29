import { WikiImg } from './WikiImg'
import { wikiFileUrls } from '../wikiImages'

const PLAT_URLS = wikiFileUrls('Platinum.png')
const DUCAT_URLS = [...wikiFileUrls('DucatsEmoji.png'), ...wikiFileUrls('Ducats_vector(xBlack).png')]

export function PlatIcon({ className }: { className?: string }) {
  return <WikiImg urls={PLAT_URLS} className={className || 'currency-ico'} alt="p" />
}

export function DucatIcon({ className }: { className?: string }) {
  return <WikiImg urls={DUCAT_URLS} className={className || 'currency-ico'} alt="d" />
}

export function PlatAmount({ value }: { value: number | null | undefined }) {
  if (value == null) return <span className="muted">—</span>
  return (
    <span className="currency">
      <PlatIcon />
      <span>{Math.round(value)}</span>
    </span>
  )
}

export function DucatAmount({ value }: { value: number | null | undefined }) {
  if (value == null) return null
  return (
    <span className="currency">
      <DucatIcon />
      <span>{value}</span>
    </span>
  )
}
