import type { InventoryItem, MarketOrder, MasterySet } from '../api'

export type Tri = boolean | null
export type PartKind = 'normal' | 'prime' | null
export type MinPlat = 5 | 10 | 15 | null

export type InventoryFilters = {
  mastered: Tri
  ownedGt1: Tri
  vaulted: Tri
  orderPlaced: Tri
  partKind: PartKind
  favorite: Tri
  minPlat: MinPlat
  setComplete: Tri
}

export const EMPTY_FILTERS: InventoryFilters = {
  mastered: null,
  ownedGt1: null,
  vaulted: null,
  orderPlaced: null,
  partKind: null,
  favorite: null,
  minPlat: null,
  setComplete: null,
}

export function isPrimeItem(item: Pick<InventoryItem, 'name' | 'unique_name'>): boolean {
  return /prime|прайм/i.test(item.name) || /Prime/i.test(item.unique_name)
}

export function filtersActive(f: InventoryFilters): boolean {
  return (
    f.mastered != null ||
    f.ownedGt1 != null ||
    f.vaulted != null ||
    f.orderPlaced != null ||
    f.partKind != null ||
    f.favorite != null ||
    f.minPlat != null ||
    f.setComplete != null
  )
}

export function isMasterySetComplete(set: MasterySet): boolean {
  const parts = set.parts || []
  if (!parts.length) return false
  return parts.every((p) => (p.count || 0) > 0)
}

/** url_name → whether its parent set has every part owned (count > 0). */
export function buildSetCompleteMap(sets: MasterySet[]): Map<string, boolean> {
  const map = new Map<string, boolean>()
  for (const s of sets) {
    const parts = s.parts || []
    if (!parts.length) continue
    const complete = parts.every((p) => (p.count || 0) > 0)
    if (s.url_name) map.set(s.url_name, complete)
    for (const p of parts) {
      if (p.url_name) map.set(p.url_name, complete)
    }
  }
  return map
}

/** Part / set url_name → parent MasterySet. */
export function buildUrlToSetMap(sets: MasterySet[]): Map<string, MasterySet> {
  const map = new Map<string, MasterySet>()
  for (const s of sets) {
    if (s.url_name) map.set(s.url_name, s)
    for (const p of s.parts || []) {
      if (p.url_name) map.set(p.url_name, s)
    }
  }
  return map
}

export function orderUrlSet(orders: MarketOrder[]): Set<string> {
  const s = new Set<string>()
  for (const o of orders) {
    if (o.item_url_name) s.add(o.item_url_name)
  }
  return s
}

export function matchInventoryFilters(
  item: InventoryItem,
  f: InventoryFilters,
  orderedUrls: Set<string>,
  setComplete: Map<string, boolean>,
): boolean {
  if (f.mastered != null && item.mastered !== f.mastered) return false
  if (f.ownedGt1 != null && item.count > 1 !== f.ownedGt1) return false
  if (f.vaulted != null && !!item.vaulted !== f.vaulted) return false
  if (f.favorite != null && !!item.favorite !== f.favorite) return false
  if (f.partKind === 'prime' && !isPrimeItem(item)) return false
  if (f.partKind === 'normal' && isPrimeItem(item)) return false
  if (f.minPlat != null && (item.platinum == null || item.platinum < f.minPlat)) return false

  if (f.orderPlaced != null) {
    const has = !!(item.url_name && orderedUrls.has(item.url_name))
    if (has !== f.orderPlaced) return false
  }

  if (f.setComplete != null) {
    if (!item.url_name || !setComplete.has(item.url_name)) return false
    if (setComplete.get(item.url_name) !== f.setComplete) return false
  }

  return true
}

/** Toggle Yes/No: same value again clears to «any». */
export function toggleTri(current: Tri, next: boolean): Tri {
  return current === next ? null : next
}

export function togglePartKind(current: PartKind, next: 'normal' | 'prime'): PartKind {
  return current === next ? null : next
}

export function toggleMinPlat(current: MinPlat, next: 5 | 10 | 15): MinPlat {
  return current === next ? null : next
}

/** Synthetic inventory row so RightRail can load WFM orders for a set/part. */
export function normalizeMarketSlug(slug: string): string {
  return slug.replace(/_helmet_blueprint$/i, '_neuroptics_blueprint')
}

export function masterySelectionToItem(opts: {
  urlName: string
  name: string
  platinum?: number | null
  thumb?: string | null
  count?: number
}): InventoryItem {
  const urlName = normalizeMarketSlug(opts.urlName)
  return {
    unique_name: urlName,
    name: opts.name,
    count: opts.count ?? 1,
    mastered: false,
    item_type: 'part',
    url_name: urlName,
    platinum: opts.platinum ?? null,
    thumb: opts.thumb ?? null,
    favorite: false,
  }
}
