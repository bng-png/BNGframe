import { useCallback, useEffect, useMemo, useState } from 'react'
import {
  api,
  connectWs,
  type DaemonStatus,
  type InventoryCacheMeta,
  type InventoryItem,
  type RewardSnapshot,
} from './api'
import { I18nProvider, useI18n, type Locale } from './i18n'

type Tab =
  | 'overview'
  | 'inventory'
  | 'relics'
  | 'market'
  | 'rivens'
  | 'stats'
  | 'analytics'
  | 'settings'

export default function App() {
  return (
    <I18nProvider>
      <AppInner />
    </I18nProvider>
  )
}

function AppInner() {
  const { t, locale } = useI18n()
  const [tab, setTab] = useState<Tab>('overview')
  const [status, setStatus] = useState<DaemonStatus | null>(null)
  const [rewards, setRewards] = useState<RewardSnapshot[]>([])
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)

  const tabs = useMemo(
    () =>
      [
        { id: 'overview' as const, label: t('tab_overview') },
        { id: 'inventory' as const, label: t('tab_inventory') },
        { id: 'relics' as const, label: t('tab_relics') },
        { id: 'market' as const, label: t('tab_market') },
        { id: 'rivens' as const, label: t('tab_rivens') },
        { id: 'stats' as const, label: t('tab_stats') },
        { id: 'analytics' as const, label: t('tab_analytics') },
        { id: 'settings' as const, label: t('tab_settings') },
      ],
    [t],
  )

  const refresh = useCallback(async () => {
    try {
      const [s, r] = await Promise.all([api.status(), api.rewards()])
      setStatus(s)
      setRewards(r)
      setError(null)
    } catch (e: any) {
      setError(e.message || String(e))
    }
  }, [])

  useEffect(() => {
    refresh()
    const ws = connectWs(() => refresh())
    const timer = setInterval(refresh, 8000)
    return () => {
      ws.close()
      clearInterval(timer)
    }
  }, [refresh])

  async function run(fn: () => Promise<unknown>) {
    setBusy(true)
    setError(null)
    try {
      await fn()
      await refresh()
    } catch (e: any) {
      setError(e.message || String(e))
    } finally {
      setBusy(false)
    }
  }

  return (
    <div className="app">
      <header className="brand">
        <div>
          <h1>
            BNG<span>frame</span>
          </h1>
          <p>{t('tagline')}</p>
        </div>
        <div className="stat" style={{ minWidth: 180 }}>
          <div className="label">{t('daemon')}</div>
          <div className="value" style={{ fontSize: '1rem' }}>
            {status?.phase || '…'} · {status?.uptime_secs ?? 0}s
          </div>
        </div>
      </header>

      <nav className="tabs">
        {tabs.map((item) => (
          <button
            key={item.id}
            className={tab === item.id ? 'active' : ''}
            onClick={() => setTab(item.id)}
          >
            {item.label}
          </button>
        ))}
      </nav>

      {error && (
        <div className="panel" style={{ marginBottom: 12, borderColor: 'var(--danger)' }}>
          <span className="err">{error}</span>
        </div>
      )}

      {tab === 'overview' && (
        <Overview status={status} rewards={rewards} busy={busy} run={run} locale={locale} />
      )}
      {tab === 'inventory' && <InventoryTab busy={busy} run={run} />}
      {tab === 'relics' && <RelicsTab />}
      {tab === 'market' && <MarketTab busy={busy} run={run} />}
      {tab === 'rivens' && <RivensTab />}
      {tab === 'stats' && <StatsTab />}
      {tab === 'analytics' && <AnalyticsTab />}
      {tab === 'settings' && <SettingsTab status={status} busy={busy} run={run} onSaved={refresh} />}
    </div>
  )
}

