/* dashboard/src/App.tsx */
import type React from 'react'
import { useState, useEffect, useCallback } from 'react'
import { api, WalletRow, WalletDetail, Stats, KnownInsider, TradeRow } from './lib/api'
import {
  RadarChart, Radar, PolarGrid, PolarAngleAxis,
  BarChart, Bar, XAxis, YAxis, Tooltip, ResponsiveContainer, Cell,
} from 'recharts'

/* ── colour palette ─────────────────────────────────────────────────────── */
const C = {
  bg:      '#080b0f',
  surface: '#0d1117',
  border:  '#1c2333',
  muted:   '#30363d',
  dim:     '#6e7681',
  text:    '#e6edf3',
  accent:  '#f0883e',
  red:     '#ff4c4c',
  green:   '#3fb950',
  yellow:  '#d29922',
  blue:    '#58a6ff',
}

/* ── helpers ─────────────────────────────────────────────────────────────── */
function fmt(n: number, d = 2) { return n.toFixed(d) }
function fmtUSD(n: number) {
  if (n >= 1_000_000) return `$${(n / 1e6).toFixed(2)}M`
  if (n >= 1_000)     return `$${(n / 1e3).toFixed(1)}K`
  return `$${n.toFixed(0)}`
}
function short(addr: string) { return addr.slice(0, 6) + '…' + addr.slice(-4) }
function scoreColor(s: number) {
  if (s >= 0.75) return C.red
  if (s >= 0.5)  return C.yellow
  return C.green
}
function formatDate(iso: string | null) {
  if (!iso) return '—'
  return new Date(iso).toLocaleDateString('en-US', { month: 'short', day: 'numeric', year: '2-digit' })
}

/* ── ScoreBar ────────────────────────────────────────────────────────────── */
export function ScoreBar({ value }: { value: number }) {
  const pct = Math.min(value * 100, 100)
  return (
    <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
      <div style={{ flex: 1, height: 4, background: C.muted, borderRadius: 2, overflow: 'hidden' }}>
        <div style={{ width: `${pct}%`, height: '100%', background: scoreColor(value), borderRadius: 2, transition: 'width 0.4s ease' }} />
      </div>
      <span style={{ fontFamily: 'monospace', fontSize: 11, color: scoreColor(value), minWidth: 36, textAlign: 'right' }}>
        {fmt(value)}
      </span>
    </div>
  )
}

/* ── ThreatBadge ─────────────────────────────────────────────────────────── */
export function ThreatBadge({ score, flagged }: { score: number; flagged: boolean }) {
  const label = flagged ? 'FLAGGED' : score >= 0.5 ? 'SUSPECT' : 'CLEAN'
  const color = flagged ? C.red : score >= 0.5 ? C.yellow : C.green
  return (
    <span style={{
      fontSize: 9, fontFamily: 'monospace', fontWeight: 700, letterSpacing: 1.5,
      padding: '2px 6px', border: `1px solid ${color}`, borderRadius: 2,
      color, background: `${color}15`,
    }}>
      {label}
    </span>
  )
}

/* ── StatCard ────────────────────────────────────────────────────────────── */
function StatCard({ label, value, sub, color }: { label: string; value: string; sub?: string; color?: string }) {
  return (
    <div style={{ background: C.surface, border: `1px solid ${C.border}`, borderRadius: 6, padding: '16px 20px' }}>
      <div style={{ fontSize: 10, fontFamily: 'monospace', letterSpacing: 2, color: C.dim, marginBottom: 6 }}>
        {label}
      </div>
      <div style={{ fontSize: 26, fontWeight: 700, color: color ?? C.text, letterSpacing: -0.5 }}>{value}</div>
      {sub && <div style={{ fontSize: 11, color: C.dim, marginTop: 4 }}>{sub}</div>}
    </div>
  )
}

