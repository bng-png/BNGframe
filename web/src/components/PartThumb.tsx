import { WikiImg } from './WikiImg'
import {
  partVisualFromSlug,
  rolePartFile,
  wikiFileUrls,
  wikiUrlForName,
  type PartVisual,
} from '../wikiImages'

/** Main item art with optional part-role circle (bottom-right), AlecaFrame-style. */
export function PartThumb({
  urls,
  partUrls,
  className,
  size = 72,
  tone,
}: {
  urls: string[]
  partUrls?: string[]
  className?: string
  size?: number
  /** Blueprint rarity backdrop */
  tone?: 'prime' | 'normal' | null
}) {
  const showBadge = !!(partUrls && partUrls.length)
  const toneClass = tone === 'prime' ? ' tone-prime' : tone === 'normal' ? ' tone-normal' : ''
  return (
    <div
      className={`part-thumb${toneClass}${className ? ` ${className}` : ''}`}
      style={{ width: size, height: size }}
    >
      <WikiImg urls={urls} className="part-thumb-main" />
      {showBadge && (
        <div className="part-thumb-badge">
          <WikiImg urls={partUrls!} className="part-thumb-badge-img" />
        </div>
      )}
    </div>
  )
}

export function thumbFromItem(opts: {
  urlName?: string | null
  name?: string | null
  wfmThumbUrl?: string | null
  role?: string | null
  /** Mods (and similar) must not be treated as frame/weapon parts. */
  skipPartRole?: boolean
}): { visual: PartVisual; mainUrls: string[]; partUrls: string[]; tone: 'prime' | 'normal' | null } {
  const visual = opts.skipPartRole
    ? {
        isPart: false,
        role: null,
        mainFile: null,
        partFile: null,
        mainUrls: opts.name ? wikiUrlForName(opts.name) : [],
        partUrls: [] as string[],
      }
    : partVisualFromSlug(opts.urlName, opts.role, opts.name)
  const parentName = opts.name
    ? opts.name.replace(/:.+$/, '').replace(/\([^)]*\)/g, '').trim()
    : ''
  const nameUrls = parentName ? wikiUrlForName(parentName) : []
  const mainUrls = [...new Set([...visual.mainUrls, ...nameUrls, ...(opts.wfmThumbUrl ? [opts.wfmThumbUrl] : [])])]
  const blob = `${opts.urlName || ''} ${opts.name || ''}`.toLowerCase()
  const isPrime = /prime|прайм/.test(blob)
  const looksLikePart =
    !opts.skipPartRole &&
    (visual.isPart ||
      /чертеж|blueprint|приёмник|приемник|панцир|ствол|каркас|систем|мозг|связь|приклад|клинок|рукоят|cerebrum|carapace|receiver|barrel/.test(
        blob,
      ))

  let tone: 'prime' | 'normal' | null = null
  if (looksLikePart) tone = isPrime ? 'prime' : 'normal'
  else if (isPrime) tone = 'prime'

  const partUrls = opts.skipPartRole
    ? []
    : [
        ...new Set(
          visual.partUrls.length
            ? visual.partUrls
            : visual.role
              ? wikiFileUrls(rolePartFile(visual.role))
              : [],
        ),
      ]

  return {
    visual: { ...visual, isPart: visual.isPart || looksLikePart },
    mainUrls,
    partUrls,
    tone,
  }
}

export function thumbFromSet(opts: {
  setKey: string
  urlName?: string | null
  name?: string | null
  wfmThumbUrl?: string | null
}): string[] {
  const cleanRu = (opts.name || '').replace(/<[^>]+>\s*/g, '').trim()
  const fromName = cleanRu ? wikiUrlForName(cleanRu) : []
  // English slug from set_key is the reliable wiki filename (Aklato.png)
  const fromKey = wikiFileUrls(
    `${opts.setKey
      .split('_')
      .filter(Boolean)
      .map((w) => w.charAt(0).toUpperCase() + w.slice(1))
      .join('')}.png`,
  )
  const fromUrl = opts.urlName
    ? partVisualFromSlug(opts.urlName.replace(/_set$/, '')).mainUrls
    : []
  return [
    ...new Set([
      ...(opts.wfmThumbUrl ? [opts.wfmThumbUrl] : []),
      ...fromKey,
      ...fromUrl,
      ...fromName,
    ]),
  ]
}
