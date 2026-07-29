import type { MasterySet } from '../api'
import { wfmThumbUrl } from '../api'
import { WikiImg } from './WikiImg'
import { PartThumb, thumbFromSet } from './PartThumb'
import { PlatAmount, DucatAmount } from './Currency'
import { AbsorbedIcon, MasteredIcon, UnvaultedIcon, VaultedIcon } from './StatusIcons'
import { rolePartFile, wikiFileUrls, wikiUrlForName } from '../wikiImages'

const FRAME_ROLES = ['blueprint', 'neuroptics', 'chassis', 'systems'] as const
const WEAPON_ROLES = [
  'blueprint',
  'barrel',
  'receiver',
  'stock',
  'link',
  'blade',
  'handle',
  'gauntlet',
  'grip',
  'string',
  'hilt',
  'guard',
  'carapace',
  'cerebrum',
  'systems',
] as const

function rolesForSet(set: MasterySet): string[] {
  const isWeapon =
    set.category === 'weapon_prime' ||
    set.category === 'weapon' ||
    set.category === 'primary' ||
    set.category === 'secondary' ||
    set.category === 'melee' ||
    set.category === 'archwing' ||
    set.category === 'modular' ||
    set.category === 'companion' ||
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
      ].includes(p.role),
    ) ||
    set.parts.some((p) => p.role.startsWith('component_'))
  const prefer = isWeapon
    ? set.category === 'companion'
      ? (['blueprint', 'carapace', 'cerebrum', 'systems'] as const)
      : set.category === 'archwing' &&
          !set.parts.some((p) =>
            ['barrel', 'receiver', 'stock', 'blade', 'handle', 'head', 'link'].includes(p.role),
          )
        ? (['blueprint', 'harness', 'wings', 'systems'] as const)
        : WEAPON_ROLES
    : FRAME_ROLES
  const present = new Set(set.parts.map((p) => p.role))
  // Only show slots that exist on this set (from API / catalog) — never invent a blueprint.
  const ordered = prefer.filter((r) => present.has(r))
  for (const p of set.parts) {
    if (!ordered.includes(p.role) && p.role !== 'other') {
      ordered.push(p.role)
    }
  }
  // Warframes / archwing suits: always show the classic 4 craft slots
  if (!isWeapon) {
    return [...FRAME_ROLES]
  }
  if (
    set.category === 'archwing' &&
    !set.parts.some((p) =>
      ['barrel', 'receiver', 'stock', 'blade', 'handle', 'head', 'link'].includes(p.role),
    )
  ) {
    return ['blueprint', 'harness', 'wings', 'systems']
  }
  return ordered
}

function partImageUrls(role: string, name?: string | null, urlName?: string | null): string[] {
  if (role.startsWith('component_')) {
    const fromName = name ? wikiUrlForName(name) : []
    const fromSlug = urlName
      ? wikiUrlForName(urlName.replace(/_set$/, '').replace(/_/g, ' '))
      : []
    return [...new Set([...fromName, ...fromSlug])]
  }
  return wikiFileUrls(rolePartFile(role))
}

export function SetCard({
  set,
  labels,
}: {
  set: MasterySet
  labels: { owned: string; vaulted: string; unvaulted: string; mastered: string; absorbed: string }
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

  return (
    <article
      className={`set-card${set.owned ? ' owned' : ''}${set.mastered ? ' is-mastered' : ''}`}
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
          >
            {parts.map((p) => {
              const need = p.required ?? 1
              const has = p.count >= need
              const partUrls = partImageUrls(p.role, p.name, p.url_name)
              const label = p.name || p.role
              return (
                <div
                  key={p.role}
                  className={`part-circle${has ? ' has' : ''} tone-${tone}`}
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
        <div className="set-card-flags">
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
