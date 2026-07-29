import { useEffect, useMemo, useState } from 'react'
import {
  api,
  type FissureInfo,
  type InventoryItem,
  type MarketOrder,
  type WorldStateSnapshot,
  wfmThumbUrl,
} from '../api'
import { PartThumb, thumbFromItem } from '../components/PartThumb'
import { isArcaneItem, isModItem } from '../itemClass'

type RailTab = 'timers' | 'market'

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
  const [orderType, setOrderType] = useState<'sell' | 'buy'>(defaultOrderType)
  const [busy, setBusy] = useState(false)
  const [msg, setMsg] = useState('')

  useEffect(() => {
    setOrderType(defaultOrderType)
  }, [defaultOrderType])

  useEffect(() => {
    if (selectedItem?.platinum) setPrice(Math.max(1, Math.round(selectedItem.platinum)))
    if (selectedItem?.count) setUnits(Math.min(selectedItem.count, 1))
    if (selectedItem) setTab('market')
  }, [selectedItem])

  useEffect(() => {
    let alive = true
    const load = () =>
      api
        .worldstate()
        .then((d) => alive && setWs(d))
        .catch(() => {})
    load()
    const id = setInterval(load, 45000)
    return () => {
      alive = false
      clearInterval(id)
    }
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
        // Keep inventory card in sync with the live sell book (lowest ingame/online)
        const sells = o
          .filter(
            (x) =>
              x.order_type === 'sell' &&
              x.visible !== false &&
              (x.status === 'ingame' || x.status === 'online') &&
              typeof x.platinum === 'number' &&
              x.platinum > 0,
          )
          .map((x) => x.platinum)
        if (sells.length && onItemPriced) {
          const floor = Math.min(...sells)
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
    setBusy(true)
    setMsg('')
    try {
      await api.createOrder({
        item_url_name: selectedItem.url_name,
        order_type: orderType,
        platinum: price,
        quantity: units,
      })
      setMsg(t('order_posted'))
      const o = await api.marketItemOrders(selectedItem.url_name)
      setOrders(o)
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
            <TimersPanel t={t} ws={ws} fissures={fissures} />
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

function TimersPanel({
  t,
  ws,
  fissures,
}: {
  t: (k: string) => string
  ws: WorldStateSnapshot | null
  fissures: FissureInfo[]
}) {
  const cycles = [
    { key: 'earth', ico: '🌍', c: ws?.earth },
    { key: 'cetus', ico: '🌅', c: ws?.cetus },
    { key: 'vallis', ico: '❄', c: ws?.vallis },
    { key: 'cambion', ico: '🦂', c: ws?.cambion },
  ]
  const baro = ws?.void_trader

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
            {c?.time_left || '—'}
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
          {baro?.expiry ? new Date(baro.expiry).toLocaleString() : ''}
        </div>
      </div>

      <h3 style={{ margin: '16px 0 8px', fontSize: 14 }}>{t('void_fissures')}</h3>
      {fissures.length === 0 && <div className="muted">{t('loading')}</div>}
      {fissures.map((f) => (
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
            {f.eta || '—'}
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
  orderType: 'sell' | 'buy'
  setOrderType: (t: 'sell' | 'buy') => void
  busy: boolean
  msg: string
  onPost: () => void
}) {
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

  const isMod = selectedItem ? isModItem(selectedItem) : false
  const isArcane = selectedItem ? isArcaneItem(selectedItem) : false
  const showRank = isMod || isArcane
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
            size={64}
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
          <div
            className="order-row"
            key={o.id || `${o.user}-${o.platinum}-${o.quantity}-${o.rank ?? ''}`}
          >
            <span
              className={`order-status${o.status ? ` ${o.status}` : ''}`}
              title={o.status || undefined}
            >
              {o.status || '—'}
            </span>
            <span style={{ flex: 1, overflow: 'hidden', textOverflow: 'ellipsis' }}>
              {o.user || '—'}
            </span>
            <span style={{ display: 'inline-flex', alignItems: 'center', gap: 6 }}>
              {showRank && o.rank != null && (
                <span className="order-rank" title={t('order_rank')}>
                  R{o.rank}
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
          </div>
        ))}
        {filtered.length === 0 && <div className="muted">{t('no_orders')}</div>}
      </div>
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
