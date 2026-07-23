const BASE = ''

async function req<T>(path: string, init?: RequestInit): Promise<T> {
  const res = await fetch(`${BASE}${path}`, {
    headers: { 'Content-Type': 'application/json', ...(init?.headers || {}) },
    ...init,
  })
  const data = await res.json().catch(() => ({}))
  if (!res.ok) throw new Error(data.error || res.statusText)
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
  count: number
  mastered: boolean
  item_type: string
  url_name?: string | null
  platinum?: number | null
  ducats?: number | null
  favorite: boolean
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

export const api = {
  status: () => req<DaemonStatus>('/api/status'),
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
  marketOrders: () => req<any[]>('/api/market/orders'),
  marketSignIn: (email: string, password: string) =>
    req('/api/market/signin', { method: 'POST', body: JSON.stringify({ email, password }) }),
  marketJwt: (jwt: string) =>
    req('/api/market/jwt', { method: 'POST', body: JSON.stringify({ jwt }) }),
  marketSuggestions: () => req<any[]>('/api/market/suggestions'),
  createOrder: (body: any) =>
    req('/api/market/orders/create', { method: 'POST', body: JSON.stringify(body) }),
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
