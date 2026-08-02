import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import {
  api,
  MARKET_ORDERS_CHANGED,
  notifyMarketOrdersChanged,
  readCachedMyOrders,
  writeCachedMyOrders,
  type InventoryItem,
  type MarketOrder,
  type MasterySet,
} from '../api'
import { ItemCard } from '../components/ItemCard'
import { isArcaneItem, isModItem } from '../itemClass'
import { buildUrlToSetMap } from './inventoryFilters'

function prettySlug(slug: string) {
  return slug
    .split('_')
    .filter(Boolean)
    .map((w) => w.charAt(0).toUpperCase() + w.slice(1))
    .join(' ')
}

function orderToItem(o: MarketOrder): InventoryItem {
  const name = o.item_name || prettySlug(o.item_url_name || '')
  const name_en = o.item_name_en || null
  const url = o.item_url_name || ''
  let item_type = 'misc'
  if (/^arcane_/i.test(url)) item_type = 'arcane'
  else if (o.rank != null) item_type = 'mod'
  return {
    unique_name: url || name,
    name,
    name_en:
      name_en && name_en.trim() && name_en.trim().toLowerCase() !== name.trim().toLowerCase()
        ? name_en
        : null,
    count: o.quantity || 1,
    mastered: false,
    item_type,
    url_name: o.item_url_name,
    platinum: o.platinum,
    thumb: o.thumb,
    favorite: false,
    rank: o.rank ?? null,
  }
}

function suggestionToItem(s: any): InventoryItem {
  const url = s.url_name || ''
  const unique = s.unique_name || url
  const item_type = s.item_type || (/^arcane_/i.test(url) ? 'arcane' : 'misc')
  return {
    unique_name: unique,
    name: s.inventory_name || s.name || prettySlug(url),
    count: s.count || 1,
    mastered: false,
    item_type,
    url_name: url || null,
    platinum: s.suggested_plat ?? null,
    thumb: s.thumb ?? null,
    favorite: false,
    rank: s.rank ?? null,
  }
}

function itemCardLabels(t: (k: string) => string) {
  return {
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
  }
}

