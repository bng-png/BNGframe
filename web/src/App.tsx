import { useCallback, useEffect, useState } from 'react'
import {
  api,
  connectWs,
  type DaemonStatus,
  type InventoryItem,
  type PlayerProfile,
  type RewardSnapshot,
} from './api'
import { useI18n, I18nProvider } from './i18n'
import { LeftNav, type NavId } from './nav/LeftNav'
import { RightRail } from './rail/RightRail'
import { InventoryView } from './views/InventoryView'
import { MasteryView } from './views/MasteryView'
import { RelicsView } from './views/RelicsView'
import { RivensView } from './views/RivensView'
import { MarketView } from './views/MarketView'
import { AnalyticsView, SettingsView, StatsView } from './views/MiscViews'
import { DucatAmount, PlatAmount } from './components/Currency'

function AppInner() {
  const { t, locale, setLocale } = useI18n()
  const [tab, setTab] = useState<NavId>('inventory')
  const [status, setStatus] = useState<DaemonStatus | null>(null)
  const [items, setItems] = useState<InventoryItem[]>([])
  const [rewards, setRewards] = useState<RewardSnapshot[]>([])
  const [busy, setBusy] = useState(false)
  const [err, setErr] = useState('')
  const [selected, setSelected] = useState<InventoryItem | null>(null)
  const [orderType, setOrderType] = useState<'sell' | 'buy'>('sell')
  const [cacheAge, setCacheAge] = useState<string>('')
  const [profile, setProfile] = useState<PlayerProfile | null>(null)

  const run = useCallback(async (fn: () => Promise<unknown>) => {
    setBusy(true)
    setErr('')
    try {
      await fn()
    } catch (e: any) {
      setErr(e.message || String(e))
    } finally {
      setBusy(false)
    }
  }, [])

  const refreshStatus = useCallback(async () => {
    const s = await api.status()
    setStatus(s)
  }, [])

  const loadProfile = useCallback(async () => {
    try {
      setProfile(await api.profile())
    } catch {
      /* ignore — keep placeholder until inventory sync */
    }
  }, [])

  const loadInventory = useCallback(async () => {
    try {
      const meta = await api.inventoryCache()
      if (meta?.age_secs != null) {
        const a = meta.age_secs
        if (a < 60) setCacheAge(t('cache_just_now'))
        else if (a < 3600) setCacheAge(t('cache_minutes', { n: Math.floor(a / 60) }))
        else if (a < 86400) setCacheAge(t('cache_hours', { n: Math.floor(a / 3600) }))
        else setCacheAge(t('cache_days', { n: Math.floor(a / 86400) }))
      }
    } catch {
      /* ignore */
    }
    const data = await api.inventory()
    const list = Array.isArray(data) ? data : data.items || []
    setItems(list)
    loadProfile().catch(() => {})
  }, [t, loadProfile])

  useEffect(() => {
    refreshStatus()
    loadInventory().catch(() => {})
    loadProfile().catch(() => {})
    api.rewards().then(setRewards).catch(() => {})
    const ws = connectWs((ev) => {
      if (ev?.InventoryUpdated) loadInventory().catch(() => {})
      if (ev?.OverlayShown || ev?.type === 'OverlayShown') {
        api.rewards().then(setRewards).catch(() => {})
      }
    })
    const id = setInterval(() => {
      refreshStatus().catch(() => {})
      api.rewards().then(setRewards).catch(() => {})
    }, 10000)
    return () => {
      ws.close()
      clearInterval(id)
    }
  }, [refreshStatus, loadInventory, loadProfile])

  const pick = (item: InventoryItem, ot: 'sell' | 'buy') => {
    setSelected(item)
    setOrderType(ot)
  }

  const onItemPriced = useCallback((urlName: string, platinum: number) => {
    setItems((prev) =>
      prev.map((i) => (i.url_name === urlName ? { ...i, platinum } : i)),
    )
    setSelected((sel) =>
      sel?.url_name === urlName ? { ...sel, platinum } : sel,
    )
  }, [])

  return (
    <div className="shell">
      <LeftNav
        active={tab}
        onSelect={setTab}
        t={t}
        profileName={profile?.display_name || t('profile_placeholder')}
        mr={profile?.mastery_rank ?? null}
        avatarUrl={profile?.avatar_url}
      />
      <div className="main">
        <header className="topbar">
          <div className="brand">
            BNG<span>frame</span>
          </div>
          <span className={`badge${status?.watching_eelog ? ' ok' : ''}`}>
            EE.log {status?.eelog_exists ? t('found') : t('missing')}
          </span>
          <span className={`badge${status?.inventory_loaded ? ' ok' : ' warn'}`}>
            {t('cache_label')}: {cacheAge || (status?.inventory_loaded ? t('loaded') : t('cache_empty'))}
          </span>
          <div className="topbar-actions">
            <button
              className="btn ghost sm"
              type="button"
              disabled={busy}
              onClick={() => run(() => api.triggerReward())}
            >
              {t('trigger_ocr')}
            </button>
            <button
              className="btn ghost sm"
              type="button"
              disabled={busy}
              onClick={() => run(() => api.refreshItems().then(loadInventory))}
            >
              {t('refresh_items')}
            </button>
            <a className="btn ghost sm" href="/overlay" target="_blank" rel="noreferrer">
              {t('open_overlay')}
            </a>
          </div>
        </header>
        <div className="main-scroll">
          {err && <div className="err" style={{ marginBottom: 10 }}>{err}</div>}
          {rewards[0] && (
            <div className="reward-strip">
              {rewards[0].slots.map((s, i) => (
                <div
                  key={i}
                  className={`reward-card${rewards[0].best_index === i ? ' best' : ''}`}
                >
                  <div style={{ fontWeight: 650 }}>{s.name}</div>
                  <div className="muted" style={{ display: 'flex', gap: 8, alignItems: 'center' }}>
                    <PlatAmount value={s.platinum} />
                    <DucatAmount value={s.ducats} />
                  </div>
                </div>
              ))}
            </div>
          )}
          {tab === 'inventory' && (
            <InventoryView
              items={items}
              t={t}
              busy={busy}
              onSync={() => run(() => api.syncInventory().then(loadInventory))}
              onRefreshPrices={() => run(() => api.refreshPrices(100).then(loadInventory))}
              onItemPriced={onItemPriced}
              onWts={(i) => pick(i, 'sell')}
              onWtb={(i) => pick(i, 'buy')}
            />
          )}
          {tab === 'mastery' && <MasteryView t={t} />}
          {tab === 'relics' && <RelicsView t={t} />}
          {tab === 'rivens' && <RivensView t={t} />}
          {tab === 'market' && <MarketView t={t} />}
          {tab === 'analytics' && <AnalyticsView t={t} />}
          {tab === 'stats' && <StatsView t={t} />}
          {tab === 'settings' && (
            <SettingsView
              t={t}
              locale={locale}
              setLocale={setLocale}
              busy={busy}
              run={run}
              onSaved={refreshStatus}
            />
          )}
        </div>
      </div>
      <RightRail
        t={t}
        selectedItem={selected}
        defaultOrderType={orderType}
        onItemPriced={onItemPriced}
      />
    </div>
  )
}

export default function App() {
  return (
    <I18nProvider>
      <AppInner />
    </I18nProvider>
  )
}
