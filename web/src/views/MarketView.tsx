import { useEffect, useState } from 'react'
import { api } from '../api'

export function MarketView({ t }: { t: (k: string) => string }) {
  const [email, setEmail] = useState('')
  const [password, setPassword] = useState('')
  const [jwt, setJwt] = useState('')
  const [orders, setOrders] = useState<any[]>([])
  const [suggestions, setSuggestions] = useState<any[]>([])
  const [msg, setMsg] = useState('')

  const refresh = () => {
    api.marketOrders().then(setOrders).catch(() => setOrders([]))
    api.marketSuggestions().then(setSuggestions).catch(() => setSuggestions([]))
  }

  useEffect(() => {
    refresh()
  }, [])

  return (
    <>
      <div className="panel">
        <div className="row">
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
                  refresh()
                })
                .catch((e) => setMsg(e.message))
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
                  refresh()
                })
                .catch((e) => setMsg(e.message))
            }
          >
            {t('save_jwt')}
          </button>
        </div>
        {msg && <div className="muted" style={{ marginTop: 8 }}>{msg}</div>}
      </div>

      <h3>{t('your_orders')}</h3>
      <div className="item-list">
        {orders.map((o, i) => (
          <div className="item-card" key={i}>
            <div className="item-thumb placeholder">{o.order_type}</div>
            <div className="item-body">
              <div className="item-title">{o.item_url_name}</div>
              <div className="item-meta">
                <span>{o.platinum}p</span>
                {o.rank != null && <span>R{o.rank}</span>}
                <span>×{o.quantity}</span>
              </div>
            </div>
          </div>
        ))}
      </div>

      <h3>{t('listing_suggestions')}</h3>
      <div className="item-list">
        {suggestions.map((s, i) => (
          <div className="item-card" key={i}>
            <div className="item-thumb placeholder">WTS</div>
            <div className="item-body">
              <div className="item-title">{s.inventory_name || s.name}</div>
              <div className="item-meta">
                <span>×{s.count}</span>
                <span>{s.suggested_plat}p</span>
              </div>
            </div>
            <button
              className="btn sell sm"
              type="button"
              onClick={() =>
                api
                  .createOrder({
                    item_url_name: s.url_name,
                    order_type: 'sell',
                    platinum: s.suggested_plat || 1,
                    quantity: 1,
                  })
                  .then(refresh)
              }
            >
              {t('list_btn')}
            </button>
          </div>
        ))}
      </div>
    </>
  )
}
