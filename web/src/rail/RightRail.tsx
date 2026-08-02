import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import {
  api,
  notifyMarketOrdersChanged,
  type FissureInfo,
  type InventoryItem,
  type MarketOrder,
  type WorldStateSnapshot,
  wfmThumbUrl,
} from '../api'
import { PartThumb, thumbFromItem } from '../components/PartThumb'
import { isArcaneItem, isModItem } from '../itemClass'

type RailTab = 'timers' | 'market'

function prettySlug(slug: string) {
  return slug
    .split('_')
    .filter(Boolean)
    .map((w) => w.charAt(0).toUpperCase() + w.slice(1))
    .join(' ')
}

/** English-ish WFM label for in-game whisper (AlecaFrame-style). */
function whisperItemLabel(item: InventoryItem, order: MarketOrder): string {
  const slug = (order.item_url_name || item.url_name || '').trim()
  let name = slug ? prettySlug(slug) : item.name
  const isRelic = /_relic$/i.test(slug) || /relic/i.test(item.item_type || '')
  if (isRelic) {
    name = name.replace(/\s*Relic$/i, '').trim()
    const raw = (order.subtype || 'intact').toLowerCase()
    const quality = raw.charAt(0).toUpperCase() + raw.slice(1)
    return `${name} (${quality})`
  }
  return name
}

/** `/w User Hi! I want to buy/sell: "Item" for N platinum. (…)`. */
function buildWhisper(user: string, order: MarketOrder, item: InventoryItem): string {
  // Sell order → they sell → we buy; buy order → they buy → we sell.
  const want = order.order_type === 'sell' ? 'buy' : 'sell'
  const label = whisperItemLabel(item, order)
  const plat = Math.max(1, Math.round(order.platinum || 0))
  return `/w ${user} Hi! I want to ${want}: "${label}" for ${plat} platinum. (warframe.market through BNGframe)`
}

export function RightRail({
  t,
  selectedItem,
  defaultOrderType,
  onItemPriced,
}: {
  t: (k: string) => string
  selectedItem: InventoryItem | null
  defaultOrderType: 'sell' | 'buy'
  onItemPriced?: (urlName: string, platinum: number) => void
}) {
  const [tab, setTab] = useState<RailTab>('timers')
  const [ws, setWs] = useState<WorldStateSnapshot | null>(null)
  const [orders, setOrders] = useState<MarketOrder[]>([])
  const [units, setUnits] = useState(1)
  const [price, setPrice] = useState(1)
  const [rank, setRank] = useState(0)
  const [orderType, setOrderType] = useState<'sell' | 'buy'>(defaultOrderType)
  const [busy, setBusy] = useState(false)
  const [msg, setMsg] = useState('')

  useEffect(() => {
    setOrderType(defaultOrderType)
  }, [defaultOrderType])

  useEffect(() => {
    if (selectedItem?.platinum) setPrice(Math.max(1, Math.round(selectedItem.platinum)))
    if (selectedItem?.count) setUnits(Math.min(selectedItem.count, 1))
    if (selectedItem) {
      setRank(Math.max(0, selectedItem.rank ?? 0))
      setTab('market')
    }
  }, [selectedItem])

  useEffect(() => {
    let alive = true
    const load = (force = false) =>
      api
        .worldstate({ refresh: force })
        .then((d) => alive && setWs(d))
        .catch(() => {})
    load(false)
    const id = setInterval(() => load(false), 30000)
    return () => {
      alive = false
      clearInterval(id)
    }
  }, [])

  const refreshWorldstate = useCallback(() => {
    api
      .worldstate({ refresh: true })
      .then((d) => setWs(d))
      .catch(() => {})
  }, [])

  useEffect(() => {
    if (tab !== 'market') return
    const slug = selectedItem?.url_name
    if (!slug) {
      setOrders([])
      return
    }
    let alive = true
    api
      .marketItemOrders(slug)
      .then((o) => {
        if (!alive) return
        setOrders(o)
        // Keep inventory card in sync with the live sell book (ingame floor, else online)
        const sells = o
          .filter(
            (x) =>
              x.order_type === 'sell' &&
              x.visible !== false &&
              typeof x.platinum === 'number' &&
              x.platinum > 0,
          )
        const ingame = sells.filter((x) => x.status === 'ingame').map((x) => x.platinum)
        const online = sells.filter((x) => x.status === 'online').map((x) => x.platinum)
        const pool = ingame.length ? ingame : online.length ? online : sells.map((x) => x.platinum)
        if (pool.length && onItemPriced) {
          const floor = Math.min(...pool)
          onItemPriced(slug, floor)
          setPrice(Math.max(1, Math.round(floor)))
        }
      })
      .catch(() => alive && setOrders([]))
    return () => {
      alive = false
    }
  }, [tab, selectedItem?.url_name, onItemPriced])

  const fissures = useMemo(() => {
    const list = ws?.fissures || []
    return list.filter((f) => !f.is_storm).slice(0, 24)
  }, [ws])

  const post = async () => {
    if (!selectedItem?.url_name) return
    const needsRank = isModItem(selectedItem) || isArcaneItem(selectedItem)
    setBusy(true)
    setMsg('')
    try {
      await api.createOrder({
        item_url_name: selectedItem.url_name,
        order_type: orderType,
        platinum: price,
        quantity: units,
        ...(needsRank ? { rank } : {}),
      })
      setMsg(t('order_posted'))
      notifyMarketOrdersChanged()
      // Refresh public book + confirm a moment later (WFM lag).
      const reloadBook = () =>
        api
          .marketItemOrders(selectedItem.url_name!)
          .then((o) => setOrders(o))
          .catch(() => {})
      await reloadBook()
      window.setTimeout(() => void reloadBook(), 800)
    } catch (e: any) {
      setMsg(e.message || String(e))
    } finally {
      setBusy(false)
    }
  }

  return (
    <aside className="rail">
      <div className="rail-tabs">
        <button
          type="button"
          className={tab === 'timers' ? 'active' : ''}
          onClick={() => setTab('timers')}
        >
          {t('rail_timers')}
        </button>
        <button
          type="button"
          className={tab === 'market' ? 'active' : ''}
          onClick={() => setTab('market')}
        >
          {t('rail_market')}
        </button>
      </div>
      <div className="rail-body">
        {tab === 'timers' ? (
          <div className="timers-panel">
            <TimersPanel t={t} ws={ws} fissures={fissures} onExpired={refreshWorldstate} />
          </div>
        ) : (
          <MarketPanel
            t={t}
            selectedItem={selectedItem}
            orders={orders}
            units={units}
            setUnits={setUnits}
            price={price}
            setPrice={setPrice}
            rank={rank}
            setRank={setRank}
            orderType={orderType}
            setOrderType={setOrderType}
            busy={busy}
            msg={msg}
            onPost={post}
          />
        )}
      </div>
    </aside>
  )
}

