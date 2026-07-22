import { useCallback, useEffect, useMemo, useState } from 'react'
import { Link, useNavigate } from 'react-router-dom'
import { api } from '../api/client'
import type { DefinitionVersion, ProcessInstance } from '../api/types'
import { Badge, Card, EmptyState, Notice, PageHeader, Spinner } from '../components/Ui'
import { errorMessage, parseVariables } from '../utils'
import { useI18n } from '../i18n'

export function InstancesPage() {
  const { locale, t } = useI18n()
  const statusLabel = (status: string) => locale === 'zh' ? t(`status.${status.toLowerCase()}`) : status
  const navigate = useNavigate()
  const [instances, setInstances] = useState<ProcessInstance[]>([])
  const [definitions, setDefinitions] = useState<DefinitionVersion[]>([])
  const [definitionId, setDefinitionId] = useState('')
  const [variables, setVariables] = useState('{}')
  const [query, setQuery] = useState('')
  const [lookupId, setLookupId] = useState('')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')

  const load = useCallback(async () => {
    setBusy(true); setError('')
    try {
      const [instanceList, definitionList] = await Promise.all([api.listInstances(), api.listDefinitions()])
      setInstances(instanceList); setDefinitions(definitionList)
      if (!definitionId) setDefinitionId(definitionList.find((item) => item.status === 'active')?.id ?? definitionList[0]?.id ?? '')
    } catch (e) { setError(errorMessage(e)) } finally { setBusy(false) }
  }, [definitionId])

  useEffect(() => { void load() }, [load])
  const filtered = useMemo(() => instances.filter((item) => `${item.instance_id} ${item.process_def_id} ${item.status}`.toLowerCase().includes(query.toLowerCase())), [instances, query])

  const start = async () => {
    setBusy(true); setError('')
    try {
      const result = await api.startInstance(definitionId, parseVariables(variables))
      navigate(`/trace/${encodeURIComponent(result.instance_id)}`)
    } catch (e) { setError(errorMessage(e)) } finally { setBusy(false) }
  }

  const lookup = async () => {
    setBusy(true); setError('')
    try {
      await api.getInstance(lookupId.trim())
      navigate(`/trace/${encodeURIComponent(lookupId.trim())}`)
    } catch (e) { setError(errorMessage(e)) } finally { setBusy(false) }
  }

  return <>
    <PageHeader eyebrow={t('instances.eyebrow')} title={t('instances.title')} description={t('instances.description')} actions={<button className="button secondary" onClick={load}>{busy ? <Spinner /> : t('common.refresh')}</button>} />
    {error && <Notice kind="error">{error}</Notice>}
    <div className="two-column uneven">
      <Card title={t('instances.startTitle')} subtitle={t('instances.startHint')}>
        <div className="form-grid"><label className="span-2">{t('instances.definition')}<select value={definitionId} onChange={(e) => setDefinitionId(e.target.value)}>{definitions.map((item) => <option key={item.id} value={item.id}>{item.id} · {statusLabel(item.status)}</option>)}</select></label><label className="span-2">{t('common.variables')}<textarea rows={5} value={variables} onChange={(e) => setVariables(e.target.value)} /></label></div>
        <button className="button" disabled={!definitionId || busy} onClick={start}>{t('instances.start')}</button>
      </Card>
      <Card title={t('instances.lookup')} subtitle={t('instances.lookupHint')}>
        <label>{t('instances.id')}<input value={lookupId} onChange={(e) => setLookupId(e.target.value)} placeholder="UUID" /></label>
        <button className="button secondary" disabled={!lookupId.trim() || busy} onClick={lookup}>{t('instances.load')}</button>
      </Card>
    </div>
    <Card title={t('instances.inventory')} subtitle={t('instances.matching', { count: filtered.length })} actions={<input className="search-input" value={query} onChange={(e) => setQuery(e.target.value)} placeholder={t('instances.search')} />}>
      {filtered.length === 0 ? <EmptyState title={t('instances.empty')} detail={t('instances.emptyHint')} /> : <div className="table-wrap"><table><thead><tr><th>{t('common.instance')}</th><th>{t('instances.definitionColumn')}</th><th>{t('common.status')}</th><th>{t('instances.currentNodes')}</th><th>{t('instances.tokens')}</th><th /></tr></thead><tbody>{filtered.map((item) => <tr key={item.instance_id}><td><code>{item.instance_id.slice(0, 12)}…</code></td><td>{item.process_def_id}</td><td><Badge value={item.status} /></td><td>{item.current_nodes.join(', ') || '—'}</td><td>{item.tokens.length}</td><td><Link className="text-link" to={`/trace/${encodeURIComponent(item.instance_id)}`}>{t('instances.openTrace')}</Link></td></tr>)}</tbody></table></div>}
    </Card>
  </>
}
