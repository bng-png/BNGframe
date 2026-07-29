/** Warframe Wiki / Fandom image helpers.
 * Primary host: warframe.fandom.com FilePath → static.wikia CDN.
 * Fallback: wiki.warframe.com/images (same files, no CF challenge).
 */

const PART_SUFFIXES: { suffix: string; role: string; file: string }[] = [
  { suffix: '_neuroptics_blueprint', role: 'neuroptics', file: 'Helmet.png' },
  { suffix: '_chassis_blueprint', role: 'chassis', file: 'Chassis.png' },
  { suffix: '_systems_blueprint', role: 'systems', file: 'Systems.png' },
  { suffix: '_neuroptics', role: 'neuroptics', file: 'Helmet.png' },
  { suffix: '_chassis', role: 'chassis', file: 'Chassis.png' },
  { suffix: '_systems', role: 'systems', file: 'Systems.png' },
  { suffix: '_blueprint', role: 'blueprint', file: 'Blueprint.png' },
  { suffix: '_barrel', role: 'barrel', file: 'Barrel.png' },
  // WFM typo in some listings
  { suffix: '_reciever', role: 'receiver', file: 'Receiver.png' },
  { suffix: '_receiver', role: 'receiver', file: 'Receiver.png' },
  { suffix: '_stock', role: 'stock', file: 'Stock.png' },
  { suffix: '_blade', role: 'blade', file: 'Blade.png' },
  { suffix: '_handle', role: 'handle', file: 'Handle.png' },
  { suffix: '_link', role: 'link', file: 'Receiver.png' },
  { suffix: '_gauntlet', role: 'gauntlet', file: 'Blade.png' },
  { suffix: '_grip', role: 'grip', file: 'Handle.png' },
  { suffix: '_string', role: 'string', file: 'Stock.png' },
  { suffix: '_pouch', role: 'pouch', file: 'Receiver.png' },
  { suffix: '_chain', role: 'chain', file: 'Blade.png' },
  { suffix: '_hilt', role: 'hilt', file: 'Handle.png' },
  { suffix: '_guard', role: 'guard', file: 'Blade.png' },
  { suffix: '_head', role: 'head', file: 'Blade.png' },
  { suffix: '_lower_limb', role: 'lower_limb', file: 'Blade.png' },
  { suffix: '_upper_limb', role: 'upper_limb', file: 'Blade.png' },
  // Sentinel / companion parts
  { suffix: '_carapace', role: 'carapace', file: 'Chassis.png' },
  { suffix: '_cerebrum', role: 'cerebrum', file: 'Systems.png' },
]

const ROLE_FILE: Record<string, string> = {
  blueprint: 'Blueprint.png',
  neuroptics: 'Helmet.png',
  chassis: 'Chassis.png',
  systems: 'Systems.png',
  barrel: 'Barrel.png',
  receiver: 'Receiver.png',
  stock: 'Stock.png',
  blade: 'Blade.png',
  handle: 'Handle.png',
  link: 'Receiver.png',
  gauntlet: 'Blade.png',
  grip: 'Handle.png',
  string: 'Stock.png',
  pouch: 'Receiver.png',
  chain: 'Blade.png',
  hilt: 'Handle.png',
  guard: 'Blade.png',
  head: 'Blade.png',
  lower_limb: 'Blade.png',
  upper_limb: 'Blade.png',
  carapace: 'Chassis.png',
  cerebrum: 'Systems.png',
  other: 'Blueprint.png',
}

/** Detect part role from RU/EN display name (when slug is missing or misspelled). */
export function roleFromDisplayName(name?: string | null): string | null {
  if (!name) return null
  const n = name.toLowerCase()
  if (/нейро|neuroptic/.test(n)) return 'neuroptics'
  if (/каркас|chassis/.test(n)) return 'chassis'
  if (/систем|systems/.test(n)) return 'systems'
  if (/ствол|barrel/.test(n)) return 'barrel'
  if (/приёмник|приемник|receiver|reciever/.test(n)) return 'receiver'
  if (/приклад|stock/.test(n)) return 'stock'
  if (/клинок|blade/.test(n)) return 'blade'
  if (/рукоят|рукоять|handle/.test(n)) return 'handle'
  if (/связь|link/.test(n)) return 'link'
  if (/панцир|carapace/.test(n)) return 'carapace'
  if (/мозг|cerebrum/.test(n)) return 'cerebrum'
  if (/чертеж|blueprint/.test(n)) return 'blueprint'
  return null
}