function Overview({
  status,
  rewards,
  busy,
  run,
  locale,
}: {
  status: DaemonStatus | null
  rewards: RewardSnapshot[]
  busy: boolean
  run: (fn: () => Promise<unknown>) => Promise<void>
  locale: Locale
}) {
  const { t } = useI18n()
  const latest = rewards[0]
  return (
    <div className="panel">
      <div className="grid" style={{ marginBottom: 16 }}>
        <div className="stat">
          <div className="label">{t('eelog')}</div>
          <div className="value" style={{ fontSize: '1rem' }}>
            {status?.eelog_exists ? (
              <span className="ok">{t('found')}</span>
            ) : (
              <span className="warn">{t('missing')}</span>
            )}
          </div>
          <div className="muted" style={{ fontSize: 12, marginTop: 6 }}>
            {status?.eelog_path}
          </div>
        </div>
        <div className="stat">
          <div className="label">{t('items_cached')}</div>
          <div className="value">{status?.items_cached ?? 0}</div>
        </div>
        <div className="stat">
          <div className="label">{t('inventory')}</div>
          <div className="value" style={{ fontSize: '1rem' }}>
            {status?.inventory_loaded ? <span className="ok">{t('loaded')}</span> : t('empty')}
          </div>
        </div>
        <div className="stat">
          <div className="label">{t('watching')}</div>
          <div className="value" style={{ fontSize: '1rem' }}>
            {status?.watching_eelog ? <span className="ok">{t('yes')}</span> : t('no')}
          </div>
        </div>
      </div>

      <div className="row">
        <button className="btn" disabled={busy} onClick={() => run(() => api.triggerReward())}>
          {t('trigger_ocr')}
        </button>
        <button
          className="btn secondary"
          disabled={busy}
          onClick={() => run(() => api.refreshItems())}
        >
          {t('refresh_items')}
        </button>
        <button
          className="btn secondary"
          disabled={busy}
          onClick={() => run(() => api.refreshPrices(80))}
        >
          {t('refresh_prices')}
        </button>
        <a className="btn secondary" href="/overlay" target="_blank" rel="noreferrer">
          {t('open_overlay')}
        </a>
      </div>
      <p className="muted">{t('prices_hint')}</p>

      <h3>{t('last_rewards')}</h3>
      {!latest && <p className="muted">{t('no_rewards')}</p>}
      {latest && (
        <>
          <p className="muted">
            {new Date(latest.detected_at).toLocaleString(locale === 'ru' ? 'ru-RU' : 'en-US')} ·{' '}
            {t('source')}={latest.source}
          </p>
          <div className="reward-cards">
            {latest.slots.map((s, i) => (
              <div key={i} className={`reward-card ${latest.best_index === i ? 'best' : ''}`}>
                <div className="muted">#{s.rank ?? i + 1}</div>
                <strong>{s.name}</strong>
                <div className="row" style={{ marginTop: 8, marginBottom: 0 }}>
                  <span>{s.platinum != null ? `${s.platinum.toFixed(0)}p` : '—'}</span>
                  <span className="muted">{s.ducats != null ? `${s.ducats}d` : ''}</span>
                </div>
              </div>
            ))}
          </div>
        </>
      )}
    </div>
  )
}

