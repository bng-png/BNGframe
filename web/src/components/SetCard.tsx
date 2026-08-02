import type { MasterySet } from '../api'
import { wfmThumbUrl } from '../api'
import { WikiImg } from './WikiImg'
import { PartThumb, thumbFromSet } from './PartThumb'
import { PlatAmount, DucatAmount } from './Currency'
import { AbsorbedIcon, MasteredIcon, UnvaultedIcon, VaultedIcon } from './StatusIcons'
import { rolePartFile, toWikiCamel, wikiFileUrls, wikiUrlForName } from '../wikiImages'

const FRAME_ROLES = ['blueprint', 'neuroptics', 'chassis', 'systems'] as const
const WEAPON_ROLES = [
  'blueprint',
  'barrel',
  'receiver',
  'stock',
  'link',
  'blade',
  'handle',
  'hilt',
  'gauntlet',
  'grip',
  'string',
  'chain',
  'ornament',
  'guard',
  'carapace',
  'cerebrum',
  'systems',
] as const

function rolesForSet(set: MasterySet): string[] {
  const hasMeta = (role: string) => {
    const p = set.parts.find((x) => x.role === role)
    return !!(p && (p.url_name || p.name))
  }

  const unitKey = (role: string) => role.replace(/_\d+$/, '')
  const unitNum = (role: string) => {
    const m = role.match(/_(\d+)$/)
    return m ? parseInt(m[1], 10) : 1
  }

  // Group barrel / barrel_2 / … so qty slots stay adjacent (not dumped at the end).
  const byBase = new Map<string, string[]>()
  for (const p of set.parts) {
    if (p.role === 'other' || !hasMeta(p.role)) continue
    const base = unitKey(p.role)
    const list = byBase.get(base) || []
    list.push(p.role)
    byBase.set(base, list)
  }
  for (const list of byBase.values()) {
    list.sort((a, b) => unitNum(a) - unitNum(b))
  }

  const take = (base: string): string[] => {
    const list = byBase.get(base)
    if (!list) return []
    byBase.delete(base)
    return list
  }

  if (set.category === 'warframe') {
    return FRAME_ROLES.flatMap((r) => take(r))
  }
  if (set.category === 'necramech') {
    return (
      ['blueprint', 'casing', 'systems', 'engine', 'weapon_pod', 'neuroptics', 'chassis'] as const
    ).flatMap((r) => take(r))
  }
  if (set.category === 'vehicle') {
    return (['blueprint', 'engines', 'fuselage', 'avionics'] as const).flatMap((r) => take(r))
  }
  if (set.category === 'kdrive' || set.category === 'plexus') {
    return [...byBase.keys()].sort().flatMap((k) => take(k))
  }
  if (set.category === 'intrinsic') {
    return Array.from({ length: 10 }, (_, i) => `rank_${i + 1}`).flatMap((r) => take(r))
  }
  if (set.category === 'companion') {
    return (['blueprint', 'carapace', 'cerebrum', 'systems'] as const).flatMap((r) => take(r))
  }
  if (
    set.category === 'archwing' &&
    !set.parts.some((p) =>
      ['barrel', 'receiver', 'stock', 'blade', 'handle', 'head', 'link'].includes(unitKey(p.role)),
    )
  ) {
    return (['blueprint', 'harness', 'wings', 'systems'] as const).flatMap((r) => take(r))
  }

  const isWeapon =
    set.category === 'weapon_prime' ||
    set.category === 'weapon' ||
    set.category === 'primary' ||
    set.category === 'secondary' ||
    set.category === 'melee' ||
    set.category === 'archwing' ||
    set.category === 'modular' ||
    set.parts.some((p) =>
      [
        'barrel',
        'receiver',
        'stock',
        'link',
        'blade',
        'handle',
        'gauntlet',
        'carapace',
        'cerebrum',
        'harness',
        'wings',
      ].includes(unitKey(p.role)),
    ) ||
    set.parts.some((p) => p.role.startsWith('component_'))

  const prefer = isWeapon ? WEAPON_ROLES : FRAME_ROLES
  const ordered: string[] = []
  for (const r of prefer) {
    ordered.push(...take(r))
  }
  // Remaining component_* / odd roles
  const rest = [...byBase.keys()].sort()
  for (const k of rest) {
    if (k.startsWith('component_')) ordered.push(...take(k))
  }
  for (const k of [...byBase.keys()].sort()) {
    ordered.push(...take(k))
  }
  return ordered
}

