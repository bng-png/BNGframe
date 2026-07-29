import { useEffect, useState } from 'react'
import { api } from '../api'

export function RelicsView({ t }: { t: (k: string) => string }) {
  const [relics, setRelics] = useState<any[]>([])
  const [mode, setMode] = useState('ducats_profit')
  const [err, setErr] = useState('')

  const load = () => {
    api
      .relics()
      .then(setRelics)
      .catch((e) => setErr(e.message || String(e)))
    api
      .planner()
      .then((p) => {
        if (p?.order_mode) setMode(p.order_mode)
      })
      .catch(() => {})
  }

  useEffect(() => {
    load()
  }, [])

  const apply = async () => {
    await api.setPlanner({ order_mode: mode, filters: {} })
    load()
  }

  const sorted = [...relics].sort((a, b) => (b.score || 0) - (a.score || 0))

  return (
    <>
      <div className="toolbar">
        <select className="select" value={mode} onChange={(e) => setMode(e.target.value)}>
          <option value="ducats_profit">{t('order_ducats')}</option>
          <option value="platinum">{t('order_platinum')}</option>
          <option value="best_for_mr">{t('order_mr')}</option>
        </select>
        <button className="btn sm" type="button" onClick={apply}>
          {t('apply_planner')}
        </button>
      </div>
      {err && <div className="err">{err}</div>}
      <div className="item-list">
        {sorted.slice(0, 80).map((r) => (
          <article key={r.name} className="item-card">
            <div className="item-thumb placeholder">{r.tier || '?'}</div>
            <div className="item-body">
              <div className="item-title">{r.name}</div>
              <div className="item-meta">
                <span className="tag">{r.tier}</span>
                <span>
                  {t('col_owned')}: {r.owned ? t('yes') : t('no')}
                </span>
                <span>
                  {t('col_score')}: {(r.score || 0).toFixed?.(1) ?? r.score}
                </span>
                <span className="muted">{r.drops?.[0]?.item_name || '—'}</span>
              </div>
            </div>
          </article>
        ))}
      </div>
    </>
  )
}
