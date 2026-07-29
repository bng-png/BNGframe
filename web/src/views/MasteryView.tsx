import { useEffect, useMemo, useState } from 'react'
import { api, type MasterySet } from '../api'
import { SetCard } from '../components/SetCard'

const MASTERY_TYPES = [
  'warframe',
  'primary',
  'secondary',
  'melee',
  'companion',
  'archwing',
  'modular',
] as const

type MasteryType = (typeof MASTERY_TYPES)[number]

const TYPE_I18N: Record<MasteryType, string> = {
  warframe: 'cat_warframe',
  primary: 'cat_primary',
  secondary: 'cat_secondary',
  melee: 'cat_melee',
  companion: 'cat_companion',
  archwing: 'cat_archwing',
  modular: 'cat_modular',
}

function normalizeCategory(cat: string): MasteryType | null {
  if ((MASTERY_TYPES as readonly string[]).includes(cat)) return cat as MasteryType
  if (cat === 'prime' || cat === 'weapon_prime' || cat === 'weapon') return 'primary'
  return null
}

export function MasteryView({ t }: { t: (k: string) => string }) {
  const [sets, setSets] = useState<MasterySet[]>([])
  const [err, setErr] = useState('')
  const [q, setQ] = useState('')
  const [typeFilter, setTypeFilter] = useState<MasteryType | null>(null)
  const [onlyIncomplete, setOnlyIncomplete] = useState(false)
  const [pricing, setPricing] = useState<{ done: number; total: number } | null>(null)

  useEffect(() => {
    let cancelled = false
    setErr('')
    setPricing(null)

    ;(async () => {
      try {
        const r = await api.masterySets()
        if (cancelled) return
        const list = r.sets || []
        setSets(list)

        const targets = list
          .map((s) => s.url_name)
          .filter((u): u is string => !!u && u.endsWith('_set'))
        // unique preserve order
        const seen = new Set<string>()
        const urls = targets.filter((u) => (seen.has(u) ? false : (seen.add(u), true)))

        if (!urls.length) return
        setPricing({ done: 0, total: urls.length })

        for (let i = 0; i < urls.length; i++) {
          if (cancelled) return
          const url = urls[i]
          try {
            const p = await api.priceItem(url)
            if (cancelled) return
            if (p.platinum > 0) {
              setSets((prev) =>
                prev.map((s) => (s.url_name === url ? { ...s, platinum: p.platinum } : s)),
              )
            }
          } catch {
            /* ignore single failures */
          }
          if (!cancelled) setPricing({ done: i + 1, total: urls.length })
        }
        if (!cancelled) setPricing(null)
      } catch (e: any) {
        if (!cancelled) setErr(e.message || String(e))
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

  return (
    <>
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
        {pricing && (
          <span className="muted" style={{ fontSize: '0.9rem' }}>
            {t('loading_prices')} {pricing.done}/{pricing.total}
          </span>
        )}
      </div>
      <div className="chips" style={{ marginBottom: 16 }}>
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
      {err && <div className="err">{err}</div>}
      <div className="set-grid">
        {filtered.map((s) => (
          <SetCard
            key={s.set_key}
            set={s}
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
      {!filtered.length && !err && <div className="muted">{t('no_sets')}</div>}
    </>
  )
}
