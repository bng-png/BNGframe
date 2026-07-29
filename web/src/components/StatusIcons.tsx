import masteryIcon from '../assets/mastery-icon.png'
import absorbedIcon from '../assets/absorbed-icon.png'
import vaultedIcon from '../assets/vaulted-icon.png'
import unvaultedIcon from '../assets/unvaulted-icon.png'

/** Owned — green check. */
export function OwnedIcon({ title }: { title?: string }) {
  return (
    <span className="status-ico owned" title={title} aria-label={title}>
      <svg viewBox="0 0 24 24" width="24" height="24" aria-hidden>
        <path
          fill="currentColor"
          d="M9.2 16.6 4.8 12.2l1.4-1.4 3 3 8-8 1.4 1.4z"
        />
      </svg>
    </span>
  )
}

/** Mastered — custom MR badge, turquoise via CSS mask. */
export function MasteredIcon({ title }: { title?: string }) {
  return (
    <span className="status-ico mastered" title={title} aria-label={title}>
      <span
        className="status-ico-mask"
        style={{ WebkitMaskImage: `url(${masteryIcon})`, maskImage: `url(${masteryIcon})` }}
        aria-hidden
      />
    </span>
  )
}

/** Helminth subsumed — custom icon, red via CSS mask. */
export function AbsorbedIcon({ title }: { title?: string }) {
  return (
    <span className="status-ico absorbed" title={title} aria-label={title}>
      <span
        className="status-ico-mask"
        style={{ WebkitMaskImage: `url(${absorbedIcon})`, maskImage: `url(${absorbedIcon})` }}
        aria-hidden
      />
    </span>
  )
}

/** Prime Vault — closed chest. */
export function VaultedIcon({ title }: { title?: string }) {
  return (
    <span className="status-ico vaulted" title={title} aria-label={title}>
      <span
        className="status-ico-mask"
        style={{ WebkitMaskImage: `url(${vaultedIcon})`, maskImage: `url(${vaultedIcon})` }}
        aria-hidden
      />
    </span>
  )
}

/** Not in Prime Vault — open chest / unvaulted mark. */
export function UnvaultedIcon({ title }: { title?: string }) {
  return (
    <span className="status-ico unvaulted" title={title} aria-label={title}>
      <span
        className="status-ico-mask"
        style={{ WebkitMaskImage: `url(${unvaultedIcon})`, maskImage: `url(${unvaultedIcon})` }}
        aria-hidden
      />
    </span>
  )
}