function roleWikiSuffix(role: string): string | null {
  switch (role) {
    case 'blueprint':
      return 'Blueprint'
    case 'neuroptics':
      return 'Neuroptics'
    case 'chassis':
      return 'Chassis'
    case 'systems':
      return 'Systems'
    case 'harness':
      return 'Harness'
    case 'wings':
      return 'Wings'
    case 'engines':
      return 'Engines'
    case 'fuselage':
      return 'Fuselage'
    case 'avionics':
      return 'Avionics'
    case 'casing':
      return 'Casing'
    case 'capsule':
      return 'Capsule'
    case 'weapon_pod':
      return 'Weapon Pod'
    case 'engine':
      return 'Engine'
    case 'barrel':
      return 'Barrel'
    case 'receiver':
      return 'Receiver'
    case 'stock':
      return 'Stock'
    case 'blade':
      return 'Blade'
    case 'handle':
      return 'Handle'
    case 'link':
      return 'Link'
    case 'carapace':
      return 'Carapace'
    case 'cerebrum':
      return 'Cerebrum'
    case 'gauntlet':
      return 'Gauntlet'
    case 'grip':
      return 'Grip'
    case 'string':
      return 'String'
    case 'chain':
      return 'Chain'
    case 'ornament':
      return 'Ornament'
    case 'hilt':
      return 'Hilt'
    case 'guard':
      return 'Guard'
    case 'head':
      return 'Head'
    default:
      return null
  }
}

/** WFM content-addressed thumb hash (`…/name.<hash>.128x128.png`). */
function thumbContentHash(thumb?: string | null): string | null {
  if (!thumb) return null
  const m = thumb.match(/\.([a-f0-9]{32})\./i)
  return m ? m[1].toLowerCase() : null
}

/** Prefer real part art (wiki role icons); skip WFM thumbs that are just the set icon. */
function partImageUrls(
  role: string,
  opts: {
    name?: string | null
    urlName?: string | null
    thumbUrl?: string | null
    thumbPath?: string | null
    setKey?: string
    setThumbPath?: string | null
  },
): string[] {
  const urls: string[] = []

  // blade_2 / component_bronco_2 → base role for wiki stubs
  const baseRole = role.replace(/_\d+$/, '')

  if (baseRole.startsWith('component_')) {
    const ingKey = baseRole.slice('component_'.length)
    // Unique WFM thumb (not a copy of the parent set icon)
    const partHash = thumbContentHash(opts.thumbPath)
    const setHash = thumbContentHash(opts.setThumbPath)
    if (opts.thumbUrl && partHash && partHash !== setHash) {
      urls.push(opts.thumbUrl)
    }
    if (opts.urlName) {
      const slug = opts.urlName.replace(/_set$/, '')
      urls.push(...wikiFileUrls(`${toWikiCamel(slug)}.png`))
      urls.push(...wikiUrlForName(opts.urlName.replace(/_set$/, '').replace(/_/g, ' ')))
    }
    if (ingKey) {
      urls.push(...wikiFileUrls(`${toWikiCamel(ingKey)}.png`))
    }
    if (opts.name) {
      const cleaned = opts.name
        .replace(/<[^>]+>\s*/g, '')
        .replace(/\([^)]*\)/g, ' ')
        .replace(/:/g, ' ')
        .trim()
      if (cleaned) urls.push(...wikiUrlForName(cleaned))
    }
    return [...new Set(urls.filter(Boolean))]
  }

  // Prefer reliable generic wiki role icons first (Barrel.png / Receiver.png).
  // Speculative set+role filenames often 404 and previously left qty slots on Blueprint.png.
  urls.push(...wikiFileUrls(rolePartFile(baseRole)))

  // Specific wiki file from WFM slug (AkariusPrimeBarrel.png) — try after role icon
  if (opts.urlName) {
    const slug = opts.urlName.replace(/_set$/, '')
    urls.push(...wikiFileUrls(`${toWikiCamel(slug)}.png`))
  }

  // set + role
  if (opts.setKey) {
    const base = toWikiCamel(opts.setKey)
    const suf = roleWikiSuffix(baseRole)
    if (base && suf) {
      urls.push(...wikiFileUrls(`${base}${suf}.png`))
    }
  }

  // WFM thumb only when it is not the duplicated set artwork
  const partHash = thumbContentHash(opts.thumbPath)
  const setHash = thumbContentHash(opts.setThumbPath)
  if (opts.thumbUrl && partHash && setHash && partHash !== setHash) {
    urls.push(opts.thumbUrl)
  } else if (opts.thumbUrl && partHash && !setHash) {
    urls.push(opts.thumbUrl)
  }

  return [...new Set(urls.filter(Boolean))]
}

