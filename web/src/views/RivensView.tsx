import { useState } from 'react'
import { api } from '../api'

export function RivensView({ t }: { t: (k: string) => string }) {
  const [text, setText] = useState('')
  const [oldT, setOldT] = useState('')
  const [newT, setNewT] = useState('')
  const [result, setResult] = useState<any>(null)

  return (
    <div className="row" style={{ alignItems: 'stretch' }}>
      <div className="panel" style={{ flex: 1 }}>
        <h3 style={{ marginTop: 0 }}>{t('analyze')}</h3>
        <textarea
          style={{ width: '100%', minHeight: 120, background: '#0b0e14', border: '1px solid var(--line)', borderRadius: 8, padding: 8 }}
          value={text}
          onChange={(e) => setText(e.target.value)}
        />
        <button
          className="btn"
          style={{ marginTop: 8 }}
          type="button"
          onClick={() => api.analyzeRiven(text).then(setResult)}
        >
          {t('analyze_btn')}
        </button>
      </div>
      <div className="panel" style={{ flex: 1 }}>
        <h3 style={{ marginTop: 0 }}>{t('compare_rerolls')}</h3>
        <textarea
          placeholder="old"
          style={{ width: '100%', minHeight: 60, background: '#0b0e14', border: '1px solid var(--line)', borderRadius: 8, padding: 8 }}
          value={oldT}
          onChange={(e) => setOldT(e.target.value)}
        />
        <textarea
          placeholder="new"
          style={{ width: '100%', minHeight: 60, marginTop: 8, background: '#0b0e14', border: '1px solid var(--line)', borderRadius: 8, padding: 8 }}
          value={newT}
          onChange={(e) => setNewT(e.target.value)}
        />
        <button
          className="btn"
          style={{ marginTop: 8 }}
          type="button"
          onClick={() => api.compareRivens(oldT, newT).then(setResult)}
        >
          {t('compare_btn')}
        </button>
      </div>
      {result && (
        <pre className="log" style={{ width: '100%' }}>
          {JSON.stringify(result, null, 2)}
        </pre>
      )}
    </div>
  )
}
