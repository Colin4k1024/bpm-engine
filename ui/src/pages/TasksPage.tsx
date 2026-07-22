import { useCallback, useEffect, useState } from 'react'
import { Link } from 'react-router-dom'
import { api } from '../api/client'
import type { FormField, TaskForm, TaskListItem, Variables } from '../api/types'
import { Card, EmptyState, Notice, PageHeader, Spinner } from '../components/Ui'
import { errorMessage } from '../utils'
import { useI18n } from '../i18n'

function FieldInput({ field, value, onChange }: { field: FormField; value: string; onChange: (value: string) => void }) {
  const { t } = useI18n()
  if (field.type === 'enum') return <select value={value} onChange={(e) => onChange(e.target.value)}><option value="">{t('common.select')}</option>{field.options?.map((item) => <option key={item}>{item}</option>)}</select>
  if (field.type === 'boolean') return <select value={value} onChange={(e) => onChange(e.target.value)}><option value="">{t('common.select')}</option><option value="true">{t('common.true')}</option><option value="false">{t('common.false')}</option></select>
  return <input type={field.type === 'number' ? 'number' : 'text'} value={value} onChange={(e) => onChange(e.target.value)} />
}

export function TasksPage() {
  const { t } = useI18n()
  const [tasks, setTasks] = useState<TaskListItem[]>([])
  const [selected, setSelected] = useState<TaskListItem | null>(null)
  const [form, setForm] = useState<TaskForm | null>(null)
  const [values, setValues] = useState<Variables>({})
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')
  const [notice, setNotice] = useState('')

  const load = useCallback(async () => {
    setBusy(true); setError('')
    try { setTasks(await api.listTasks('user')) } catch (e) { setError(errorMessage(e)) } finally { setBusy(false) }
  }, [])
  useEffect(() => { void load() }, [load])

  const selectTask = async (task: TaskListItem) => {
    setSelected(task); setBusy(true); setError(''); setNotice('')
    try {
      const schema = await api.getTaskForm(task.task_id)
      setForm(schema)
      setValues(Object.fromEntries((schema.fields ?? []).map((field) => [field.id, field.default_value ?? ''])))
    } catch (e) { setForm(null); setError(errorMessage(e)) } finally { setBusy(false) }
  }

  const complete = async () => {
    if (!selected) return
    setBusy(true); setError('')
    try {
      await api.completeTask(selected.task_id, values)
      setNotice(t('tasks.completed', { id: selected.task_id })); setSelected(null); setForm(null); await load()
    } catch (e) { setError(errorMessage(e)) } finally { setBusy(false) }
  }

  return <>
    <PageHeader eyebrow={t('tasks.eyebrow')} title={t('tasks.title')} description={t('tasks.description')} actions={<button className="button secondary" onClick={load}>{busy ? <Spinner /> : t('tasks.refresh')}</button>} />
    {error && <Notice kind="error">{error}</Notice>}{notice && <Notice kind="success">{notice}</Notice>}
    <div className="split-workspace">
      <Card title={t('tasks.waiting')} subtitle={t('tasks.available', { count: tasks.length })} className="list-card">
        {tasks.length === 0 ? <EmptyState title={t('tasks.empty')} detail={t('tasks.emptyHint')} /> : <div className="record-list">{tasks.map((task) => <button key={task.task_id} className={selected?.task_id === task.task_id ? 'selected' : ''} onClick={() => selectTask(task)}><span><strong>{task.node_id}</strong><small>{task.instance_id}</small></span><span><small>{task.task_type}</small></span></button>)}</div>}
      </Card>
      <Card title={selected ? t('tasks.completeTitle', { node: selected.node_id }) : t('tasks.detail')} subtitle={selected?.task_id ?? t('tasks.choose')}>
        {!selected ? <EmptyState title={t('tasks.noSelection')} detail={t('tasks.noSelectionHint')} /> : <>
          <div className="detail-grid"><div><span>{t('common.instance')}</span><strong><Link to={`/trace/${encodeURIComponent(selected.instance_id)}`}>{selected.instance_id.slice(0, 12)}…</Link></strong></div><div><span>{t('tasks.formKey')}</span><strong>{form?.form_key ?? t('tasks.inlineForm')}</strong></div></div>
          <div className="dynamic-form">{form?.fields?.length ? form.fields.map((field) => <label key={field.id}>{field.label}{field.required && <em>{t('common.required')}</em>}<FieldInput field={field} value={values[field.id] ?? ''} onChange={(value) => setValues((current) => ({ ...current, [field.id]: value }))} /></label>) : <Notice>{t('tasks.noFields')}</Notice>}</div>
          <button className="button" disabled={busy || Boolean(form?.fields?.some((field) => field.required && !values[field.id]))} onClick={complete}>{busy ? <Spinner /> : t('tasks.complete')}</button>
        </>}
      </Card>
    </div>
  </>
}
