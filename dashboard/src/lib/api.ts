// dashboard/src/lib/api.ts
// Typed API client — field names match the Rust API response exactly.

const BASE = import.meta.env.VITE_API_BASE ?? ''

// ── Response types (mirror src/api/routes.rs) ──────────────────────────────

export interface WalletRow {
  address: string
  score: number
  entry_timing_score: number
  concentration_score: number
  size_score: number
  wallet_age_score: number
  win_rate_score: number
  total_volume_usdc: number
  markets_traded: number
  flagged: boolean
  first_activity_ts: string | null
  scored_at: string
}

export interface WalletDetail extends WalletRow {
  known_label: string | null
}

export interface TradeRow {
  condition_id: string
  title: string | null
  side: string
  price: number
  size: number
  usdc_amount: number
  block_time: string
}

export interface Stats {
  total_wallets: number
  flagged_wallets: number
  total_volume_usdc: number
  known_insiders_scored: number
  total_known_insiders: number
}

export interface KnownInsider {
  address: string
  label: string
  market: string
  source: string | null
  score: number | null
  flagged: boolean | null
  scored_at: string | null
}

// ── Query param types ──────────────────────────────────────────────────────

export interface WalletListParams {
  flagged_only?: boolean
  limit?: number
  offset?: number
  min_score?: number
  min_volume_usdc?: number
}

// ── Fetch helpers ──────────────────────────────────────────────────────────

async function get<T>(path: string, params?: Record<string, string | number | boolean>): Promise<T> {
  const url = new URL(`${BASE}${path}`, window.location.href)
  if (params) {
    for (const [k, v] of Object.entries(params)) {
      if (v !== undefined) url.searchParams.set(k, String(v))
    }
  }
  const res = await fetch(url.toString())
  if (!res.ok) throw new Error(`GET ${path} failed: ${res.status}`)
  return res.json()
}

async function post<T>(path: string): Promise<T> {
  const res = await fetch(`${BASE}${path}`, { method: 'POST' })
  if (!res.ok) throw new Error(`POST ${path} failed: ${res.status}`)
  return res.json()
}

// ── API client ─────────────────────────────────────────────────────────────

export const api = {
  getStats: () =>
    get<Stats>('/api/stats'),

  getWallets: (params?: WalletListParams) =>
    get<WalletRow[]>('/api/wallets', params as Record<string, string | number | boolean>),

  getWallet: (address: string) =>
    get<WalletDetail>(`/api/wallets/${address}`),

  getWalletTrades: (address: string) =>
    get<TradeRow[]>(`/api/wallets/${address}/trades`),

  getKnownInsiders: () =>
    get<KnownInsider[]>('/api/known-insiders'),

  triggerScoring: () =>
    post<{ status: string }>('/api/score'),
}
