import { useEffect, useMemo, useState } from 'react'
import { api, type InventoryItem } from '../api'
import { ItemCard } from '../components/ItemCard'
import { DucatAmount, PlatAmount } from '../components/Currency'
import { useWarmVisiblePrices } from '../hooks/useWarmVisiblePrices'
import {
  isArcaneItem,
  isMiscCategoryItem,
  isModItem,
  isRelicItem,
  isSetPartItem,
} from '../itemClass'
import { InventoryFilterPanel } from './InventoryFilterPanel'
import {
  buildSetCompleteMap,
  EMPTY_FILTERS,
  filtersActive,
  matchInventoryFilters,
  orderUrlSet,
  type InventoryFilters,
} from './inventoryFilters'

const VISIBLE_LIMIT = 250

const CATS: { id: string; labelKey: string; match: (i: InventoryItem) => boolean }[] = [
  { id: 'all', labelKey: 'cat_all', match: () => true },
  { id: 'relic', labelKey: 'cat_relic', match: (i) => isRelicItem(i) },
  { id: 'mod', labelKey: 'cat_mod', match: (i) => isModItem(i) },
  { id: 'arcane', labelKey: 'cat_arcane', match: (i) => isArcaneItem(i) },
  { id: 'set', labelKey: 'cat_set', match: (i) => isSetPartItem(i) },
  { id: 'misc', labelKey: 'cat_misc', match: (i) => isMiscCategoryItem(i) },
]

type SortMode = 'ducanator' | 'plat' | 'name' | 'qty'

