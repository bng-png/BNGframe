import { useEffect, useState } from 'react'
import { api } from '../api'

export function StatsView({ t }: { t: (k: string) => string }) {
  const [data, setData] = useState<any>(null)
  useEffect(() => {
    api.stats().then(setData).catch(() => {})
  }, [])
  return (
    <div className="panel">
      <div className="row">
        <div className="badge ok">
          {t('col_plat')}: {data?.latest_platinum ?? '—'}
        </div>
        <div className="badge">
          Credits: {data?.latest_credits ?? '—'}
        </div>
        <div className="badge">
          Trades: {data?.trade_count ?? '—'}
        </div>
      </div>
      <pre className="log" style={{ marginTop: 12 }}>
        {JSON.stringify(data, null, 2)}
      </pre>
    </div>
  )
}

export function AnalyticsView({ t }: { t: (k: string) => string }) {
  const [data, setData] = useState<any>(null)
  const [langs, setLangs] = useState<Record<string, boolean>>({})
  useEffect(() => {
    api.analytics().then(setData).catch(() => {})
    api.languages().then(setLangs).catch(() => {})
  }, [])
  return (
    <>
      <div className="panel">
        <h3 style={{ marginTop: 0 }}>{t('tab_analytics')}</h3>
        <p className="muted">{data?.generated_note}</p>
        <table className="data">
          <thead>
            <tr>
              <th>{t('col_name')}</th>
              <th>{t('col_plat')}</th>
              <th>Vol</th>
            </tr>
          </thead>
          <tbody>
            {(data?.top_market_cap || []).slice(0, 20).map((i: any) => (
              <tr key={i.url_name}>
                <td>{i.name}</td>
                <td>{i.platinum?.toFixed?.(0)}</td>
                <td>{i.volume}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
      <div className="panel">
        <h3>{t('ocr_lang_matrix')}</h3>
        <div className="row">
          {Object.entries(langs).map(([k, v]) => (
            <span key={k} className={v ? 'ok' : 'muted'}>
              {k}:{v ? t('yes') : t('no')}
            </span>
          ))}
        </div>
      </div>
    </>
  )
}

export function SettingsView({
  t,
  locale,
  setLocale,
  busy,
  run,
  onSaved,
}: {
  t: (k: string) => string
  locale: 'ru' | 'en'
  setLocale: (l: 'ru' | 'en') => void
  busy: boolean
  run: (fn: () => Promise<unknown>) => Promise<void>
  onSaved: () => void
}) {
  const [consent, setConsent] = useState(false)
  const [overlay, setOverlay] = useState(true)
  const [focusOverlayWs, setFocusOverlayWs] = useState(true)
  const [autoOpenBrowser, setAutoOpenBrowser] = useState(false)
  const [ocr, setOcr] = useState('rus+eng')
  const [eelog, setEelog] = useState('')

  useEffect(() => {
    api.config().then((c: any) => {
      setConsent(!!c.inventory_consent)
      setOverlay(c.overlay_enabled !== false)
      setFocusOverlayWs(!!c.focus_overlay_workspace)
      setAutoOpenBrowser(!!c.auto_open_browser)
      const lang = c.ocr_lang || 'rus+eng'
      setOcr(lang === 'eng' && (c.ui_lang || 'ru') === 'ru' ? 'rus+eng' : lang)
      setEelog(c.eelog_path || '')
      if (c.ui_lang === 'ru' || c.ui_lang === 'en') setLocale(c.ui_lang)
    })
  }, [setLocale])

  return (
    <div className="panel">
      <div className="row">
        <label>
          {t('lang_label')}{' '}
          <select className="select" value={locale} onChange={(e) => setLocale(e.target.value as any)}>
            <option value="ru">Русский</option>
            <option value="en">English</option>
          </select>
        </label>
      </div>
      <label className="row">
        <input type="checkbox" checked={consent} onChange={(e) => setConsent(e.target.checked)} />
        {t('consent')}
      </label>
      <label className="row">
        <input type="checkbox" checked={overlay} onChange={(e) => setOverlay(e.target.checked)} />
        {t('overlay_enabled')}
      </label>
      <label className="row">
        <input
          type="checkbox"
          checked={focusOverlayWs}
          onChange={(e) => setFocusOverlayWs(e.target.checked)}
        />
        {t('focus_overlay_workspace')}
      </label>
      <label className="row">
        <input
          type="checkbox"
          checked={autoOpenBrowser}
          onChange={(e) => setAutoOpenBrowser(e.target.checked)}
        />
        {t('auto_open_browser')}
      </label>
      <div className="row">
        <label>
          {t('ocr_lang')}{' '}
          <select className="select" value={ocr} onChange={(e) => setOcr(e.target.value)}>
            <option value="rus+eng">rus+eng</option>
            <option value="rus">rus</option>
            <option value="eng">eng</option>
          </select>
        </label>
        <span className="muted">{t('ocr_lang_hint')}</span>
      </div>
      <div className="row">
        <input
          className="search"
          value={eelog}
          onChange={(e) => setEelog(e.target.value)}
          placeholder={t('eelog_path')}
        />
      </div>
      <button
        className="btn"
        disabled={busy}
        type="button"
        onClick={() =>
          run(async () => {
            await api.patchConfig({
              inventory_consent: consent,
              overlay_enabled: overlay,
              focus_overlay_workspace: focusOverlayWs,
              auto_open_browser: autoOpenBrowser,
              ocr_lang: ocr,
              eelog_path: eelog,
              ui_lang: locale,
            })
            onSaved()
          })
        }
      >
        {t('save_settings')}
      </button>
    </div>
  )
}
