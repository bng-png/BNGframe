import { useState } from 'react'
import { cachedImgUrl } from '../imgCache'

export type NavId =
  | 'inventory'
  | 'mastery'
  | 'relics'
  | 'rivens'
  | 'market'
  | 'analytics'
  | 'stats'
  | 'settings'

export type MarketPresence = 'invisible' | 'online' | 'ingame'

const NAV: { id: NavId; ico: string; labelKey: string }[] = [
  { id: 'mastery', ico: '◆', labelKey: 'nav_mastery' },
  { id: 'inventory', ico: '▣', labelKey: 'nav_inventory' },
  { id: 'relics', ico: '◇', labelKey: 'nav_relics' },
  { id: 'rivens', ico: '✶', labelKey: 'nav_rivens' },
  { id: 'market', ico: '⇄', labelKey: 'nav_market' },
  { id: 'analytics', ico: '▤', labelKey: 'nav_analytics' },
  { id: 'stats', ico: '◎', labelKey: 'nav_stats' },
  { id: 'settings', ico: '⚙', labelKey: 'nav_settings' },
]

function marketStatusLabel(t: (k: string) => string, status: MarketPresence) {
  if (status === 'online') return t('market_online')
  if (status === 'ingame') return t('market_ingame')
  return t('market_offline')
}

export function LeftNav({
  active,
  onSelect,
  t,
  profileName,
  mr,
  avatarUrl,
  marketAuth,
  marketStatus = 'invisible',
  onCycleMarketStatus,
  marketBusy,
}: {
  active: NavId
  onSelect: (id: NavId) => void
  t: (k: string) => string
  profileName: string
  mr?: number | null
  avatarUrl?: string | null
  marketAuth?: boolean
  marketStatus?: MarketPresence
  onCycleMarketStatus?: () => void
  marketBusy?: boolean
}) {
  const [imgFailed, setImgFailed] = useState(false)
  const cachedAvatar = cachedImgUrl(avatarUrl)
  const showImg = !!cachedAvatar && !imgFailed
  const statusLabel = marketStatusLabel(t, marketStatus)

  return (
    <aside className="sidebar">
      <div className="sidebar-profile">
        <div className={`avatar${showImg ? ' has-img' : ''}`}>
          {showImg ? (
            <img
              key={cachedAvatar!}
              src={cachedAvatar!}
              alt=""
              referrerPolicy="no-referrer"
              onError={() => setImgFailed(true)}
              onLoad={() => setImgFailed(false)}
            />
          ) : (
            <span>{(mr ?? 0) || '?'}</span>
          )}
        </div>
        <div className="profile-meta">
          <div className="name-row">
            <div className="name">{profileName}</div>
            {marketAuth ? (
              <button
                type="button"
                className={`market-online-toggle ${marketStatus}`}
                title={`${statusLabel} — ${t('market_status_cycle')}`}
                aria-label={`${statusLabel}. ${t('market_status_cycle')}`}
                disabled={marketBusy || !onCycleMarketStatus}
                onClick={() => onCycleMarketStatus?.()}
              >
                <span className="dot" />
              </button>
            ) : null}
          </div>
          <div className="mr">MR {mr ?? '—'}</div>
        </div>
      </div>
      {NAV.map((n) => (
        <button
          key={n.id}
          type="button"
          className={`nav-btn${active === n.id ? ' active' : ''}`}
          onClick={() => onSelect(n.id)}
        >
          <span className="nav-ico">{n.ico}</span>
          <span className="label">{t(n.labelKey)}</span>
        </button>
      ))}
      <div className="sidebar-spacer" />
    </aside>
  )
}