/** snake_case / spaced name → Wiki CamelCase file base (no extension). */
export function toWikiCamel(name: string): string {
  return name
    .replace(/\.png$/i, '')
    .replace(/[_-]+/g, ' ')
    .split(/\s+/)
    .filter(Boolean)
    .map((w) => w.charAt(0).toUpperCase() + w.slice(1).toLowerCase())
    .join('')
    // Keep known all-caps tokens if whole word was upper? Prime stays Prime via capitalize.
    .replace(/Prime/g, 'Prime')
}

/** Candidate absolute URLs for a wiki file name like `AstillaPrime.png`. */
export function wikiFileUrls(fileName: string): string[] {
  const file = fileName.endsWith('.png') || fileName.endsWith('.PNG') ? fileName : `${fileName}.png`
  const enc = encodeURIComponent(file)
  return [
    `https://warframe.fandom.com/wiki/Special:FilePath/${enc}`,
    `https://wiki.warframe.com/images/${enc}`,
  ]
}

export function wikiUrlForName(displayOrSlug: string): string[] {
  const camel = toWikiCamel(displayOrSlug.replace(/:/g, ' '))
  if (!camel) return []
  return wikiFileUrls(`${camel}.png`)
}

export type PartVisual = {
  isPart: boolean
  role: string | null
  /** Parent set / weapon / frame wiki file */
  mainFile: string | null
  /** Part role wiki file (Chassis.png, Barrel.png, …) */
  partFile: string | null
  mainUrls: string[]
  partUrls: string[]
}

/** Derive main + part wiki images from a WFM slug or set_key. */
export function partVisualFromSlug(
  urlName?: string | null,
  roleHint?: string | null,
  displayName?: string | null,
): PartVisual {
  const slug = (urlName || '').toLowerCase().replace(/\.png$/, '')
  let base = slug.endsWith('_set') ? slug.slice(0, -4) : slug
  let role: string | null = roleHint || null
  let partFile: string | null = role ? ROLE_FILE[role] || null : null
  let matched = false

  if (base) {
    for (const p of PART_SUFFIXES) {
      if (base.endsWith(p.suffix)) {
        base = base.slice(0, -p.suffix.length)
        role = p.role
        partFile = p.file
        matched = true
        break
      }
    }
  }

  if (!matched) {
    const fromName = roleFromDisplayName(displayName) || roleHint
    if (fromName && ROLE_FILE[fromName]) {
      role = fromName
      partFile = ROLE_FILE[fromName]
      matched = true
      // Strip known part labels from EN/RU name to guess parent wiki file
      if (!base && displayName) {
        base = displayName
          .replace(/:.+$/, '')
          .replace(/\([^)]*\)/g, '')
          .replace(/\s+/g, '_')
          .toLowerCase()
          .replace(/[^a-z0-9_а-яё]+/gi, '_')
      }
    }
  }

  if (!matched && roleHint && ROLE_FILE[roleHint]) {
    partFile = ROLE_FILE[roleHint]
    matched = true
    role = roleHint
  }

  if (!base) {
    return { isPart: matched, role, mainFile: null, partFile, mainUrls: [], partUrls: partFile ? wikiFileUrls(partFile) : [] }
  }

  const mainFile = `${toWikiCamel(base)}.png`
  return {
    isPart: matched,
    role,
    mainFile,
    partFile,
    mainUrls: wikiFileUrls(mainFile),
    partUrls: partFile ? wikiFileUrls(partFile) : [],
  }
}

export function rolePartFile(role: string): string {
  return ROLE_FILE[role] || 'Blueprint.png'
}