function formatRemaining(expiry?: string | null, fallback?: string | null): string {
  if (expiry) {
    const ms = new Date(expiry).getTime() - Date.now()
    if (Number.isNaN(ms)) return fallback || '—'
    if (ms <= 0) return '0s'
    const total = Math.floor(ms / 1000)
    const h = Math.floor(total / 3600)
    const m = Math.floor((total % 3600) / 60)
    const s = total % 60
    if (h > 0) return `${h}h ${m}m ${s}s`
    if (m > 0) return `${m}m ${s}s`
    return `${s}s`
  }
  return fallback || '—'
}

function isExpired(expiry?: string | null): boolean {
  if (!expiry) return false
  const ms = new Date(expiry).getTime() - Date.now()
  return !Number.isNaN(ms) && ms <= 0
}

function TimersPanel({
  t,
  ws,
  fissures,
  onExpired,
}: {
  t: (k: string) => string
  ws: WorldStateSnapshot | null
  fissures: FissureInfo[]
  onExpired: () => void
}) {
  const [, setTick] = useState(0)
  const lastRefreshAt = useRef(0)

  useEffect(() => {
    const id = window.setInterval(() => {
      setTick((n) => n + 1)

      const expiries: (string | null | undefined)[] = [
        ws?.earth?.expiry,
        ws?.cetus?.expiry,
        ws?.vallis?.expiry,
        ws?.cambion?.expiry,
        ws?.void_trader?.active ? ws.void_trader.expiry : ws?.void_trader?.activation,
        ...fissures.map((f) => f.expiry),
      ]
      if (!expiries.some(isExpired)) return

      const now = Date.now()
      // Debounce force-refresh while warframestat catches up.
      if (now - lastRefreshAt.current < 4000) return
      lastRefreshAt.current = now
      onExpired()
    }, 1000)
    return () => window.clearInterval(id)
  }, [ws, fissures, onExpired])

  const cycles = [
    { key: 'earth', ico: '🌍', c: ws?.earth },
    { key: 'cetus', ico: '🌅', c: ws?.cetus },
    { key: 'vallis', ico: '❄', c: ws?.vallis },
    { key: 'cambion', ico: '🦂', c: ws?.cambion },
  ]
  const baro = ws?.void_trader
  const liveFissures = fissures.filter((f) => {
    if (!f.expiry) return true
    return new Date(f.expiry).getTime() > Date.now()
  })

  return (
    <>
      <h3 style={{ margin: '0 0 8px', fontSize: 14 }}>{t('world_cycles')}</h3>
      {cycles.map(({ key, ico, c }) => (
        <div className="cycle-row" key={key}>
          <div className="cycle-ico">{ico}</div>
          <div>
            <div>{t(`cycle_${key}`)}</div>
            <div className="muted" style={{ fontSize: 12 }}>
              {c?.state ? t(`state_${c.state}`) || c.state : '—'}
            </div>
          </div>
          <div className="muted" style={{ fontSize: 12 }}>
            {formatRemaining(c?.expiry, c?.time_left)}
          </div>
        </div>
      ))}

      <div className="panel" style={{ marginTop: 12, padding: 10 }}>
        <div style={{ fontWeight: 600 }}>{baro?.character || "Baro Ki'Teer"}</div>
        <div className="muted" style={{ fontSize: 12 }}>
          {baro?.location || '—'}
        </div>
        <div style={{ marginTop: 4, fontSize: 13 }}>
          {baro?.active ? t('baro_active') : t('baro_leaves')}{' '}
          {baro?.active
            ? baro?.expiry
              ? new Date(baro.expiry).toLocaleString()
              : ''
            : baro?.activation
              ? new Date(baro.activation).toLocaleString()
              : baro?.expiry
                ? new Date(baro.expiry).toLocaleString()
                : ''}
        </div>
        {(baro?.active ? baro.expiry : baro?.activation || baro?.expiry) && (
          <div className="muted" style={{ fontSize: 12, marginTop: 2 }}>
            {formatRemaining(baro?.active ? baro.expiry : baro?.activation || baro?.expiry)}
          </div>
        )}
      </div>

      <h3 style={{ margin: '16px 0 8px', fontSize: 14 }}>{t('void_fissures')}</h3>
      {liveFissures.length === 0 && <div className="muted">{t('loading')}</div>}
      {liveFissures.map((f) => (
        <div className="fissure-row" key={f.id}>
          <div className="tier">{f.tier.slice(0, 4)}</div>
          <div>
            <div>{f.mission_type}</div>
            <div className="muted" style={{ fontSize: 12 }}>
              {f.node}
              {f.is_hard ? ' · SP' : ''}
            </div>
          </div>
          <div className="muted" style={{ fontSize: 12 }}>
            {formatRemaining(f.expiry, f.eta)}
          </div>
        </div>
      ))}
    </>
  )
}

