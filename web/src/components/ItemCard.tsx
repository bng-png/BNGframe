import type { InventoryItem } from '../api'
import { wfmThumbUrl } from '../api'
import { PartThumb, thumbFromItem } from './PartThumb'
import { PlatAmount, DucatAmount, PlatIcon } from './Currency'
import { UnvaultedIcon, VaultedIcon } from './StatusIcons'
import { roleFromDisplayName } from '../wikiImages'
import { isArcaneItem, isModItem, isRelicItem, isSetPartItem } from '../itemClass'
import type { ReactNode } from 'react'

const ROLE_LABEL: Record<string, string> = {
  blueprint: 'BP',
  neuroptics: 'N',
  chassis: 'C',
  systems: 'S',
  barrel: 'ствол',
  receiver: 'приёмник',
  stock: 'приклад',
  link: 'связь',
  blade: 'клинок',
  handle: 'рукоять',
  carapace: 'панцирь',
  cerebrum: 'мозг',
}

export function ItemCard({
  item,
  onWts,
  onWtb,
  onSelect,
  labels,
  actions,
  rankEdit,
}: {
  item: InventoryItem
  onWts?: () => void
  onWtb?: () => void
  /** Open market order book (right rail). */
  onSelect?: () => void
  labels: {
    owned: string
    mastered?: string
    vaulted: string
    unvaulted?: string
    wts: string
    wtb: string
    qty: string
    rank: string
    typeMod: string
    typeArcane: string
    typeRelic: string
    typeMisc: string
    typePart: string
    typePrimary: string
    typeSecondary: string
    typeMelee: string
    typeWeapon: string
    typeWarframe: string
    typeBlueprint: string
  }
  /** Replace default WTS/WTB buttons (e.g. market Sold/Remove / List). */
  actions?: ReactNode
  /** Editable rank for mods/arcanes when listing. */
  rankEdit?: {
    value: number
    max: number
    onChange: (n: number) => void
  }
}) {
  const isMod = isModItem(item)
  const isArcane = isArcaneItem(item)
  const isRelic = isRelicItem(item)
  const skipRole = isMod || isArcane || isRelic
  const wfm = wfmThumbUrl(item.thumb)
  const { visual, mainUrls, partUrls, tone } = thumbFromItem({
    urlName: item.url_name,
    name: item.name,
    wfmThumbUrl: wfm,
    skipPartRole: skipRole,
    isRelic,
  })
  const platLabel = item.platinum != null ? Math.round(item.platinum) : null
  const role = skipRole ? null : visual.role || roleFromDisplayName(item.name)
  const typeFallback =
    {
      mod: labels.typeMod,
      arcane: labels.typeArcane,
      relic: labels.typeRelic,
      misc: labels.typeMisc,
      part: labels.typePart,
      primary: labels.typePrimary,
      secondary: labels.typeSecondary,
      melee: labels.typeMelee,
      weapon: labels.typeWeapon,
      warframe: labels.typeWarframe,
      blueprint: labels.typeBlueprint,
    }[item.item_type] || item.item_type
  const typeTag = isArcane
    ? labels.typeArcane
    : isMod
      ? labels.typeMod
      : isRelic
        ? labels.typeRelic
        : tone === 'prime'
          ? role
            ? `prime · ${ROLE_LABEL[role] || role}`
            : 'prime'
          : role
            ? ROLE_LABEL[role] || role
            : isSetPartItem(item)
              ? labels.typePart
              : typeFallback

  return (
    <article
      className={`item-card${item.mastered || item.count > 0 ? ' owned' : ''}${tone === 'prime' ? ' prime' : ''}${onSelect && item.url_name ? ' clickable' : ''}`}
      onClick={() => {
        if (onSelect && item.url_name) onSelect()
      }}
      role={onSelect && item.url_name ? 'button' : undefined}
      tabIndex={onSelect && item.url_name ? 0 : undefined}
      onKeyDown={(e) => {
        if (!onSelect || !item.url_name) return
        if (e.key === 'Enter' || e.key === ' ') {
          e.preventDefault()
          onSelect()
        }
      }}
    >
      <PartThumb
        urls={mainUrls}
        partUrls={role ? partUrls : undefined}
        size={96}
        tone={skipRole ? null : tone}
      />
      <div className="item-body">
        <div className="item-title">{item.name}</div>
        {item.name_en ? <div className="item-title-alt">{item.name_en}</div> : null}
        <div className="item-meta">
          <span>
            {labels.qty} ×{item.count}
          </span>
          {(isMod || isArcane) && (
            <span className="tag rank" title={labels.rank}>
              R{rankEdit ? rankEdit.value : (item.rank ?? 0)}
            </span>
          )}
          <PlatAmount value={item.platinum} />
          <DucatAmount value={item.ducats} />
          {item.mastered && (
            <span className="tag owned">{labels.mastered || 'Mastered'}</span>
          )}
          {item.vaulted === true && <VaultedIcon title={labels.vaulted} />}
          {item.vaulted === false && tone === 'prime' && (
            <UnvaultedIcon title={labels.unvaulted || labels.vaulted} />
          )}
          <span className={`tag${!skipRole && tone === 'prime' ? ' prime' : ''}`}>{typeTag}</span>
        </div>
      </div>
      <div className="item-actions" onClick={(e) => e.stopPropagation()}>
        {rankEdit && (isMod || isArcane) && (
          <label className="rank-edit">
            {labels.rank}
            <input
              type="number"
              min={0}
              max={rankEdit.max}
              value={rankEdit.value}
              onChange={(e) => {
                const n = Number(e.target.value)
                const max = rankEdit.max
                rankEdit.onChange(
                  Number.isFinite(n) ? Math.max(0, Math.min(max, Math.round(n))) : 0,
                )
              }}
            />
          </label>
        )}
        {actions ?? (
          <>
            <button className="btn sell" type="button" disabled={!item.url_name} onClick={onWts}>
              {labels.wts} {platLabel != null ? platLabel : ''}
              {platLabel != null && <PlatIcon className="currency-ico" />}
            </button>
            <button className="btn buy" type="button" disabled={!item.url_name} onClick={onWtb}>
              {labels.wtb}
            </button>
          </>
        )}
      </div>
    </article>
  )
}
