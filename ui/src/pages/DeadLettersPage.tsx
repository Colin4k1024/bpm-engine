import { useCallback, useEffect, useState } from 'react'
import { Link } from 'react-router-dom'
import { api } from '../api/client'
import type { DeadLetterEntry } from '../api/types'
import { Card, EmptyState, Notice, PageHeader, Spinner } from '../components/Ui'
import { errorMessage } from '../utils'
import { useI18n } from '../i18n'

export function DeadLettersPage() {
  const { t, formatDate } = useI18n()
  const [entries, setEntries] = useState<DeadLetterEntry[]>([])
  const [selected, setSelected] = useState<DeadLetterEntry | null>(null)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')
  const [notice, setNotice] = useState('')

  const load = useCallback(async () => {
    setBusy(true); setError('')
    try { setEntries(await api.listDeadLetters()) } catch (e) { setError(errorMessage(e)) } finally { setBusy(false) }
  }, [])
  useEffect(() => { void load() }, [load])

  const select = async (entry: DeadLetterEntry) => {
    setBusy(true); setError('')
    try { setSelected(await api.getDeadLetter(entry.id)) } catch (e) { setError(errorMessage(e)) } finally { setBusy(false) }
  }
  const requeue = async () => {
    if (!selected) return
    setBusy(true); setError('')
    try { const result = await api.requeueDeadLetter(selected.id); setNotice(t('dead.requeued', { id: result.task_id })); setSelected(null); await load() } catch (e) { setError(errorMessage(e)) } finally { setBusy(false) }
  }
  const remove = async () => {
    if (!selected || !window.confirm(t('dead.confirm', { id: selected.id }))) return
    setBusy(true); setError('')
    try { await api.deleteDeadLetter(selected.id); setNotice(t('dead.deleted')); setSelected(null); await load() } catch (e) { setError(errorMessage(e)) } finally { setBusy(false) }
  }

  return <>
    <PageHeader eyebrow={t('dead.eyebrow')} title={t('dead.title')} description={t('dead.description')} actions={<button className="button secondary" onClick={load}>{busy ? <Spinner /> : t('dead.refresh')}</button>} />
    {error && <Notice kind="error">{error}</Notice>}{notice && <Notice kind="success">{notice}</Notice>}
    <div className="split-workspace">
      <Card title={t('dead.failed')} subtitle={t('dead.count', { count: entries.length })} className="list-card">
        {entries.length === 0 ? <EmptyState title={t('dead.empty')} detail={t('dead.emptyHint')} /> : <div className="record-list">{entries.map((entry) => <button key={entry.id} className={selected?.id === entry.id ? 'selected' : ''} onClick={() => select(entry)}><span><strong>{entry.task_type}</strong><small>{entry.error_message}</small></span><span><small>{formatDate(entry.created_at)}</small></span></button>)}</div>}
      </Card>
      <Card title={selected?.task_type ?? t('dead.detail')} subtitle={selected?.id ?? t('dead.select')} actions={selected && <div className="button-row"><button className="button danger" onClick={remove}>{t('common.delete')}</button><button className="button" onClick={requeue}>{t('dead.requeue')}</button></div>}>
        {!selected ? <EmptyState title={t('dead.noSelection')} detail={t('dead.noSelectionHint')} /> : <>
          <div className="detail-grid"><div><span>{t('common.task')}</span><strong>{selected.task_id}</strong></div><div><span>{t('common.token')}</span><strong>{selected.token_id}</strong></div><div><span>{t('common.instance')}</span><strong><Link to={`/trace/${encodeURIComponent(selected.process_instance_id)}`}>{selected.process_instance_id.slice(0, 12)}…</Link></strong></div><div><span>{t('common.tenant')}</span><strong>{selected.tenant_id ?? t('common.default')}</strong></div></div>
          <Notice kind="error">{selected.error_message}</Notice><h3>{t('dead.persistedVariables')}</h3><pre className="json-view">{selected.variables}</pre>
        </>}
      </Card>
    </div>
  </>
}