export function InventoryView({
  items,
  t,
  busy,
  onSync,
  onRefreshPrices,
  onItemPriced,
  onWts,
  onWtb,
}: {
  items: InventoryItem[]
  t: (k: string, vars?: Record<string, string | number>) => string
  busy: boolean
  onSync: () => void
  onRefreshPrices: () => void
  onItemPriced: (urlName: string, platinum: number) => void
  onWts: (item: InventoryItem) => void
  onWtb: (item: InventoryItem) => void
}) {
  const [q, setQ] = useState('')
  const [cat, setCat] = useState('all')
  const [sort, setSort] = useState<SortMode>('ducanator')
  const [filters, setFilters] = useState<InventoryFilters>(EMPTY_FILTERS)
  const [orderedUrls, setOrderedUrls] = useState<Set<string>>(() => new Set())
  const [setComplete, setSetComplete] = useState<Map<string, boolean>>(() => new Map())

  useEffect(() => {
    let cancelled = false
    api
      .marketOrders()
      .then((orders) => {
        if (!cancelled) setOrderedUrls(orderUrlSet(orders || []))
      })
      .catch(() => {
        if (!cancelled) setOrderedUrls(new Set())
      })
    api
      .masterySets()
      .then((r) => {
        if (!cancelled) setSetComplete(buildSetCompleteMap(r.sets || []))
      })
      .catch(() => {
        if (!cancelled) setSetComplete(new Map())
      })
    return () => {
      cancelled = true
    }
  }, [])

  // Refresh set-complete after inventory size changes (sync), not on every price tick
  const invCount = items.length
  useEffect(() => {
    if (!invCount) return
    let cancelled = false
    api
      .masterySets()
      .then((r) => {
        if (!cancelled) setSetComplete(buildSetCompleteMap(r.sets || []))
      })
      .catch(() => {})
    return () => {
      cancelled = true
    }
  }, [invCount])

  const filtered = useMemo(() => {
    const catFn = CATS.find((c) => c.id === cat)?.match || (() => true)
    // Market listings + relics/arcanes (resolved lazily; show even before url_name)
    let list = items
      .filter((i) => !!i.url_name || isRelicItem(i) || isArcaneItem(i))
      .filter(catFn)
    if (q.trim()) {
      const qq = q.toLowerCase()
      list = list.filter((i) => i.name.toLowerCase().includes(qq))
    }
    list = list.filter((i) => matchInventoryFilters(i, filters, orderedUrls, setComplete))
    list = [...list]
    list.sort((a, b) => {
      if (sort === 'name') return a.name.localeCompare(b.name)
      if (sort === 'qty') return b.count - a.count
      if (sort === 'plat') return (b.platinum || 0) - (a.platinum || 0)
      const score = (i: InventoryItem) => {
        const d = i.ducats || 0
        const p = Math.max(i.platinum || 0.5, 0.5)
        return (d * i.count) / p
      }
      return score(b) - score(a)
    })
    return list
  }, [items, q, cat, sort, filters, orderedUrls, setComplete])

  const visible = useMemo(() => filtered.slice(0, VISIBLE_LIMIT), [filtered])
  const visibleUrls = useMemo(
    () => visible.map((i) => i.url_name).filter((u): u is string => !!u),
    [visible],
  )
  const pricing = useWarmVisiblePrices(
    visibleUrls,
    () => false,
    onItemPriced,
  )

  const totals = useMemo(() => {
    let plat = 0
    let ducats = 0
    for (const i of filtered) {
      if (i.platinum) plat += i.platinum * i.count
      if (i.ducats) ducats += i.ducats * i.count
    }
    return { plat, ducats }
  }, [filtered])

  return (
    <div className="inv-layout">
      <InventoryFilterPanel filters={filters} onChange={setFilters} t={t} />
      <div className="inv-main">
        <div className="inv-body">
          <div className="toolbar">
            <div className="chips">
              {CATS.map((c) => (
                <button
                  key={c.id}
                  type="button"
                  className={`chip${cat === c.id ? ' active' : ''}`}
                  onClick={() => setCat(c.id)}
                >
                  {t(c.labelKey)}
                </button>
              ))}
            </div>
            {filtersActive(filters) && (
              <button
                type="button"
                className="btn ghost sm"
                onClick={() => setFilters(EMPTY_FILTERS)}
              >
                {t('filt_clear')}
              </button>
            )}
          </div>
          <div className="toolbar">
            <input
              className="search"
              value={q}
              onChange={(e) => setQ(e.target.value)}
              placeholder={t('filter_placeholder')}
            />
            <select
              className="select"
              value={sort}
              onChange={(e) => setSort(e.target.value as SortMode)}
            >
              <option value="ducanator">{t('sort_ducanator')}</option>
              <option value="plat">{t('sort_plat')}</option>
              <option value="name">{t('sort_name')}</option>
              <option value="qty">{t('sort_qty')}</option>
            </select>
            <button className="btn ghost sm" type="button" disabled={busy} onClick={onSync}>
              {t('sync_memory')}
            </button>
            <button className="btn ghost sm" type="button" disabled={busy} onClick={onRefreshPrices}>
              {t('refresh_prices')}
            </button>
            {pricing && (
              <span className="muted" style={{ fontSize: '0.9rem' }}>
                {t('loading_prices')} {pricing.done}/{pricing.total}
              </span>
            )}
          </div>

          <div className="item-list">
            {visible.map((item) => (
              <ItemCard
                key={item.unique_name}
                item={item}
                onWts={() => onWts(item)}
                onWtb={() => onWtb(item)}
                labels={{
                  owned: t('owned'),
                  mastered: t('mastered'),
                  vaulted: t('vaulted'),
                  unvaulted: t('unvaulted'),
                  wts: t('wts'),
                  wtb: t('wtb'),
                  qty: t('col_qty'),
                  rank: t('order_rank'),
                  typeMod: t('type_mod'),
                  typeArcane: t('type_arcane'),
                  typeRelic: t('type_relic'),
                  typeMisc: t('type_misc'),
                  typePart: t('type_part'),
                  typePrimary: t('type_primary'),
                  typeSecondary: t('type_secondary'),
                  typeMelee: t('type_melee'),
                  typeWeapon: t('type_weapon'),
                  typeWarframe: t('type_warframe'),
                  typeBlueprint: t('type_blueprint'),
                }}
              />
            ))}
          </div>
          <div className="muted" style={{ marginTop: 10, marginBottom: 8 }}>
            {t('showing', {
              shown: Math.min(filtered.length, VISIBLE_LIMIT),
              total: filtered.length,
            })}
          </div>
        </div>
        <div className="footer-bar">
          <span className="currency">
            <PlatAmount value={totals.plat} />
          </span>
          <span className="currency">
            <DucatAmount value={totals.ducats} />
          </span>
        </div>
      </div>
    </div>
  )
}
