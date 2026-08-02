/** Minimal fields for category helpers (`url_name` optional, same as InventoryItem). */
export type ItemClassInput = {
  item_type: string
  unique_name: string
  name: string
  url_name?: string | null
}

/** Arcanes / «мистики» (CosmeticEnhancers). */
export function isArcaneItem(item: ItemClassInput): boolean {
  if (item.item_type === 'arcane') return true
  return /\/cosmeticenhancers\//i.test(item.unique_name || '')
}

/** True for mods / upgrades (not arcanes, not prime parts). */
export function isModItem(item: ItemClassInput): boolean {
  if (isArcaneItem(item)) return false
  if (item.item_type === 'mod') return true
  const u = item.unique_name || ''
  return (
    /#RawUpgrades$/i.test(u) ||
    /\/upgrades\//i.test(u) ||
    /meleetrees/i.test(u) ||
    /\/sentinelprecepts\//i.test(u) ||
    /\/moaprecepts\//i.test(u) ||
    /\/kubrowpetprecepts\//i.test(u) ||
    /\/catbrowpetprecepts\//i.test(u)
  )
}

export function isRelicItem(item: ItemClassInput): boolean {
  if (item.item_type === 'relic') return true
  const u = item.unique_name || ''
  return /voidprojection/i.test(u) || /\/relics\//i.test(u) || /реликви/i.test(item.name || '')
}

const SET_PART_URL =
  /_(neuroptics|helmet|chassis|systems|blueprint|barrel|receiver|stock|blade|handle|link|grip|string|gauntlet|hilt|guard|carapace|cerebrum|lower_limb|upper_limb|pouch|stars)$/i

/** Set components (prime / wraith / etc. parts), not full weapon rows. */
export function isSetPartItem(item: ItemClassInput): boolean {
  if (isModItem(item) || isArcaneItem(item) || isRelicItem(item)) return false
  if (item.item_type === 'part') return true
  const url = item.url_name || ''
  if (SET_PART_URL.test(url)) return true
  if (/weaponparts/i.test(item.unique_name || '')) return true
  // DE stores neuroptics as *Helmet* under WarframeRecipes — still a set part.
  const u = item.unique_name || ''
  if (
    /WarframeRecipes/i.test(u) &&
    /(Helmet|Chassis|Systems)(Blueprint|Component)?/i.test(u) &&
    !/KeyBlueprint/i.test(u)
  ) {
    return true
  }
  if (
    /нейрооптик|каркас \(чертеж\)|систем \(чертеж\)|:\s*каркас|:\s*систем/i.test(item.name || '') ||
    (/\(чертеж\)/i.test(item.name || '') && /нейро|каркас|систем/i.test(item.name || ''))
  ) {
    return true
  }
  // WFM full-set listing (rare in inventory)
  if (/_set$/i.test(url) && !SET_PART_URL.test(url.replace(/_set$/i, ''))) {
    const stem = url.replace(/_set$/i, '')
    if (!SET_PART_URL.test(`_${stem.split('_').pop()}`)) return true
  }
  return false
}

/** Rows that should appear in the inventory list (incl. parts without WFM url yet). */
export function isInventoryListedItem(item: ItemClassInput): boolean {
  if (item.url_name) return true
  if (isRelicItem(item) || isArcaneItem(item)) return true
  if (isSetPartItem(item)) return true
  if (item.item_type === 'blueprint' || item.item_type === 'part') return true
  return false
}

function isBlueprintItem(item: ItemClassInput): boolean {
  if (item.item_type === 'blueprint') return true
  const u = item.unique_name || ''
  return /#recipes$/i.test(u) || /blueprint/i.test(u) || /чертеж/i.test(item.name || '')
}

/** Everything tradeable that is not relic / mod / arcane / set part. */
export function isMiscCategoryItem(item: ItemClassInput): boolean {
  if (isRelicItem(item) || isModItem(item) || isArcaneItem(item) || isSetPartItem(item)) {
    return false
  }
  return true
}

/**
 * Ground weapon slot for inventory filters.
 * Excludes mods (incl. stances), blueprints, archwing, operator amps.
 */
export function weaponSlot(item: ItemClassInput): 'primary' | 'secondary' | 'melee' | null {
  if (isModItem(item) || isArcaneItem(item) || isBlueprintItem(item)) return null

  const u = (item.unique_name || '').toLowerCase()
  if (u.includes('operatoramplifiers')) return null
  if (u.includes('/archwing/')) return null // archguns / archmelee — not these tabs

  const t = item.item_type
  // Only real gear rows — never infer from /weapons/ alone (stance mods share that path)
  if (t !== 'weapon' && t !== 'primary' && t !== 'secondary' && t !== 'melee') {
    return null
  }

  if (t === 'primary' || t === 'secondary' || t === 'melee') return t

  if (/\/melee\/|meleeweapon|mk1(?:bo|furax)|skana/.test(u)) return 'melee'
  if (
    /\/pistols?\/|\/pistol\/|throwingweapons|\/akimbo\/|\/secondaries\/|\/secondary\/|grineerpistol|mk1(?:furis|kunai)|thanopistol|operator\/pistols/.test(
      u,
    )
  ) {
    return 'secondary'
  }
  if (
    /\/longguns\/|\/rifle\/|\/bows?\/|\/shotgun|\/sniper|speargun|launcher|\/spears\/|heavyweapons|mk1(?:paris|strun)|thanorifle|flamethrower|grimoire|sentrifle/.test(
      u,
    )
  ) {
    return 'primary'
  }

  const n = (item.name || '').toLowerCase()
  if (/pistol|нукор|plinx|kunai|furis/.test(n)) return 'secondary'
  if (/skana|furax|\bbo\b/.test(n)) return 'melee'
  if (/rifle|shotgun|винтов|дробов|лук|bow|grattler|railgun|machine.?gun|long.?gun|grimoire/.test(n)) {
    return 'primary'
  }
  return null
}