function InventoryTab({
  busy,
  run,
}: {
  busy: boolean
  run: (fn: () => Promise<unknown>) => Promise<void>
}) {
  const { t } = useI18n()
  const [items, setItems] = useState<InventoryItem[]>([])
  const [cache, setCache] = useState<InventoryCacheMeta | null>(null)
  const [q, setQ] = useState('')
  const [onlyDup, setOnlyDup] = useState(false)
  const [onlyTrade, setOnlyTrade] = useState(false)
  const [view, setView] = useState<'all' | 'mastery' | 'foundry'>('all')

  const load = useCallback(async () => {
    // Meta is instant; items can be large — show cache status immediately
    try {
      setCache(await api.inventoryCache())
    } catch {
      /* ignore */
    }
    const resp = await api.inventory()
    const items = Array.isArray(resp) ? resp : resp.items
    const cacheMeta = Array.isArray(resp) ? null : resp.cache
    setItems(items)
    if (cacheMeta) setCache(cacheMeta)
  }, [])

  useEffect(() => {
    load().catch(() => {})
  }, [load])

  function formatCacheAge(secs?: number | null) {
    if (secs == null) return t('cache_empty')
    if (secs < 90) return t('cache_just_now')
    if (secs < 3600) return t('cache_minutes', { n: Math.round(secs / 60) })
    if (secs < 86400) return t('cache_hours', { n: Math.round(secs / 3600) })
    return t('cache_days', { n: Math.round(secs / 86400) })
  }

  const filtered = useMemo(() => {
    return items.filter((i) => {
      if (view === 'mastery' && i.mastered) return false
      if (
        view === 'foundry' &&
        i.item_type !== 'blueprint' &&
        !i.name.toLowerCase().includes('blueprint')
      )
        return false
      if (onlyDup && i.count <= 1) return false
      if (onlyTrade && !i.url_name) return false
      if (q && !i.name.toLowerCase().includes(q.toLowerCase())) return false
      return true
    })
  }, [items, q, onlyDup, onlyTrade, view])

  return (
    <div className="panel">
      <div className="row">
        <button
          className="btn"
          disabled={busy}
          onClick={() =>
            run(async () => {
              await api.syncInventory()
              await load()
            })
          }
        >
          {t('sync_memory')}
        </button>
        <button
          className="btn secondary"
          disabled={busy}
          onClick={() =>
            run(async () => {
              await api.importInventory()
              await load()
            })
          }
        >
          {t('import_dump')}
        </button>
        <button
          className="btn secondary"
          disabled={busy}
          onClick={() =>
            run(async () => {
              await api.refreshPrices(120)
              await load()
            })
          }
        >
          {t('refresh_prices')}
        </button>
        <input
          placeholder={t('filter_placeholder')}
          value={q}
          onChange={(e) => setQ(e.target.value)}
        />
        <select value={view} onChange={(e) => setView(e.target.value as typeof view)}>
          <option value="all">{t('view_all')}</option>
          <option value="mastery">{t('view_mastery')}</option>
          <option value="foundry">{t('view_foundry')}</option>
        </select>
        <label>
          <input type="checkbox" checked={onlyDup} onChange={(e) => setOnlyDup(e.target.checked)} />{' '}
          {t('duplicates')}
        </label>
        <label>
          <input
            type="checkbox"
            checked={onlyTrade}
            onChange={(e) => setOnlyTrade(e.target.checked)}
          />{' '}
          {t('tradable_guess')}
        </label>
      </div>
      <p className="muted">{t('inventory_hint')}</p>
      <p className="muted">
        {t('cache_label')}:{' '}
        {cache?.cached
          ? t('cache_age', { age: formatCacheAge(cache.age_secs) }) +
            ` · ${cache.item_count} · ${cache.method}`
          : t('cache_empty')}
      </p>
      <table>
        <thead>
          <tr>
            <th>{t('col_name')}</th>
            <th>{t('col_type')}</th>
            <th>{t('col_qty')}</th>
            <th>{t('col_plat')}</th>
            <th>{t('col_mastered')}</th>
            <th>★</th>
          </tr>
        </thead>
        <tbody>
          {filtered.slice(0, 200).map((i) => (
            <tr key={i.unique_name}>
              <td>{i.name}</td>
              <td className="muted">{i.item_type}</td>
              <td>{i.count}</td>
              <td>{i.platinum != null ? i.platinum.toFixed(0) : '—'}</td>
              <td>{i.mastered ? t('yes') : ''}</td>
              <td>
                <button
                  className="btn secondary"
                  style={{ padding: '2px 8px' }}
                  onClick={async () => {
                    await api.favorite(i.unique_name, !i.favorite)
                    await load()
                  }}
                >
                  {i.favorite ? '★' : '☆'}
                </button>
              </td>
            </tr>
          ))}
        </tbody>
      </table>
      <p className="muted">
        {t('showing', {
          shown: Math.min(200, filtered.length),
          total: filtered.length,
        })}
      </p>
    </div>
  )
}

