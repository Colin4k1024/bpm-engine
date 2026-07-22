import { useState } from 'react'
import { Link } from 'react-router-dom'
import { api } from '../api/client'
import type { ExternalTask } from '../api/types'
import { Card, EmptyState, Notice, PageHeader, Spinner } from '../components/Ui'
import { errorMessage, parseVariables } from '../utils'
import { useI18n } from '../i18n'

export function WorkersPage() {
  const { t } = useI18n()
  const [workerId, setWorkerId] = useState('console-worker')
  const [topics, setTopics] = useState('payment')
  const [maxTasks, setMaxTasks] = useState(10)
  const [lockMs, setLockMs] = useState(60000)
  const [tasks, setTasks] = useState<ExternalTask[]>([])
  const [selected, setSelected] = useState<ExternalTask | null>(null)
  const [variables, setVariables] = useState('{}')
  const [failure, setFailure] = useState('')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')
  const [notice, setNotice] = useState('')

  const fetchTasks = async () => {
    setBusy(true); setError(''); setNotice('')
    try {
      const result = await api.fetchAndLock(workerId, topics.split(',').map((item) => item.trim()).filter(Boolean), maxTasks, lockMs)
      setTasks(result); setSelected(result[0] ?? null); setNotice(t('workers.locked', { count: result.length }))
    } catch (e) { setError(errorMessage(e)) } finally { setBusy(false) }
  }

  const act = async (action: 'complete' | 'fail' | 'extend') => {
    if (!selected) return
    setBusy(true); setError(''); setNotice('')
    try {
      if (action === 'complete') await api.completeExternalTask(selected.task_id, workerId, parseVariables(variables))
      if (action === 'fail') await api.failExternalTask(selected.task_id, workerId, failure || t('workers.defaultError'), 5000)
      if (action === 'extend') await api.extendExternalTaskLock(selected.task_id, workerId, lockMs)
      setNotice(t('workers.actionResult', { id: selected.task_id, action: t(`workers.action.${action === 'extend' ? 'extended' : action === 'complete' ? 'completed' : 'failed'}`) }))
      if (action !== 'extend') { setTasks((current) => current.filter((task) => task.task_id !== selected.task_id)); setSelected(null) }
    } catch (e) { setError(errorMessage(e)) } finally { setBusy(false) }
  }

  return <>
    <PageHeader eyebrow={t('workers.eyebrow')} title={t('workers.title')} description={t('workers.description')} />
    {error && <Notice kind="error">{error}</Notice>}{notice && <Notice kind="success">{notice}</Notice>}
    <Card title={t('workers.subscription')} subtitle={t('workers.subscriptionHint')}>
      <div className="form-grid four"><label>{t('workers.workerId')}<input value={workerId} onChange={(e) => setWorkerId(e.target.value)} /></label><label>{t('workers.topics')}<input value={topics} onChange={(e) => setTopics(e.target.value)} placeholder="payment, email" /></label><label>{t('workers.maxTasks')}<input type="number" min="1" value={maxTasks} onChange={(e) => setMaxTasks(Number(e.target.value))} /></label><label>{t('workers.lockDuration')}<input type="number" min="1000" value={lockMs} onChange={(e) => setLockMs(Number(e.target.value))} /></label></div>
      <button className="button" disabled={busy || !workerId || !topics} onClick={fetchTasks}>{busy ? <Spinner /> : t('workers.fetch')}</button>
    </Card>
    <div className="split-workspace">
      <Card title={t('workers.leased')} subtitle={t('workers.owned', { count: tasks.length, worker: workerId })} className="list-card">
        {tasks.length === 0 ? <EmptyState title={t('workers.empty')} detail={t('workers.emptyHint')} /> : <div className="record-list">{tasks.map((task) => <button key={task.task_id} className={selected?.task_id === task.task_id ? 'selected' : ''} onClick={() => setSelected(task)}><span><strong>{task.task_type}</strong><small>{task.task_id}</small></span><span><small>{task.process_instance_id.slice(0, 8)}…</small></span></button>)}</div>}
      </Card>
      <Card title={selected?.task_type ?? t('workers.operation')} subtitle={selected?.task_id ?? t('workers.select')}>
        {!selected ? <EmptyState title={t('workers.noSelection')} detail={t('workers.noSelectionHint')} /> : <>
          <div className="detail-grid"><div><span>{t('common.instance')}</span><strong><Link to={`/trace/${encodeURIComponent(selected.process_instance_id)}`}>{selected.process_instance_id.slice(0, 12)}…</Link></strong></div><div><span>{t('common.token')}</span><strong>{selected.token_id.slice(0, 12)}…</strong></div></div>
          <label>{t('workers.completionVariables')}<textarea rows={4} value={variables} onChange={(e) => setVariables(e.target.value)} /></label>
          <label>{t('workers.failureMessage')}<input value={failure} onChange={(e) => setFailure(e.target.value)} placeholder={t('workers.defaultError')} /></label>
          <div className="button-row"><button className="button secondary" disabled={busy} onClick={() => act('extend')}>{t('workers.extend')}</button><button className="button danger" disabled={busy} onClick={() => act('fail')}>{t('workers.fail')}</button><button className="button" disabled={busy} onClick={() => act('complete')}>{t('workers.complete')}</button></div>
          <h3>{t('workers.inputVariables')}</h3><pre className="json-view">{JSON.stringify(selected.variables, null, 2)}</pre>
        </>}
      </Card>
    </div>
  </>
}