/* ── Overlay ─────────────────────────────────────────────────────────────── */
function Overlay({ children, onClose }: { children: React.ReactNode; onClose: () => void }) {
  return (
    <div
      style={{ position: 'fixed', inset: 0, background: 'rgba(0,0,0,0.75)', zIndex: 100, display: 'flex', alignItems: 'center', justifyContent: 'center' }}
      onClick={e => { if (e.target === e.currentTarget) onClose() }}
    >
      <div style={{ background: C.surface, border: `1px solid ${C.border}`, borderRadius: 8, padding: 28, width: 'min(760px, 95vw)', maxHeight: '85vh', overflowY: 'auto' }}>
        {children}
      </div>
    </div>
  )
}

/* ── WalletModal ─────────────────────────────────────────────────────────── */
function WalletModal({ address, onClose }: { address: string; onClose: () => void }) {
  const [wallet, setWallet] = useState<WalletDetail | null>(null)
  const [trades, setTrades] = useState<TradeRow[]>([])

  useEffect(() => {
    api.getWallet(address).then(setWallet).catch(console.error)
    api.getWalletTrades(address).then(setTrades).catch(console.error)
  }, [address])

  if (!wallet) {
    return (
      <Overlay onClose={onClose}>
        <div style={{ color: C.dim, fontSize: 13, textAlign: 'center', padding: 32 }}>Loading…</div>
      </Overlay>
    )
  }

  const radarData = [
    { factor: 'Entry\nTiming',   value: wallet.entry_timing_score   * 100 },
    { factor: 'Mkt\nConc',       value: wallet.concentration_score  * 100 },
    { factor: 'Size',            value: wallet.size_score           * 100 },
    { factor: 'Wallet\nAge',     value: wallet.wallet_age_score     * 100 },
    { factor: 'Win\nRate',       value: wallet.win_rate_score       * 100 },
  ]

  const factors = [
    { label: 'Entry Timing',         value: wallet.entry_timing_score },
    { label: 'Market Concentration', value: wallet.concentration_score },
    { label: 'Trade Size',           value: wallet.size_score },
    { label: 'Wallet Age',           value: wallet.wallet_age_score },
    { label: 'Win Rate',             value: wallet.win_rate_score },
  ]

  return (
    <Overlay onClose={onClose}>
      {/* header */}
      <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'flex-start', marginBottom: 24 }}>
        <div>
          <div style={{ display: 'flex', alignItems: 'center', gap: 10, marginBottom: 4 }}>
            <span style={{ fontFamily: 'monospace', fontSize: 13, color: C.text }}>{address}</span>
            <ThreatBadge score={wallet.score} flagged={wallet.flagged} />
            {wallet.known_label && (
              <span style={{ fontSize: 10, fontFamily: 'monospace', color: C.accent, background: `${C.accent}18`, border: `1px solid ${C.accent}40`, padding: '2px 6px', borderRadius: 2 }}>
                ★ {wallet.known_label}
              </span>
            )}
          </div>
          <div style={{ fontSize: 11, color: C.dim }}>
            {wallet.markets_traded} markets · {fmtUSD(wallet.total_volume_usdc)} volume
            {wallet.first_activity_ts && ` · first seen ${formatDate(wallet.first_activity_ts)}`}
          </div>
        </div>
        <button onClick={onClose} style={{ background: 'none', border: 'none', color: C.dim, fontSize: 18, cursor: 'pointer', padding: '0 4px' }}>✕</button>
      </div>

      {/* score + radar */}
      <div style={{ display: 'grid', gridTemplateColumns: '1fr 220px', gap: 24, marginBottom: 24 }}>
        <div>
          <div style={{ fontSize: 10, letterSpacing: 2, color: C.dim, fontFamily: 'monospace', marginBottom: 12 }}>
            SCORE BREAKDOWN
          </div>
          {factors.map(f => (
            <div key={f.label} style={{ marginBottom: 10 }}>
              <div style={{ fontSize: 11, color: C.dim, marginBottom: 4 }}>{f.label}</div>
              <ScoreBar value={f.value} />
            </div>
          ))}
          <div style={{ marginTop: 16, paddingTop: 16, borderTop: `1px solid ${C.border}` }}>
            <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center' }}>
              <span style={{ fontSize: 13, color: C.text, fontWeight: 600 }}>INSIDER SCORE</span>
              <span style={{ fontSize: 22, fontWeight: 700, color: scoreColor(wallet.score), fontFamily: 'monospace' }}>
                {fmt(wallet.score)}
              </span>
            </div>
          </div>
        </div>
        <div>
          <RadarChart outerRadius={80} width={220} height={220} data={radarData}>
            <PolarGrid stroke={C.muted} />
            <PolarAngleAxis dataKey="factor" tick={{ fill: C.dim, fontSize: 9 }} />
            <Radar dataKey="value" stroke={scoreColor(wallet.score)} fill={scoreColor(wallet.score)} fillOpacity={0.25} />
          </RadarChart>
        </div>
      </div>

      {/* trade history */}
      <div>
        <div style={{ fontSize: 10, letterSpacing: 2, color: C.dim, fontFamily: 'monospace', marginBottom: 10 }}>
          TRADE HISTORY ({trades.length})
        </div>
        <div style={{ maxHeight: 220, overflowY: 'auto' }}>
          <table style={{ width: '100%', borderCollapse: 'collapse', fontSize: 11 }}>
            <thead>
              <tr style={{ color: C.dim, textAlign: 'left' }}>
                {['Time', 'Market', 'Side', 'USDC', 'Price'].map(h => (
                  <th key={h} style={{ padding: '4px 8px', fontWeight: 400, fontFamily: 'monospace', fontSize: 10, letterSpacing: 1, borderBottom: `1px solid ${C.border}`, position: 'sticky', top: 0, background: C.surface }}>{h}</th>
                ))}
              </tr>
            </thead>
            <tbody>
              {trades.length === 0 && (
                <tr>
                  <td colSpan={5} style={{ padding: '16px 8px', color: C.dim, textAlign: 'center' }}>No trade data</td>
                </tr>
              )}
              {trades.map((t, i) => (
                <tr key={i} style={{ borderBottom: `1px solid ${C.border}20` }}>
                  <td style={{ padding: '5px 8px', color: C.dim, fontFamily: 'monospace', fontSize: 10 }}>
                    {formatDate(t.block_time)}
                  </td>
                  <td style={{ padding: '5px 8px', color: C.text, fontSize: 10, maxWidth: 180, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
                    {t.title ?? t.condition_id.slice(0, 16) + '…'}
                  </td>
                  <td style={{ padding: '5px 8px' }}>
                    <span style={{ color: t.side === 'BUY' ? C.green : C.red, fontFamily: 'monospace', fontSize: 10, fontWeight: 700 }}>{t.side}</span>
                  </td>
                  <td style={{ padding: '5px 8px', color: C.text, fontFamily: 'monospace' }}>
                    {fmtUSD(t.usdc_amount)}
                  </td>
                  <td style={{ padding: '5px 8px', color: C.dim, fontFamily: 'monospace' }}>
                    {fmt(t.price, 3)}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      </div>
    </Overlay>
  )
}

const PAGE_SIZE = 50

/* ── Pagination controls ─────────────────────────────────────────────────── */
export function Pagination({ page, total, pageSize, onChange }: { page: number; total: number; pageSize: number; onChange: (p: number) => void }) {
  const totalPages = Math.max(1, Math.ceil(total / pageSize))
  const pages: (number | '…')[] = []

  if (totalPages <= 7) {
    for (let i = 1; i <= totalPages; i++) pages.push(i)
  } else {
    pages.push(1)
    if (page > 3) pages.push('…')
    for (let i = Math.max(2, page - 1); i <= Math.min(totalPages - 1, page + 1); i++) pages.push(i)
    if (page < totalPages - 2) pages.push('…')
    pages.push(totalPages)
  }

  const btn = (label: string | number, target: number, disabled = false, active = false) => (
    <button
      key={`${label}-${target}`}
      onClick={() => !disabled && onChange(target)}
      disabled={disabled}
      style={{
        padding: '4px 10px', fontFamily: 'monospace', fontSize: 11,
        background: active ? C.accent : 'none',
        border: `1px solid ${active ? C.accent : C.border}`,
        borderRadius: 3, color: active ? '#000' : disabled ? C.muted : C.dim,
        cursor: disabled ? 'default' : 'pointer', transition: 'all 0.15s',
      }}
    >
      {label}
    </button>
  )

  return (
    <div style={{ display: 'flex', alignItems: 'center', gap: 4, padding: '12px 16px', borderTop: `1px solid ${C.border}`, justifyContent: 'space-between' }}>
      <span style={{ fontSize: 10, color: C.dim, fontFamily: 'monospace' }}>
        {total === 0 ? '0 results' : `${(page - 1) * pageSize + 1}–${Math.min(page * pageSize, total)} of ${total.toLocaleString()}`}
      </span>
      <div style={{ display: 'flex', gap: 4 }}>
        {btn('←', page - 1, page === 1)}
        {pages.map((p, i) =>
          p === '…'
            ? <span key={`ellipsis-${i}`} style={{ padding: '4px 6px', color: C.muted, fontSize: 11 }}>…</span>
            : btn(p, p as number, false, p === page)
        )}
        {btn('→', page + 1, page === totalPages)}
      </div>
    </div>
  )
}

/* ── Main App ────────────────────────────────────────────────────────────── */
export default function App() {
  const [stats, setStats]               = useState<Stats | null>(null)
  const [wallets, setWallets]           = useState<WalletRow[]>([])
  const [walletTotal, setWalletTotal]   = useState(0)
  const [known, setKnown]               = useState<KnownInsider[]>([])
  const [selected, setSelected]         = useState<string | null>(null)
  const [flaggedOnly, setFlaggedOnly]   = useState(false)
  const [minScore, setMinScore]         = useState(0)
  const [minVolume, setMinVolume]       = useState(1000)
  const [page, setPage]                 = useState(1)
  const [tab, setTab]                   = useState<'wallets' | 'known'>('wallets')
  const [loading, setLoading]           = useState(true)

  // Reset to page 1 when filters change
  const resetPage = useCallback((fn: () => void) => { setPage(1); fn() }, [])

  const load = useCallback(async () => {
    setLoading(true)
    try {
      const offset = (page - 1) * PAGE_SIZE
      const [s, w, k] = await Promise.all([
        api.getStats(),
        api.getWallets({ flagged_only: flaggedOnly, min_score: minScore || undefined, min_volume_usdc: minVolume, limit: PAGE_SIZE, offset }),
        api.getKnownInsiders(),
      ])
      setStats(s)
      setWallets(w)
      // Total count: use flagged_wallets when flagged_only, else total_wallets as upper bound.
      // The API doesn't return a filtered total, so we infer from whether we got a full page.
      setWalletTotal(w.length < PAGE_SIZE ? offset + w.length : offset + w.length + 1)
      setKnown(k)
    } catch (e) {
      console.error(e)
    } finally {
      setLoading(false)
    }
  }, [flaggedOnly, minScore, minVolume, page])

  useEffect(() => { load() }, [load])

  const barData = wallets.slice(0, 20).map(w => ({
    name:  short(w.address),
    score: parseFloat(fmt(w.score)),
    fill:  scoreColor(w.score),
  }))

  return (
    <div style={{ minHeight: '100vh', background: C.bg, color: C.text, fontFamily: '"IBM Plex Mono", "Fira Code", monospace' }}>

      {/* ── Nav ── */}
      <nav style={{ borderBottom: `1px solid ${C.border}`, padding: '0 32px', display: 'flex', alignItems: 'center', justifyContent: 'space-between', height: 52 }}>
        <div style={{ display: 'flex', alignItems: 'center', gap: 12 }}>
          <div style={{ width: 8, height: 8, borderRadius: '50%', background: C.red, boxShadow: `0 0 8px ${C.red}` }} />
          <span style={{ fontSize: 13, fontWeight: 700, letterSpacing: 2, color: C.text }}>
            POLYMARKET INSIDER DETECTION
          </span>
        </div>
        <div style={{ display: 'flex', alignItems: 'center', gap: 16 }}>
          <span style={{ fontSize: 10, color: C.dim, letterSpacing: 1 }}>POLYGON MAINNET</span>
          <button
            onClick={load}
            style={{ fontSize: 10, letterSpacing: 1.5, padding: '6px 14px', background: 'none', border: `1px solid ${C.border}`, borderRadius: 3, color: C.dim, cursor: 'pointer', fontFamily: 'inherit' }}
          >
            REFRESH
          </button>
        </div>
      </nav>

      <div style={{ padding: '28px 32px', maxWidth: 1400, margin: '0 auto' }}>

        {/* ── Stats row ── */}
        {stats && (
          <div style={{ display: 'grid', gridTemplateColumns: 'repeat(5, 1fr)', gap: 12, marginBottom: 28 }}>
            <StatCard label="TOTAL WALLETS"  value={stats.total_wallets.toLocaleString()} />
            <StatCard
              label="FLAGGED"
              value={stats.flagged_wallets.toLocaleString()}
              color={stats.flagged_wallets > 0 ? C.red : C.green}
              sub={`${((stats.flagged_wallets / Math.max(stats.total_wallets, 1)) * 100).toFixed(1)}% of wallets`}
            />
            <StatCard label="VOLUME INDEXED" value={fmtUSD(stats.total_volume_usdc)} />
            <StatCard
              label="KNOWN INSIDERS"
              value={`${stats.known_insiders_scored}/${stats.total_known_insiders}`}
              color={C.accent}
              sub="calibration set scored"
            />
            <StatCard
              label="DETECTION RATE"
              value={`${stats.total_known_insiders > 0 ? Math.round((stats.known_insiders_scored / stats.total_known_insiders) * 100) : 0}%`}
              color={C.yellow}
            />
          </div>
        )}

        {/* ── Score distribution bar chart ── */}
        {wallets.length > 0 && (
          <div style={{ background: C.surface, border: `1px solid ${C.border}`, borderRadius: 6, padding: '20px 20px 12px', marginBottom: 24 }}>
            <div style={{ fontSize: 10, letterSpacing: 2, color: C.dim, marginBottom: 16 }}>
              SCORE DISTRIBUTION — TOP 20 WALLETS
            </div>
            <ResponsiveContainer width="100%" height={120}>
              <BarChart data={barData} margin={{ top: 0, right: 0, bottom: 0, left: 0 }}>
                <XAxis dataKey="name" tick={{ fill: C.dim, fontSize: 9 }} axisLine={false} tickLine={false} />
                <YAxis domain={[0, 1]} tick={{ fill: C.dim, fontSize: 9 }} axisLine={false} tickLine={false} width={28} />
                <Tooltip
                  contentStyle={{ background: C.surface, border: `1px solid ${C.border}`, borderRadius: 4, fontSize: 11, fontFamily: 'monospace' }}
                  labelStyle={{ color: C.text }}
                  itemStyle={{ color: C.accent }}
                />
                <Bar dataKey="score" radius={[2, 2, 0, 0]}>
                  {barData.map((d, i) => <Cell key={i} fill={d.fill} />)}
                </Bar>
              </BarChart>
            </ResponsiveContainer>
          </div>
        )}

        {/* ── Tabs ── */}
        <div style={{ display: 'flex', gap: 0, marginBottom: 16, borderBottom: `1px solid ${C.border}` }}>
          {(['wallets', 'known'] as const).map(t => (
            <button
              key={t}
              onClick={() => setTab(t)}
              style={{ padding: '8px 20px', background: 'none', border: 'none', borderBottom: tab === t ? `2px solid ${C.accent}` : '2px solid transparent', color: tab === t ? C.text : C.dim, cursor: 'pointer', fontFamily: 'inherit', fontSize: 11, letterSpacing: 1.5, fontWeight: tab === t ? 700 : 400, marginBottom: -1, transition: 'color 0.2s' }}
            >
              {t === 'wallets' ? `ALL WALLETS (${walletTotal.toLocaleString()})` : `KNOWN INSIDERS (${known.length})`}
            </button>
          ))}

          {/* filters — only on wallets tab */}
          {tab === 'wallets' && (
            <div style={{ marginLeft: 'auto', display: 'flex', alignItems: 'center', gap: 16, paddingBottom: 4 }}>
              <label style={{ display: 'flex', alignItems: 'center', gap: 6, fontSize: 11, color: C.dim, cursor: 'pointer' }}>
                <input type="checkbox" checked={flaggedOnly} onChange={e => resetPage(() => setFlaggedOnly(e.target.checked))} style={{ accentColor: C.red }} />
                FLAGGED ONLY
              </label>
              <label style={{ display: 'flex', alignItems: 'center', gap: 6, fontSize: 11, color: C.dim }}>
                MIN SCORE
                <input type="range" min={0} max={1} step={0.05} value={minScore} onChange={e => resetPage(() => setMinScore(parseFloat(e.target.value)))} style={{ accentColor: C.accent, width: 80 }} />
                <span style={{ color: C.text, minWidth: 28 }}>{fmt(minScore)}</span>
              </label>
              <label style={{ display: 'flex', alignItems: 'center', gap: 6, fontSize: 11, color: C.dim }}>
                MIN VOL
                <select
                  value={minVolume}
                  onChange={e => resetPage(() => setMinVolume(Number(e.target.value)))}
                  style={{ background: C.surface, border: `1px solid ${C.border}`, color: C.text, borderRadius: 3, fontSize: 10, padding: '2px 6px', fontFamily: 'inherit', cursor: 'pointer' }}
                >
                  {[0, 100, 500, 1000, 4000, 10000, 50000].map(v => (
                    <option key={v} value={v}>{v === 0 ? 'ANY' : fmtUSD(v)}</option>
                  ))}
                </select>
              </label>
            </div>
          )}
        </div>

        {/* ── Wallet table ── */}
        {tab === 'wallets' && (
          <div style={{ background: C.surface, border: `1px solid ${C.border}`, borderRadius: 6, overflow: 'hidden' }}>
            {loading ? (
              <div style={{ padding: 32, textAlign: 'center', color: C.dim, fontSize: 12 }}>Loading…</div>
            ) : (
              <table style={{ width: '100%', borderCollapse: 'collapse', fontSize: 12 }}>
                <thead>
                  <tr style={{ background: C.bg }}>
                    {['Status', 'Address', 'Score', 'Entry', 'Mkt Conc', 'Size', 'Wal Age', 'Win Rate', 'Volume', 'Markets', 'First Seen'].map(h => (
                      <th key={h} style={{ padding: '10px 12px', textAlign: 'left', color: C.dim, fontSize: 9, letterSpacing: 1.5, fontWeight: 400, whiteSpace: 'nowrap', borderBottom: `1px solid ${C.border}` }}>{h}</th>
                    ))}
                  </tr>
                </thead>
                <tbody>
                  {wallets.map((w, i) => (
                    <tr
                      key={w.address}
                      onClick={() => setSelected(w.address)}
                      style={{ borderBottom: `1px solid ${C.border}20`, cursor: 'pointer', background: i % 2 === 0 ? 'transparent' : `${C.surface}80`, transition: 'background 0.15s' }}
                      onMouseEnter={e => (e.currentTarget.style.background = `${C.muted}30`)}
                      onMouseLeave={e => (e.currentTarget.style.background = i % 2 === 0 ? 'transparent' : `${C.surface}80`)}
                    >
                      <td style={{ padding: '9px 12px' }}>
                        <ThreatBadge score={w.score} flagged={w.flagged} />
                      </td>
                      <td style={{ padding: '9px 12px', fontFamily: 'monospace', fontSize: 11, color: C.blue }}>
                        {short(w.address)}
                      </td>
                      <td style={{ padding: '9px 12px', minWidth: 120 }}>
                        <ScoreBar value={w.score} />
                      </td>
                      {[w.entry_timing_score, w.concentration_score, w.size_score, w.wallet_age_score, w.win_rate_score].map((v, j) => (
                        <td key={j} style={{ padding: '9px 12px', fontFamily: 'monospace', fontSize: 11, color: scoreColor(v) }}>
                          {fmt(v)}
                        </td>
                      ))}
                      <td style={{ padding: '9px 12px', fontFamily: 'monospace', fontSize: 11 }}>
                        {fmtUSD(w.total_volume_usdc)}
                      </td>
                      <td style={{ padding: '9px 12px', fontFamily: 'monospace', fontSize: 11, color: C.dim }}>
                        {w.markets_traded}
                      </td>
                      <td style={{ padding: '9px 12px', fontFamily: 'monospace', fontSize: 11, color: C.dim }}>
                        {formatDate(w.first_activity_ts)}
                      </td>
                    </tr>
                  ))}
                  {wallets.length === 0 && (
                    <tr>
                      <td colSpan={11} style={{ padding: 32, textAlign: 'center', color: C.dim, fontSize: 12 }}>
                        No wallets found. Run the indexer then trigger scoring.
                      </td>
                    </tr>
                  )}
                </tbody>
              </table>
            )}
            <Pagination page={page} total={walletTotal} pageSize={PAGE_SIZE} onChange={setPage} />
          </div>
        )}

        {/* ── Known insiders tab ── */}
        {tab === 'known' && (
          <div style={{ background: C.surface, border: `1px solid ${C.border}`, borderRadius: 6, overflow: 'hidden' }}>
            <table style={{ width: '100%', borderCollapse: 'collapse', fontSize: 12 }}>
              <thead>
                <tr style={{ background: C.bg }}>
                  {['Status', 'Address', 'Label', 'Market', 'Score', 'Last Scored'].map(h => (
                    <th key={h} style={{ padding: '10px 12px', textAlign: 'left', color: C.dim, fontSize: 9, letterSpacing: 1.5, fontWeight: 400, borderBottom: `1px solid ${C.border}` }}>{h}</th>
                  ))}
                </tr>
              </thead>
              <tbody>
                {known.map((k, i) => (
                  <tr
                    key={k.address}
                    onClick={() => setSelected(k.address)}
                    style={{ borderBottom: `1px solid ${C.border}20`, cursor: 'pointer', background: i % 2 === 0 ? 'transparent' : `${C.surface}80` }}
                    onMouseEnter={e => (e.currentTarget.style.background = `${C.muted}30`)}
                    onMouseLeave={e => (e.currentTarget.style.background = i % 2 === 0 ? 'transparent' : `${C.surface}80`)}
                  >
                    <td style={{ padding: '10px 12px' }}>
                      <ThreatBadge score={k.score ?? 0} flagged={k.flagged ?? false} />
                    </td>
                    <td style={{ padding: '10px 12px', fontFamily: 'monospace', fontSize: 11, color: C.blue }}>
                      {short(k.address)}
                    </td>
                    <td style={{ padding: '10px 12px', color: C.accent, fontSize: 11 }}>{k.label}</td>
                    <td style={{ padding: '10px 12px', color: C.dim, fontSize: 11 }}>{k.market || '—'}</td>
                    <td style={{ padding: '10px 12px', fontFamily: 'monospace', color: scoreColor(k.score ?? 0), fontWeight: 700 }}>
                      {k.score != null ? fmt(k.score) : '—'}
                    </td>
                    <td style={{ padding: '10px 12px', fontFamily: 'monospace', fontSize: 11, color: C.dim }}>
                      {formatDate(k.scored_at)}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}

        {/* ── Footer ── */}
        <div style={{ marginTop: 32, paddingTop: 16, borderTop: `1px solid ${C.border}`, display: 'flex', justifyContent: 'space-between', fontSize: 10, color: C.dim }}>
          <span>INSIDER DETECTION v0.1 · POLYGON MAINNET</span>
          <span>CTF EXCHANGE + NEGRISK EXCHANGE · POLYMARKET DATA API</span>
        </div>
      </div>

      {/* ── Wallet detail modal ── */}
      {selected && (
        <WalletModal address={selected} onClose={() => setSelected(null)} />
      )}
    </div>
  )
}