function RelicsTab() {
  const { t } = useI18n()
  const [relics, setRelics] = useState<any[]>([])
  const [planner, setPlanner] = useState<any>({
    order_mode: 'ducats_profit',
    favorites_first: false,
    min_plat: 0,
    ducat_focus: true,
    mr_focus: false,
  })

  async function load() {
    const [p, r] = await Promise.all([api.planner(), api.relics()])
    setPlanner(p)
    setRelics(r)
  }

  useEffect(() => {
    load().catch(() => {})
  }, [])

  return (
    <div className="panel">
      <div className="row">
        <select
          value={planner.order_mode}
          onChange={(e) => setPlanner({ ...planner, order_mode: e.target.value })}
        >
          <option value="ducats_profit">{t('order_ducats')}</option>
          <option value="platinum">{t('order_platinum')}</option>
          <option value="best_for_mr">{t('order_mr')}</option>
        </select>
        <label>
          <input
            type="checkbox"
            checked={!!planner.favorites_first}
            onChange={(e) => setPlanner({ ...planner, favorites_first: e.target.checked })}
          />{' '}
          {t('favorites_first')}
        </label>
        <button
          className="btn"
          onClick={async () => {
            await api.setPlanner(planner)
            await load()
          }}
        >
          {t('apply_planner')}
        </button>
      </div>
      <table>
        <thead>
          <tr>
            <th>{t('col_relic')}</th>
            <th>{t('col_tier')}</th>
            <th>{t('col_owned')}</th>
            <th>{t('col_score')}</th>
            <th>{t('col_top_drop')}</th>
          </tr>
        </thead>
        <tbody>
          {relics.slice(0, 80).map((r) => (
            <tr key={r.name}>
              <td>
                {r.favorite ? '★ ' : ''}
                {r.name}
              </td>
              <td>{r.tier}</td>
              <td>{r.owned}</td>
              <td>{r.score?.toFixed?.(1) ?? r.score}</td>
              <td className="muted">{r.drops?.[0]?.item_name || '—'}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  )
}

function MarketTab({
  busy,
  run,
}: {
  busy: boolean
  run: (fn: () => Promise<unknown>) => Promise<void>
}) {
  const { t } = useI18n()
  const [email, setEmail] = useState('')
  const [password, setPassword] = useState('')
  const [jwt, setJwt] = useState('')
  const [orders, setOrders] = useState<any[]>([])
  const [suggestions, setSuggestions] = useState<any[]>([])

  async function load() {
    try {
      setOrders(await api.marketOrders())
    } catch {
      setOrders([])
    }
    try {
      setSuggestions(await api.marketSuggestions())
    } catch {
      setSuggestions([])
    }
  }

  useEffect(() => {
    load().catch(() => {})
  }, [])

  return (
    <div className="panel">
      <h3>{t('sign_in')}</h3>
      <div className="row">
        <input
          placeholder={t('email')}
          value={email}
          onChange={(e) => setEmail(e.target.value)}
        />
        <input
          placeholder={t('password')}
          type="password"
          value={password}
          onChange={(e) => setPassword(e.target.value)}
        />
        <button
          className="btn"
          disabled={busy}
          onClick={() =>
            run(async () => {
              await api.marketSignIn(email, password)
              await load()
            })
          }
        >
          {t('sign_in_btn')}
        </button>
      </div>
      <div className="row">
        <input
          style={{ flex: 1, minWidth: 220 }}
          placeholder={t('jwt_placeholder')}
          value={jwt}
          onChange={(e) => setJwt(e.target.value)}
        />
        <button
          className="btn secondary"
          disabled={busy}
          onClick={() =>
            run(async () => {
              await api.marketJwt(jwt)
              await load()
            })
          }
        >
          {t('save_jwt')}
        </button>
      </div>

      <h3>{t('your_orders')}</h3>
      <table>
        <thead>
          <tr>
            <th>{t('col_item')}</th>
            <th>{t('col_order_type')}</th>
            <th>{t('col_plat')}</th>
            <th>{t('col_qty')}</th>
          </tr>
        </thead>
        <tbody>
          {orders.map((o) => (
            <tr key={o.id}>
              <td>{o.item_url_name}</td>
              <td>{o.order_type}</td>
              <td>{o.platinum}</td>
              <td>{o.quantity}</td>
            </tr>
          ))}
        </tbody>
      </table>

      <h3>{t('listing_suggestions')}</h3>
      <table>
        <thead>
          <tr>
            <th>{t('col_item')}</th>
            <th>{t('col_qty')}</th>
            <th>{t('col_suggested')}</th>
            <th></th>
          </tr>
        </thead>
        <tbody>
          {suggestions.slice(0, 40).map((s, i) => (
            <tr key={i}>
              <td>{s.inventory_name}</td>
              <td>{s.count}</td>
              <td>{s.suggested_plat != null ? s.suggested_plat.toFixed(0) : '—'}</td>
              <td>
                {s.url_name && s.suggested_plat != null && (
                  <button
                    className="btn secondary"
                    onClick={() =>
                      run(() =>
                        api.createOrder({
                          item_url_name: s.url_name,
                          order_type: 'sell',
                          platinum: Math.max(1, Math.round(s.suggested_plat)),
                          quantity: 1,
                        }),
                      )
                    }
                  >
                    {t('list_btn')}
                  </button>
                )}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  )
}

function RivensTab() {
  const { t } = useI18n()
  const [text, setText] = useState('Soma Riven\n+110% Multishot\n+80% Critical Chance\n-40% Recoil')
  const [oldT, setOldT] = useState('Old\n+90% Multishot')
  const [newT, setNewT] = useState('New\n+120% Multishot')
  const [analysis, setAnalysis] = useState<any>(null)
  const [compare, setCompare] = useState<any>(null)

  return (
    <div className="panel">
      <h3>{t('analyze')}</h3>
      <textarea
        rows={5}
        style={{ width: '100%' }}
        value={text}
        onChange={(e) => setText(e.target.value)}
      />
      <div className="row">
        <button className="btn" onClick={async () => setAnalysis(await api.analyzeRiven(text))}>
          {t('analyze_btn')}
        </button>
      </div>
      {analysis && <pre className="log">{JSON.stringify(analysis, null, 2)}</pre>}

      <h3>{t('compare_rerolls')}</h3>
      <div className="grid">
        <textarea rows={4} value={oldT} onChange={(e) => setOldT(e.target.value)} />
        <textarea rows={4} value={newT} onChange={(e) => setNewT(e.target.value)} />
      </div>
      <div className="row">
        <button
          className="btn"
          onClick={async () => setCompare(await api.compareRivens(oldT, newT))}
        >
          {t('compare_btn')}
        </button>
      </div>
      {compare && <pre className="log">{JSON.stringify(compare, null, 2)}</pre>}
    </div>
  )
}

function StatsTab() {
  const { t, locale } = useI18n()
  const [data, setData] = useState<any>(null)
  useEffect(() => {
    api.stats().then(setData).catch(() => {})
  }, [])
  const loc = locale === 'ru' ? 'ru-RU' : 'en-US'
  return (
    <div className="panel">
      <div className="grid">
        <div className="stat">
          <div className="label">{t('credits')}</div>
          <div className="value">{data?.latest_credits?.toLocaleString?.(loc) ?? '—'}</div>
        </div>
        <div className="stat">
          <div className="label">{t('platinum')}</div>
          <div className="value">{data?.latest_platinum?.toLocaleString?.(loc) ?? '—'}</div>
        </div>
        <div className="stat">
          <div className="label">{t('trades_logged')}</div>
          <div className="value">{data?.trade_count ?? 0}</div>
        </div>
      </div>
      <h3>{t('history')}</h3>
      <pre className="log">{JSON.stringify(data?.points?.slice?.(0, 40) ?? [], null, 2)}</pre>
    </div>
  )
}

function AnalyticsTab() {
  const { t } = useI18n()
  const [data, setData] = useState<any>(null)
  const [langs, setLangs] = useState<Record<string, boolean>>({})
  useEffect(() => {
    api.analytics().then(setData).catch(() => {})
    api.languages().then(setLangs).catch(() => {})
  }, [])
  return (
    <div className="panel">
      <p className="muted">{data?.generated_note}</p>
      <h3>{t('top_market_cap')}</h3>
      <table>
        <thead>
          <tr>
            <th>{t('col_item')}</th>
            <th>{t('col_plat')}</th>
            <th>{t('col_vol')}</th>
            <th>{t('col_cap')}</th>
          </tr>
        </thead>
        <tbody>
          {(data?.top_market_cap || []).map((i: any) => (
            <tr key={i.url_name}>
              <td>{i.name}</td>
              <td>{i.platinum?.toFixed?.(0)}</td>
              <td>{i.volume}</td>
              <td>{i.market_cap?.toFixed?.(0)}</td>
            </tr>
          ))}
        </tbody>
      </table>
      <h3>{t('ocr_lang_matrix')}</h3>
      <div className="row">
        {Object.entries(langs).map(([k, v]) => (
          <span key={k} className={v ? 'ok' : 'muted'}>
            {k}:{v ? t('yes') : t('no')}
          </span>
        ))}
      </div>
    </div>
  )
}

function SettingsTab({
  status,
  busy,
  run,
  onSaved,
}: {
  status: DaemonStatus | null
  busy: boolean
  run: (fn: () => Promise<unknown>) => Promise<void>
  onSaved: () => void
}) {
  const { t, locale, setLocale } = useI18n()
  const [consent, setConsent] = useState(false)
  const [overlay, setOverlay] = useState(true)
  const [ocr, setOcr] = useState('eng')
  const [eelog, setEelog] = useState('')

  useEffect(() => {
    api.config().then((c: any) => {
      setConsent(!!c.inventory_consent)
      setOverlay(c.overlay_enabled !== false)
      setOcr(c.ocr_lang || 'eng')
      setEelog(c.eelog_path || '')
      if (c.ui_lang === 'ru' || c.ui_lang === 'en') {
        setLocale(c.ui_lang)
      }
    })
  }, [setLocale])

  return (
    <div className="panel">
      <div className="row">
        <label>
          {t('lang_label')}{' '}
          <select
            value={locale}
            onChange={(e) => setLocale(e.target.value as Locale)}
          >
            <option value="ru">Русский</option>
            <option value="en">English</option>
          </select>
        </label>
      </div>
      <label className="row">
        <input type="checkbox" checked={consent} onChange={(e) => setConsent(e.target.checked)} />
        {t('consent')}
      </label>
      <label className="row">
        <input type="checkbox" checked={overlay} onChange={(e) => setOverlay(e.target.checked)} />
        {t('overlay_enabled')}
      </label>
      <div className="row">
        <label>
          {t('ocr_lang')}{' '}
          <input value={ocr} onChange={(e) => setOcr(e.target.value)} style={{ width: 80 }} />
        </label>
      </div>
      <div className="row">
        <input
          style={{ flex: 1 }}
          value={eelog}
          onChange={(e) => setEelog(e.target.value)}
          placeholder={t('eelog_path')}
        />
      </div>
      <button
        className="btn"
        disabled={busy}
        onClick={() =>
          run(async () => {
            await api.patchConfig({
              inventory_consent: consent,
              overlay_enabled: overlay,
              ocr_lang: ocr,
              eelog_path: eelog,
              ui_lang: locale,
            })
            onSaved()
          })
        }
      >
        {t('save_settings')}
      </button>
      <pre className="log" style={{ marginTop: 16 }}>
        {JSON.stringify(status, null, 2)}
      </pre>
    </div>
  )
}
