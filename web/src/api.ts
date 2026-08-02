const BASE = ''

async function req<T>(path: string, init?: RequestInit): Promise<T> {
  const res = await fetch(`${BASE}${path}`, {
    headers: { 'Content-Type': 'application/json', ...(init?.headers || {}) },
    ...init,
  })
  const data = await res.json().catch(() => ({}))
  if (!res.ok) throw new Error((data as any).error || res.statusText)
  return data as T
}

export type DaemonStatus = {
  watching_eelog: boolean
  eelog_path: string
  eelog_exists: boolean
  overlay_enabled: boolean
  inventory_loaded: boolean
  inventory_consent: boolean
  items_cached: number
  last_error?: string | null
  uptime_secs: number
  phase: string
}

export type RewardSlot = {
  name: string
  matched_url_name?: string | null
  platinum?: number | null
  volume?: number | null
  ducats?: number | null
  rank?: number | null
}

export type RewardSnapshot = {
  id: string
  detected_at: string
  slots: RewardSlot[]
  best_index?: number | null
  source: string
}

export type InventoryItem = {
  unique_name: string
  name: string
  /** English name shown under localized title when different. */
  name_en?: string | null
  count: number
  mastered: boolean
  item_type: string
  url_name?: string | null
  platinum?: number | null
  ducats?: number | null
  favorite: boolean
  thumb?: string | null
  vaulted?: boolean | null
  /** Mod / arcane rank (max owned). */
  rank?: number | null
}

export type InventoryCacheMeta = {
  synced_at?: string | null
  method: string
  item_count: number
  account_id?: string | null
  age_secs?: number | null
  cached: boolean
}

export type InventoryListResponse = {
  items: InventoryItem[]
  cache: InventoryCacheMeta
}

export type CycleInfo = {
  id: string
  state: string
  time_left?: string | null
  expiry?: string | null
}

export type FissureInfo = {
  id: string
  node: string
  mission_type: string
  tier: string
  enemy?: string | null
  expiry?: string | null
  eta?: string | null
  is_hard: boolean
  is_storm: boolean
}

export type WorldStateSnapshot = {
  earth?: CycleInfo | null
  cetus?: CycleInfo | null
  vallis?: CycleInfo | null
  cambion?: CycleInfo | null
  zariman?: CycleInfo | null
  void_trader?: {
    character: string
    location: string
    active: boolean
    expiry?: string | null
    activation?: string | null
  } | null
  fissures: FissureInfo[]
  events: string[]
  fetched_at: string
}

export type SetPartProgress = {
  role: string
  count: number
  /** Needed to craft (e.g. 2× Bronco for Akbronco). */
  required?: number
  url_name?: string | null
  name?: string | null
  /** WFM relative thumb path. */
  thumb?: string | null
}

export type MasterySet = {
  set_key: string
  name: string
  name_ru?: string | null
  category: string
  thumb?: string | null
  thumb_url?: string | null
  vaulted: boolean
  owned: boolean
  mastered: boolean
  /** Helminth subsumed this warframe. */
  absorbed?: boolean
  parts: SetPartProgress[]
  platinum?: number | null
  ducats?: number | null
  url_name?: string | null
}

export function wfmThumbUrl(thumb?: string | null): string | null {
  if (!thumb) return null
  if (thumb.startsWith('http://') || thumb.startsWith('https://')) return thumb
  return `https://warframe.market/static/assets/${thumb}`
}

export type MarketOrder = {
  id: string
  item_url_name: string
  order_type: string
  platinum: number
  quantity: number
  visible: boolean
  user?: string | null
  status?: string | null
  /** Mod / arcane rank (0–5 for mystics). */
  rank?: number | null
  item_name?: string | null
  /** English catalog name (secondary label). */
  item_name_en?: string | null
  thumb?: string | null
  /** Relic quality: intact / exceptional / flawless / radiant. */
  subtype?: string | null
}

export type PlayerProfile = {
  display_name?: string | null
  mastery_rank?: number | null
  account_id?: string | null
  avatar_url?: string | null
}