export function SetCard({
  set,
  labels,
  onSelectSet,
  onSelectPart,
}: {
  set: MasterySet
  labels: { owned: string; vaulted: string; unvaulted: string; mastered: string; absorbed: string }
  /** Open market orders for the full set listing. */
  onSelectSet?: () => void
  /** Open market orders for a specific part. */
  onSelectPart?: (part: {
    role: string
    url_name?: string | null
    name?: string | null
    thumb?: string | null
  }) => void
}) {
  const title = (set.name_ru || set.name).replace(/<[^>]+>\s*/g, '').trim()
  const mainUrls = thumbFromSet({
    setKey: set.set_key,
    urlName: set.url_name,
    name: title,
    wfmThumbUrl: set.thumb_url || wfmThumbUrl(set.thumb),
  })
  const isPrime = /prime|прайм/i.test(`${set.set_key} ${set.name} ${set.name_ru || ''}`)
  const tone = isPrime ? 'prime' : 'normal'

  const roles = rolesForSet(set)
  const parts = roles.map((role) => {
    const found = set.parts.find((p) => p.role === role)
    return found || { role, count: 0, required: 1, url_name: null, name: null }
  })

  const hasPrice = set.platinum != null || set.ducats != null
  const showVault = isPrime || set.vaulted
  const showFlags = set.absorbed || showVault
  const setClickable = !!(onSelectSet && set.url_name)

  return (
    <article
      className={`set-card${set.owned ? ' owned' : ''}${set.mastered ? ' is-mastered' : ''}${setClickable ? ' clickable' : ''}`}
      onClick={() => {
        if (setClickable) onSelectSet?.()
      }}
      role={setClickable ? 'button' : undefined}
      tabIndex={setClickable ? 0 : undefined}
      onKeyDown={(e) => {
        if (!setClickable) return
        if (e.key === 'Enter' || e.key === ' ') {
          e.preventDefault()
          onSelectSet?.()
        }
      }}
    >
      <PartThumb urls={mainUrls} size={112} tone={tone} />
      <div className="set-card-body">
        {set.mastered && (
          <div className="set-card-mastered" title={labels.mastered} aria-label={labels.mastered}>
            <MasteredIcon title={labels.mastered} />
          </div>
        )}
        <div className="item-title">{title}</div>
        {hasPrice && (
          <div className="item-meta item-meta-prices">
            <PlatAmount value={set.platinum} />
            <DucatAmount value={set.ducats} />
          </div>
        )}
        {parts.length > 0 && (
          <div
            className="set-parts"
            style={{ gridTemplateColumns: `repeat(${parts.length}, 1fr)` }}
            onClick={(e) => e.stopPropagation()}
          >
            {parts.map((p) => {
              const need = p.required ?? 1
              const has = p.count >= need
              const partUrls = partImageUrls(p.role, {
                name: p.name,
                urlName: p.url_name,
                thumbUrl: wfmThumbUrl(p.thumb),
                thumbPath: p.thumb,
                setKey: set.set_key,
                setThumbPath: set.thumb,
              })
              const label = p.name || p.role
              const partClickable = !!(onSelectPart && p.url_name)
              const circleClass = `part-circle${has ? ' has' : ''} tone-${tone}${partClickable ? ' clickable' : ''}`
              if (partClickable) {
                return (
                  <button
                    key={p.role}
                    type="button"
                    className={circleClass}
                    title={`${label}: ×${p.count}/${need}`}
                    onClick={(e) => {
                      e.stopPropagation()
                      onSelectPart?.(p)
                    }}
                  >
                    <WikiImg urls={partUrls} className="part-circle-img" />
                    {need > 1 && <span className="part-circle-qty">×{need}</span>}
                  </button>
                )
              }
              return (
                <div
                  key={p.role}
                  className={circleClass}
                  title={`${label}: ×${p.count}/${need}`}
                >
                  <WikiImg urls={partUrls} className="part-circle-img" />
                  {need > 1 && <span className="part-circle-qty">×{need}</span>}
                </div>
              )
            })}
          </div>
        )}
      </div>
      {showFlags && (
        <div className="set-card-flags" onClick={(e) => e.stopPropagation()}>
          {set.absorbed && <AbsorbedIcon title={labels.absorbed} />}
          {showVault &&
            (set.vaulted ? (
              <VaultedIcon title={labels.vaulted} />
            ) : (
              <UnvaultedIcon title={labels.unvaulted} />
            ))}
        </div>
      )}
    </article>
  )
}
