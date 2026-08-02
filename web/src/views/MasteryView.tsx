import { useEffect, useMemo, useState } from 'react'
import { api, type InventoryItem, type MasterySet } from '../api'
import { SetCard } from '../components/SetCard'
import { useWarmVisiblePrices } from '../hooks/useWarmVisiblePrices'
import { masterySelectionToItem } from './inventoryFilters'

const MASTERY_TYPES = [
  'warframe',
  'primary',
  'secondary',
  'melee',
  'companion',
  'archwing',
  'necramech',
  'modular',
  'kdrive',
  'plexus',
  'intrinsic',
] as const

type MasteryType = (typeof MASTERY_TYPES)[number]

const TYPE_I18N: Record<MasteryType, string> = {
  warframe: 'cat_warframe',
  primary: 'cat_primary',
  secondary: 'cat_secondary',
  melee: 'cat_melee',
  companion: 'cat_companion',
  archwing: 'cat_archwing',
  necramech: 'cat_necramech',
  modular: 'cat_modular',
  kdrive: 'cat_kdrive',
  plexus: 'cat_plexus',
  intrinsic: 'cat_intrinsic',
}

function normalizeCategory(cat: string): MasteryType | null {
  if ((MASTERY_TYPES as readonly string[]).includes(cat)) return cat as MasteryType
  if (cat === 'prime' || cat === 'weapon_prime' || cat === 'weapon') return 'primary'
  return null
}

export function MasteryView({
  t,
  onSelectItem,
}: {
  t: (k: string) => string
  onSelectItem?: (item: InventoryItem) => void
}) {
  const [sets, setSets] = useState<MasterySet[]>([])
  const [err, setErr] = useState('')
  const [loading, setLoading] = useState(true)
  const [q, setQ] = useState('')
  const [typeFilter, setTypeFilter] = useState<MasteryType | null>(null)
  const [onlyIncomplete, setOnlyIncomplete] = useState(false)

  useEffect(() => {
    let cancelled = false
    setErr('')
    setLoading(true)

    ;(async () => {
      try {
        const r = await api.masterySets()
        if (cancelled) return
        setSets(r.sets || [])
      } catch (e: any) {
        if (!cancelled) setErr(e.message || String(e))
      } finally {
        if (!cancelled) setLoading(false)
      }
    })()

    return () => {
      cancelled = true
    }
  }, [])

  const typeCounts = useMemo(() => {
    const counts: Record<string, number> = {}
    for (const s of sets) {
      const cat = normalizeCategory(s.category)
      if (!cat) continue
      counts[cat] = (counts[cat] || 0) + 1
    }
    return counts
  }, [sets])

  const filtered = useMemo(() => {
    let list = sets.filter((s) => normalizeCategory(s.category) != null)
    if (typeFilter) {
      list = list.filter((s) => normalizeCategory(s.category) === typeFilter)
    }
    if (q.trim()) {
      const qq = q.toLowerCase()
      list = list.filter(
        (s) =>
          s.name.toLowerCase().includes(qq) ||
          (s.name_ru || '').toLowerCase().includes(qq),
      )
    }
    if (onlyIncomplete) {
      list = list.filter((s) => {
        const incompletePart = s.parts.some((p) => p.count < (p.required ?? 1))
        return incompletePart || !s.mastered
      })
    }
    return list
  }, [sets, q, onlyIncomplete, typeFilter])

  // Only warm missing prices for currently visible cards (API already fills cached plat).
  const visibleUrls = useMemo(
    () =>
      filtered
        .slice(0, 60)
        .map((s) => s.url_name)
        .filter((u): u is string => !!u && u.endsWith('_set')),
    [filtered],
  )
  const pricing = useWarmVisiblePrices(
    visibleUrls,
    (url) => sets.some((s) => s.url_name === url && (s.platinum ?? 0) > 0),
    (url, platinum) => {
      setSets((prev) => prev.map((s) => (s.url_name === url ? { ...s, platinum } : s)))
    },
  )

  return (
    <div className="mastery-layout">
      <div className="mastery-toolbar">
        <div className="toolbar">
          <input
            className="search"
            value={q}
            onChange={(e) => setQ(e.target.value)}
            placeholder={t('filter_placeholder')}
          />
          <label className="chip">
            <input
              type="checkbox"
              checked={onlyIncomplete}
              onChange={(e) => setOnlyIncomplete(e.target.checked)}
            />{' '}
            {t('incomplete_sets')}
          </label>
          {loading && (
            <span className="muted" style={{ fontSize: '0.9rem' }}>
              {t('loading')}
            </span>
          )}
          {pricing && (
            <span className="muted" style={{ fontSize: '0.9rem' }}>
              {t('loading_prices')} {pricing.done}/{pricing.total}
            </span>
          )}
        </div>
        <div className="chips">
          <button
            type="button"
            className={`chip${typeFilter == null ? ' active' : ''}`}
            onClick={() => setTypeFilter(null)}
          >
            {t('cat_all')}
            <span className="muted" style={{ marginLeft: 6 }}>
              {sets.filter((s) => normalizeCategory(s.category)).length}
            </span>
          </button>
          {MASTERY_TYPES.map((id) => (
            <button
              key={id}
              type="button"
              className={`chip${typeFilter === id ? ' active' : ''}`}
              onClick={() => setTypeFilter((cur) => (cur === id ? null : id))}
            >
              {t(TYPE_I18N[id])}
              <span className="muted" style={{ marginLeft: 6 }}>
                {typeCounts[id] || 0}
              </span>
            </button>
          ))}
        </div>
      </div>
      <div className="mastery-body">
        {err && <div className="err">{err}</div>}
        <div className="set-grid">
          {filtered.map((s) => (
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
      </div>
    </div>
  )
}
