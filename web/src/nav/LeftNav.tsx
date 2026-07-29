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

export function LeftNav({
  active,
  onSelect,
  t,
  profileName,
  mr,
  avatarUrl,
}: {
  active: NavId
  onSelect: (id: NavId) => void
  t: (k: string) => string
  profileName: string
  mr?: number | null
  avatarUrl?: string | null
}) {
  const [imgFailed, setImgFailed] = useState(false)
  const cachedAvatar = cachedImgUrl(avatarUrl)
  const showImg = !!cachedAvatar && !imgFailed

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
          <div className="name">{profileName}</div>
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