export function MarketView({
  t,
  onAuthChange,
  onSelectItem,
}: {
  t: (k: string) => string
  onAuthChange?: () => void
  onSelectItem?: (item: InventoryItem) => void
}) {
  const [email, setEmail] = useState('')
  const [password, setPassword] = useState('')
  const [jwt, setJwt] = useState('')
  const [orders, setOrders] = useState<MarketOrder[]>(() => readCachedMyOrders() || [])
  const [suggestions, setSuggestions] = useState<any[]>([])
  const [masterySets, setMasterySets] = useState<MasterySet[]>([])
  const [msg, setMsg] = useState('')
  const [err, setErr] = useState('')
  const [busyId, setBusyId] = useState<string | null>(null)
  const [authed, setAuthed] = useState(false)
  const [loading, setLoading] = useState(() => !(readCachedMyOrders()?.length))
  const [q, setQ] = useState('')
  const [sugRanks, setSugRanks] = useState<Record<string, number>>({})
  const confirmTimer = useRef<number | null>(null)

  const applyOrders = useCallback((o: MarketOrder[]) => {
    setOrders(o)
    writeCachedMyOrders(o)
  }, [])

  const refresh = useCallback(async (opts?: { soft?: boolean; force?: boolean }) => {
    const soft = !!opts?.soft
    const force = !!opts?.force
    if (!soft) {
      setLoading(true)
      setErr('')
    }
    try {
      const auth = await api.marketAuth().catch(() => ({ authenticated: false }))
      setAuthed(!!auth.authenticated)
      if (auth.authenticated) {
        if (soft) {
          const [o, s] = await Promise.all([
            api.marketOrders({ refresh: force }).catch(() => [] as MarketOrder[]),
            api.marketSuggestions().catch(() => []),
          ])
          applyOrders(Array.isArray(o) ? o : [])
          setSuggestions(Array.isArray(s) ? s : [])
        } else {
          const [o, s, m] = await Promise.all([
            api.marketOrders({ refresh: force }).catch(() => [] as MarketOrder[]),
            api.marketSuggestions().catch(() => []),
            api.masterySets().catch(() => ({ sets: [] as MasterySet[] })),
          ])
          applyOrders(Array.isArray(o) ? o : [])
          setSuggestions(Array.isArray(s) ? s : [])
          setMasterySets(m?.sets || [])
        }
      } else {
        applyOrders([])
        setSuggestions([])
      }
    } catch (e: any) {
      if (!soft) setErr(e.message || String(e))
    } finally {
      if (!soft) setLoading(false)
    }
  }, [applyOrders])

  const syncOrders = useCallback(async () => {
    // Mutate handlers already refreshed daemon cache — prefer warm hit first.
    await refresh({ soft: true, force: false })
    if (confirmTimer.current != null) window.clearTimeout(confirmTimer.current)
    // WFM can lag briefly after mutate — confirm once more.
    confirmTimer.current = window.setTimeout(() => {
      void refresh({ soft: true, force: true })
      confirmTimer.current = null
    }, 750)
  }, [refresh])

  useEffect(() => {
    void refresh()
    return () => {
      if (confirmTimer.current != null) window.clearTimeout(confirmTimer.current)
    }
  }, [refresh])

  useEffect(() => {
    const onChanged = () => {
      void syncOrders()
    }
    window.addEventListener(MARKET_ORDERS_CHANGED, onChanged)
    return () => window.removeEventListener(MARKET_ORDERS_CHANGED, onChanged)
  }, [syncOrders])

  const runOrder = async (
    id: string,
    fn: () => Promise<unknown>,
    optimistic?: () => void,
  ) => {
    setBusyId(id)
    setErr('')
    setMsg('')
    try {
      optimistic?.()
      await fn()
      await syncOrders()
      notifyMarketOrdersChanged()
    } catch (e: any) {
      setErr(e.message || String(e))
      // Roll back optimistic UI from server truth.
      await refresh({ soft: true, force: true })
    } finally {
      setBusyId(null)
    }
  }

  const urlToSet = useMemo(() => buildUrlToSetMap(masterySets), [masterySets])

  const filteredOrders = useMemo(() => {
    const nq = q.trim().toLowerCase()
    if (!nq) return orders
    const matchingSetKeys = new Set<string>()
    for (const s of masterySets) {
      const hay = `${s.name_ru || ''} ${s.name} ${s.set_key}`.toLowerCase()
      if (hay.includes(nq)) matchingSetKeys.add(s.set_key)
    }
    return orders.filter((o) => {
      const set = o.item_url_name ? urlToSet.get(o.item_url_name) : undefined
      if (set && matchingSetKeys.has(set.set_key)) return true
      const name = (o.item_name || '').toLowerCase()
      const nameEn = (o.item_name_en || '').toLowerCase()
      const slug = (o.item_url_name || '').replace(/_/g, ' ').toLowerCase()
      const raw = (o.item_url_name || '').toLowerCase()
      return (
        name.includes(nq) ||
        nameEn.includes(nq) ||
        slug.includes(nq) ||
        raw.includes(nq)
      )
    })
  }, [orders, q, masterySets, urlToSet])

  const sellOrders = filteredOrders.filter((o) => o.order_type === 'sell')
  const buyOrders = filteredOrders.filter((o) => o.order_type !== 'sell')

  const labels = useMemo(() => itemCardLabels(t), [t])

  const openOrder = (o: MarketOrder) => {
    if (!o.item_url_name || !onSelectItem) return
    onSelectItem(orderToItem(o))
  }

  const renderOrder = (o: MarketOrder) => {
    const item = orderToItem(o)
    const isSell = o.order_type === 'sell'
    const busy = busyId === o.id
    return (
      <ItemCard
        key={o.id}
        item={item}
        labels={labels}
        onSelect={o.item_url_name ? () => openOrder(o) : undefined}
        actions={
          <>
            <span className={`chip ${isSell ? 'sell' : 'buy'}`}>{isSell ? 'WTS' : 'WTB'}</span>
            {o.visible === false && <span className="muted">{t('order_hidden')}</span>}
            <button
              className="btn sell"
              type="button"
              disabled={busy}
              title={isSell ? t('order_sold') : t('order_bought')}
              onClick={() =>
                void runOrder(
                  o.id,
                  async () => {
                    await api.closeOrder(o.id, 1)
                    setMsg(isSell ? t('order_sold_ok') : t('order_bought_ok'))
                  },
                  () => {
                    applyOrders(
                      orders.flatMap((x) => {
                        if (x.id !== o.id) return [x]
                        if (x.quantity <= 1) return []
                        return [{ ...x, quantity: x.quantity - 1 }]
                      }),
                    )
                  },
                )
              }
            >
              {isSell ? t('order_sold') : t('order_bought')}
            </button>
            <button
              className="btn ghost"
              type="button"
              disabled={busy}
              title={t('order_remove')}
              onClick={() =>
                void runOrder(
                  o.id,
                  async () => {
                    await api.deleteOrder(o.id)
                    setMsg(t('order_removed_ok'))
                  },
                  () => applyOrders(orders.filter((x) => x.id !== o.id)),
                )
              }
            >
              {t('order_remove')}
            </button>
          </>
        }
      />
    )
  }

  return (
    <>
      <div className="panel">
        <div className="row" style={{ justifyContent: 'space-between', alignItems: 'center' }}>
          <div className="muted">{authed ? t('market_signed_in') : t('sign_in')}</div>
          <button
            className="btn ghost sm"
            type="button"
            disabled={loading}
            onClick={() => void refresh({ force: true })}
          >
            {t('market_refresh')}
          </button>
        </div>
        {!authed && (
          <>
            <div className="row" style={{ marginTop: 8 }}>
              <input
                className="search"
                placeholder={t('email')}
                value={email}
                onChange={(e) => setEmail(e.target.value)}
              />
              <input
                className="search"
                type="password"
                placeholder={t('password')}
                value={password}
                onChange={(e) => setPassword(e.target.value)}
              />
              <button
                className="btn sm"
                type="button"
                onClick={() =>
                  api
                    .marketSignIn(email, password)
                    .then(() => {
                      setMsg(t('yes'))
                      onAuthChange?.()
                      return refresh()
                    })
                    .catch((e) => setErr(e.message))
                }
              >
                {t('sign_in_btn')}
              </button>
            </div>
            <div className="row" style={{ marginTop: 8 }}>
              <input
                className="search"
                placeholder={t('jwt_placeholder')}
                value={jwt}
                onChange={(e) => setJwt(e.target.value)}
              />
              <button
                className="btn ghost sm"
                type="button"
                onClick={() =>
                  api
                    .marketJwt(jwt)
                    .then(() => {
                      setMsg(t('yes'))
                      onAuthChange?.()
                      return refresh()
                    })
                    .catch((e) => setErr(e.message))
                }
              >
                {t('save_jwt')}
              </button>
            </div>
          </>
        )}
        {msg && (
          <div className="ok" style={{ marginTop: 8 }}>
            {msg}
          </div>
        )}
        {err && (
          <div className="err" style={{ marginTop: 8 }}>
            {err}
          </div>
        )}
      </div>

      <div className="market-orders-head">
        <h3>{t('your_orders')}</h3>
        <span className="muted">
          {sellOrders.length} WTS · {buyOrders.length} WTB
        </span>
      </div>

      {authed && (
        <div className="toolbar market-orders-search">
          <input
            className="search"
            value={q}
            onChange={(e) => setQ(e.target.value)}
            placeholder={t('filter_placeholder')}
          />
        </div>
      )}

      {loading && <div className="muted">{t('loading')}</div>}
      {!loading && authed && orders.length === 0 && (
        <div className="muted">{t('no_orders')}</div>
      )}
      {!loading && authed && orders.length > 0 && filteredOrders.length === 0 && (
        <div className="muted">{t('no_orders')}</div>
      )}

      {filteredOrders.length > 0 && (
        <div className="item-list">{filteredOrders.map(renderOrder)}</div>
      )}

      {authed && (
        <>
          <h3>{t('listing_suggestions')}</h3>
          <div className="item-list">
            {suggestions.length === 0 && <div className="muted">{t('no_suggestions')}</div>}
            {suggestions.map((s, i) => {
              const sugKey = `${s.url_name || s.inventory_name}-${i}`
              const item = suggestionToItem(s)
              const needsRank = isModItem(item) || isArcaneItem(item)
              const maxRank = isArcaneItem(item) ? 5 : 10
              const sugRank =
                sugRanks[sugKey] ?? (typeof s.rank === 'number' ? s.rank : item.rank ?? 0)
              return (
                <ItemCard
                  key={sugKey}
                  item={{ ...item, rank: needsRank ? sugRank : item.rank }}
                  labels={labels}
                  onSelect={
                    s.url_name && onSelectItem
                      ? () =>
                          onSelectItem({
                            ...item,
                            rank: needsRank ? sugRank : item.rank,
                          })
                      : undefined
                  }
                  rankEdit={
                    needsRank
                      ? {
                          value: sugRank,
                          max: maxRank,
                          onChange: (n) =>
                            setSugRanks((prev) => ({
                              ...prev,
                              [sugKey]: n,
                            })),
                        }
                      : undefined
                  }
                  actions={
                    <button
                      className="btn sell"
                      type="button"
                      disabled={!s.url_name || busyId === `sug-${i}`}
                      onClick={() =>
                        void runOrder(
                          `sug-${i}`,
                          async () => {
                            await api.createOrder({
                              item_url_name: s.url_name,
                              order_type: 'sell',
                              platinum: Math.max(1, Math.round(s.suggested_plat || 1)),
                              quantity: 1,
                              ...(needsRank ? { rank: sugRank } : {}),
                            })
                            setMsg(t('order_posted'))
                          },
                          () => {
                            const slug = s.url_name as string
                            const idx = orders.findIndex(
                              (x) =>
                                x.item_url_name === slug &&
                                x.order_type === 'sell' &&
                                (!needsRank || x.rank === sugRank),
                            )
                            if (idx < 0) return
                            const next = [...orders]
                            next[idx] = {
                              ...next[idx],
                              quantity: next[idx].quantity + 1,
                            }
                            applyOrders(next)
                          },
                        )
                      }
                    >
                      {t('list_btn')}
                    </button>
                  }
                />
              )
            })}
          </div>
        </>
      )}
    </>
  )
}