export const api = {
  status: () => req<DaemonStatus>('/api/status'),
  profile: () => req<PlayerProfile>('/api/profile'),
  config: () => req<Record<string, unknown>>('/api/config'),
  patchConfig: (body: Record<string, unknown>) =>
    req('/api/config', { method: 'POST', body: JSON.stringify(body) }),
  rewards: () => req<RewardSnapshot[]>('/api/rewards'),
  triggerReward: () => req<RewardSnapshot>('/api/rewards/trigger', { method: 'POST' }),
  inventory: () => req<InventoryListResponse | InventoryItem[]>('/api/inventory'),
  inventoryCache: () => req<InventoryCacheMeta>('/api/inventory/cache'),
  syncInventory: () => req('/api/inventory/sync', { method: 'POST' }),
  importInventory: (path?: string) =>
    req('/api/inventory/import', { method: 'POST', body: JSON.stringify({ path }) }),
  favorite: (id: string, favorite: boolean) =>
    req(`/api/inventory/favorite/${encodeURIComponent(id)}`, {
      method: 'POST',
      body: JSON.stringify({ favorite }),
    }),
  relics: () => req<any[]>('/api/relics'),
  planner: () => req<any>('/api/relics/planner'),
  setPlanner: (body: any) =>
    req('/api/relics/planner', { method: 'POST', body: JSON.stringify(body) }),
  marketOrders: (opts?: { refresh?: boolean }) =>
    req<MarketOrder[]>(
      opts?.refresh ? '/api/market/orders?refresh=true' : '/api/market/orders',
    ),
  marketItemOrders: (urlName: string) =>
    req<MarketOrder[]>(`/api/market/orders/item/${encodeURIComponent(urlName)}`),
  marketSignIn: (email: string, password: string) =>
    req('/api/market/signin', { method: 'POST', body: JSON.stringify({ email, password }) }),
  marketJwt: (jwt: string) =>
    req('/api/market/jwt', { method: 'POST', body: JSON.stringify({ jwt }) }),
  marketAuth: () =>
    req<{ authenticated: boolean; status?: string | null }>('/api/market/auth'),
  marketSetStatus: (status: 'online' | 'ingame' | 'invisible') =>
    req<{ authenticated: boolean; status?: string | null }>('/api/market/status', {
      method: 'POST',
      body: JSON.stringify({ status }),
    }),
  marketSuggestions: () => req<any[]>('/api/market/suggestions'),
  createOrder: (body: any) =>
    req('/api/market/orders/create', { method: 'POST', body: JSON.stringify(body) }),
  deleteOrder: (orderId: string) =>
    req(`/api/market/orders/${encodeURIComponent(orderId)}`, { method: 'DELETE' }),
  closeOrder: (orderId: string, quantity?: number) =>
    req(`/api/market/orders/${encodeURIComponent(orderId)}/close`, {
      method: 'POST',
      body: JSON.stringify({ quantity: quantity ?? null }),
    }),
  analyzeRiven: (text: string) =>
    req('/api/rivens/analyze', { method: 'POST', body: JSON.stringify({ text }) }),
  compareRivens: (old_text: string, new_text: string) =>
    req('/api/rivens/compare', { method: 'POST', body: JSON.stringify({ old_text, new_text }) }),
  stats: () => req<any>('/api/stats'),
  analytics: () => req<any>('/api/analytics'),
  languages: () => req<Record<string, boolean>>('/api/languages'),
  refreshItems: () => req('/api/items/refresh', { method: 'POST' }),
  refreshPrices: (limit = 80) =>
    req<{ matched: number; priced: number; failed: number; catalog_size: number }>(
      `/api/prices/refresh?limit=${limit}`,
      { method: 'POST' },
    ),
  priceItem: (urlName: string) =>
    req<{ url_name: string; platinum: number; volume: number; updated_at: string }>(
      `/api/prices/item/${encodeURIComponent(urlName)}`,
    ),
  worldstate: (opts?: { refresh?: boolean }) =>
    req<WorldStateSnapshot>(
      opts?.refresh ? '/api/worldstate?refresh=true' : '/api/worldstate',
    ),
  masterySets: () => req<{ sets: MasterySet[] }>('/api/mastery/sets'),
}

/** Fired after create / close / delete so MarketView can soft-refresh without a manual button. */
export const MARKET_ORDERS_CHANGED = 'bngframe:market-orders-changed'
const MY_ORDERS_LS_KEY = 'bngframe.my_orders.v1'

export function notifyMarketOrdersChanged() {
  if (typeof window === 'undefined') return
  window.dispatchEvent(new Event(MARKET_ORDERS_CHANGED))
}

export function readCachedMyOrders(): MarketOrder[] | null {
  try {
    const raw = sessionStorage.getItem(MY_ORDERS_LS_KEY)
    if (!raw) return null
    const parsed = JSON.parse(raw)
    return Array.isArray(parsed) ? (parsed as MarketOrder[]) : null
  } catch {
    return null
  }
}

export function writeCachedMyOrders(orders: MarketOrder[]) {
  try {
    sessionStorage.setItem(MY_ORDERS_LS_KEY, JSON.stringify(orders))
  } catch {
    /* quota / private mode */
  }
}

export function connectWs(onEvent: (ev: any) => void) {
  const proto = location.protocol === 'https:' ? 'wss' : 'ws'
  const ws = new WebSocket(`${proto}://${location.host}/api/ws`)
  ws.onmessage = (m) => {
    try {
      onEvent(JSON.parse(m.data))
    } catch {
      /* ignore */
    }
  }
  return ws
}
