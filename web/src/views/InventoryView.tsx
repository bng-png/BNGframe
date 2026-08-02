import { useEffect, useMemo, useState } from 'react'
import {
  api,
  MARKET_ORDERS_CHANGED,
  readCachedMyOrders,
  writeCachedMyOrders,
  type InventoryItem,
  type MasterySet,
} from '../api'
import { ItemCard } from '../components/ItemCard'
import { SetCard } from '../components/SetCard'
import { DucatAmount, PlatAmount } from '../components/Currency'
import { useWarmVisiblePrices } from '../hooks/useWarmVisiblePrices'
import {
  isArcaneItem,
  isInventoryListedItem,
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
  isMasterySetComplete,
  matchInventoryFilters,
  masterySelectionToItem,
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
  onSelectItem,
}: {
  items: InventoryItem[]
  t: (k: string, vars?: Record<string, string | number>) => string
  busy: boolean
  onSync: () => void
  onRefreshPrices: () => void
  onItemPriced: (urlName: string, platinum: number) => void
  onWts: (item: InventoryItem) => void
  onWtb: (item: InventoryItem) => void
  onSelectItem?: (item: InventoryItem) => void
}) {
  const [q, setQ] = useState('')
  const [cat, setCat] = useState('all')
  const [sort, setSort] = useState<SortMode>('ducanator')
  const [filters, setFilters] = useState<InventoryFilters>(EMPTY_FILTERS)
  const [orderedUrls, setOrderedUrls] = useState<Set<string>>(() => {
    const cached = readCachedMyOrders()
    return cached ? orderUrlSet(cached) : new Set()
  })
  const [masterySets, setMasterySets] = useState<MasterySet[]>([])

  const setComplete = useMemo(() => buildSetCompleteMap(masterySets), [masterySets])

  useEffect(() => {
    let cancelled = false
    const loadOrders = (force = false) =>
      api
        .marketOrders({ refresh: force })
        .then((orders) => {
          if (cancelled) return
          const list = Array.isArray(orders) ? orders : []
          writeCachedMyOrders(list)
          setOrderedUrls(orderUrlSet(list))
        })
        .catch(() => {
          if (!cancelled) setOrderedUrls(new Set())
        })

    loadOrders(false)
    const onChanged = () => loadOrders(true)
    window.addEventListener(MARKET_ORDERS_CHANGED, onChanged)

    api
      .masterySets()
      .then((r) => {
        if (!cancelled) setMasterySets(r.sets || [])
      })
      .catch(() => {
        if (!cancelled) setMasterySets([])
      })
    return () => {
      cancelled = true
      window.removeEventListener(MARKET_ORDERS_CHANGED, onChanged)
    }
  }, [])

  const invCount = items.length
  useEffect(() => {
    if (!invCount) return
    let cancelled = false
    api
      .masterySets()
      .then((r) => {
        if (!cancelled) setMasterySets(r.sets || [])
      })
      .catch(() => {})
    return () => {
      cancelled = true
    }
  }, [invCount])

  /** «Сет полный = да» → mastery-style set cards instead of loose parts. */
  const showCompleteSets = filters.setComplete === true

  const completeSetCards = useMemo(() => {
    if (!showCompleteSets) return [] as MasterySet[]
    let list = masterySets.filter(isMasterySetComplete)
    if (q.trim()) {
      const qq = q.toLowerCase()
      list = list.filter(
        (s) =>
          s.name.toLowerCase().includes(qq) ||
          (s.name_ru || '').toLowerCase().includes(qq),
      )
    }
    if (filters.vaulted != null) {
      list = list.filter((s) => !!s.vaulted === filters.vaulted)
    }
    if (filters.mastered != null) {
      list = list.filter((s) => !!s.mastered === filters.mastered)
    }
    if (filters.partKind === 'prime') {
      list = list.filter((s) => /prime|прайм/i.test(`${s.name} ${s.name_ru || ''} ${s.set_key}`))
    }
    if (filters.partKind === 'normal') {
      list = list.filter((s) => !/prime|прайм/i.test(`${s.name} ${s.name_ru || ''} ${s.set_key}`))
    }
    if (filters.minPlat != null) {
      list = list.filter((s) => (s.platinum ?? 0) >= filters.minPlat!)
    }
    if (filters.orderPlaced != null) {
      list = list.filter((s) => {
        const urls = [s.url_name, ...(s.parts || []).map((p) => p.url_name)].filter(Boolean) as string[]
        const has = urls.some((u) => orderedUrls.has(u))
        return has === filters.orderPlaced
      })
    }
    list = [...list]
    list.sort((a, b) => {
      if (sort === 'name') return (a.name_ru || a.name).localeCompare(b.name_ru || b.name)
      if (sort === 'plat') return (b.platinum || 0) - (a.platinum || 0)
      if (sort === 'qty') {
        const qa = (a.parts || []).reduce((n, p) => n + (p.count || 0), 0)
        const qb = (b.parts || []).reduce((n, p) => n + (p.count || 0), 0)
        return qb - qa
      }
      const score = (s: MasterySet) => {
        const d = s.ducats || 0
        const p = Math.max(s.platinum || 0.5, 0.5)
        return d / p
      }
      return score(b) - score(a)
    })
    return list
  }, [showCompleteSets, masterySets, q, filters, orderedUrls, sort])

  const filtered = useMemo(() => {
    if (showCompleteSets) return [] as InventoryItem[]
    const catFn = CATS.find((c) => c.id === cat)?.match || (() => true)
    let list = items
      .filter((i) => isInventoryListedItem(i))
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
  }, [items, q, cat, sort, filters, orderedUrls, setComplete, showCompleteSets])

  const visible = useMemo(() => filtered.slice(0, VISIBLE_LIMIT), [filtered])
  const visibleSetCards = useMemo(
    () => completeSetCards.slice(0, VISIBLE_LIMIT),
    [completeSetCards],
  )
  const visibleUrls = useMemo(() => {
    if (showCompleteSets) {
      return visibleSetCards.map((s) => s.url_name).filter((u): u is string => !!u)
    }
    return visible.map((i) => i.url_name).filter((u): u is string => !!u)
  }, [showCompleteSets, visible, visibleSetCards])

  const pricing = useWarmVisiblePrices(
    visibleUrls,
    showCompleteSets
      ? (url) => masterySets.some((s) => s.url_name === url && (s.platinum ?? 0) > 0)
      : () => false,
    (url, platinum) => {
      onItemPriced(url, platinum)
      if (showCompleteSets) {
        setMasterySets((prev) =>
          prev.map((s) => (s.url_name === url ? { ...s, platinum } : s)),
        )
      }
    },
  )

  const totals = useMemo(() => {
    if (showCompleteSets) {
      let plat = 0
      let ducats = 0
      for (const s of completeSetCards) {
        if (s.platinum) plat += s.platinum
        if (s.ducats) ducats += s.ducats
      }
      return { plat, ducats }
    }
    let plat = 0
    let ducats = 0
    for (const i of filtered) {
      if (i.platinum) plat += i.platinum * i.count
      if (i.ducats) ducats += i.ducats * i.count
    }
    return { plat, ducats }
  }, [filtered, showCompleteSets, completeSetCards])

  const shownCount = showCompleteSets ? completeSetCards.length : filtered.length

  return (
    <div className="inv-layout">
      <InventoryFilterPanel filters={filters} onChange={setFilters} t={t} />
      <div className="inv-main">
        <div className="inv-toolbar">
          <div className="toolbar">
            <div className="chips">
              {CATS.map((c) => (
                <button
                  key={c.id}
                  type="button"
                  className={`chip${cat === c.id ? ' active' : ''}`}
                  onClick={() => setCat(c.id)}
                  disabled={showCompleteSets && c.id !== 'all' && c.id !== 'set'}
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
        </div>
        <div className="inv-body">
          {showCompleteSets ? (
            <div className="set-grid">
              {visibleSetCards.map((s) => (
                <SetCard
                  key={s.set_key}
                  set={s}
                  onSelectSet={
                    onSelectItem && s.url_name
                      ? () =>
                          onSelectItem(
                            masterySelectionToItem({
                              urlName: s.url_name!,
                              name: (s.name_ru || s.name).replace(/<[^>]+>\s*/g, '').trim(),
                              platinum: s.platinum,
                              thumb: s.thumb,
                            }),
                          )
                      : undefined
                  }
                  onSelectPart={
                    onSelectItem
                      ? (p) => {
                          if (!p.url_name) return
                          onSelectItem(
                            masterySelectionToItem({
                              urlName: p.url_name,
                              name: p.name || p.role,
                              thumb: p.thumb,
                              count: 1,
                            }),
                          )
                        }
                      : undefined
                  }
                  labels={{
                    owned: t('owned'),
                    vaulted: t('vaulted'),
                    unvaulted: t('unvaulted'),
                    mastered: t('mastered'),
                    absorbed: t('absorbed'),
                  }}
                />
              ))}
            </div>
          ) : (
            <div className="item-list">
              {visible.map((item) => (
                <ItemCard
                  key={item.unique_name}
                  item={item}
                  onWts={() => onWts(item)}
                  onWtb={() => onWtb(item)}
                  onSelect={onSelectItem ? () => onSelectItem(item) : undefined}
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
          )}
          <div className="muted" style={{ marginTop: 10, marginBottom: 8 }}>
            {t('showing', {
              shown: Math.min(shownCount, VISIBLE_LIMIT),
              total: shownCount,
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