function MarketPanel({
  t,
  selectedItem,
  orders,
  units,
  setUnits,
  price,
  setPrice,
  rank,
  setRank,
  orderType,
  setOrderType,
  busy,
  msg,
  onPost,
}: {
  t: (k: string) => string
  selectedItem: InventoryItem | null
  orders: MarketOrder[]
  units: number
  setUnits: (n: number) => void
  price: number
  setPrice: (n: number) => void
  rank: number
  setRank: (n: number) => void
  orderType: 'sell' | 'buy'
  setOrderType: (t: 'sell' | 'buy') => void
  busy: boolean
  msg: string
  onPost: () => void
}) {
  const [copyMsg, setCopyMsg] = useState('')
  const filtered = useMemo(() => {
    const want = orderType === 'sell' ? 'sell' : 'buy'
    const rank = (status?: string | null) => {
      if (status === 'ingame') return 0
      if (status === 'online') return 1
      if (status === 'offline') return 2
      return 3
    }
    const list = orders.filter((o) => o.order_type === want && o.visible !== false)
    list.sort((a, b) => {
      const rs = rank(a.status) - rank(b.status)
      if (rs !== 0) return rs
      return want === 'sell' ? a.platinum - b.platinum : b.platinum - a.platinum
    })
    return list.slice(0, 40)
  }, [orders, orderType])

  const copyWhisper = async (o: MarketOrder) => {
    if (!selectedItem) return
    const user = (o.user || '').trim()
    if (!user) {
      setCopyMsg(t('whisper_no_user'))
      return
    }
    const text = buildWhisper(user, o, selectedItem)
    try {
      await navigator.clipboard.writeText(text)
      setCopyMsg(t('whisper_copied'))
    } catch {
      setCopyMsg(text)
    }
  }

  const isMod = selectedItem ? isModItem(selectedItem) : false
  const isArcane = selectedItem ? isArcaneItem(selectedItem) : false
  const showRank = isMod || isArcane
  const maxRank = isArcane ? 5 : isMod ? 10 : 0
  const thumb = selectedItem
    ? thumbFromItem({
        urlName: selectedItem.url_name,
        name: selectedItem.name,
        wfmThumbUrl: wfmThumbUrl(selectedItem.thumb) || undefined,
        skipPartRole: isMod || isArcane,
      })
    : null

  return (
    <div className="market-panel">
      <div className="row" style={{ marginBottom: 8, alignItems: 'center', gap: 8, flexShrink: 0 }}>
        {thumb ? (
          <PartThumb
            urls={thumb.mainUrls}
            partUrls={!isMod && !isArcane && thumb.visual.isPart ? thumb.partUrls : undefined}
            size={128}
            tone={isMod || isArcane ? null : thumb.tone}
          />
        ) : null}
        <div className="muted" style={{ fontSize: 13, fontWeight: 600, color: 'var(--text, #ddd)' }}>
          {selectedItem ? selectedItem.name : t('pick_item_for_market')}
        </div>
      </div>
      <div className="row" style={{ marginBottom: 8, flexShrink: 0 }}>
        <button
          type="button"
          className={`chip${orderType === 'sell' ? ' active' : ''}`}
          onClick={() => setOrderType('sell')}
        >
          {t('wts')}
        </button>
        <button
          type="button"
          className={`chip${orderType === 'buy' ? ' active' : ''}`}
          onClick={() => setOrderType('buy')}
        >
          {t('wtb')}
        </button>
      </div>
      <div className="order-list">
        {filtered.map((o) => (
          <button
            type="button"
            className="order-row"
            key={o.id || `${o.user}-${o.platinum}-${o.quantity}-${o.rank ?? ''}-${o.subtype ?? ''}`}
            disabled={!o.user || !selectedItem}
            title={t('whisper_copy_hint')}
            onClick={() => void copyWhisper(o)}
          >
            <span
              className={`order-status${o.status ? ` ${o.status}` : ''}`}
              title={o.status || undefined}
            >
              {o.status || '—'}
            </span>
            <span className="order-user">{o.user || '—'}</span>
            <span style={{ display: 'inline-flex', alignItems: 'center', gap: 6 }}>
              {showRank && o.rank != null && (
                <span className="order-rank" title={t('order_rank')}>
                  R{o.rank}
                </span>
              )}
              {o.subtype && (
                <span className="order-subtype" title={o.subtype}>
                  {o.subtype.charAt(0).toUpperCase() + o.subtype.slice(1)}
                </span>
              )}
              <span>×{o.quantity ?? 1}</span>
            </span>
            <span style={{ fontWeight: 600, display: 'inline-flex', alignItems: 'center', gap: 4 }}>
              {o.platinum}
              <img
                src="/api/img?u=https%3A%2F%2Fwiki.warframe.com%2Fimages%2FPlatinum.png"
                alt=""
                width={12}
                height={12}
                style={{ objectFit: 'contain' }}
                referrerPolicy="no-referrer"
              />
            </span>
          </button>
        ))}
        {filtered.length === 0 && <div className="muted">{t('no_orders')}</div>}
      </div>
      {copyMsg && (
        <div className="ok" style={{ marginTop: 6, fontSize: 12 }}>
          {copyMsg}
        </div>
      )}
      <div className="post-box">
        <div style={{ fontWeight: 600 }}>
          {orderType === 'sell' ? t('post_sell') : t('post_buy')}
        </div>
        <div className="row">
          <label>
            {t('units')}{' '}
            <input
              type="number"
              min={1}
              value={units}
              onChange={(e) => setUnits(Number(e.target.value) || 1)}
            />
          </label>
          <label>
            {t('col_plat')}{' '}
            <input
              type="number"
              min={1}
              value={price}
              onChange={(e) => setPrice(Number(e.target.value) || 1)}
            />
          </label>
          {showRank && (
            <label>
              {t('order_rank')}{' '}
              <input
                type="number"
                min={0}
                max={maxRank}
                value={rank}
                onChange={(e) => {
                  const n = Number(e.target.value)
                  setRank(Number.isFinite(n) ? Math.max(0, Math.min(maxRank, Math.round(n))) : 0)
                }}
              />
            </label>
          )}
        </div>
        <button
          className={`btn ${orderType === 'sell' ? 'sell' : 'buy'}`}
          style={{ width: '100%', marginTop: 10 }}
          disabled={busy || !selectedItem?.url_name}
          onClick={onPost}
          type="button"
        >
          {orderType === 'sell' ? t('post_sell') : t('post_buy')}
        </button>
        {msg && (
          <div className={msg === t('order_posted') ? 'ok' : 'err'} style={{ marginTop: 8 }}>
            {msg}
          </div>
        )}
      </div>
    </div>
  )
}
