import { useCallback, useEffect, useState } from 'react'
import { useNavigate } from 'react-router-dom'
import { api } from '../api/client'
import type { DefinitionVersion, ProcessDefinitionView } from '../api/types'
import { Badge, Card, EmptyState, Notice, PageHeader, Spinner } from '../components/Ui'
import { errorMessage, parseVariables } from '../utils'
import { useI18n } from '../i18n'

const starterBpmn = `<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL" targetNamespace="http://bpm.local">
  <process id="approval-flow:1" isExecutable="true">
    <startEvent id="start" />
    <userTask id="review" name="Review request" />
    <endEvent id="done" />
    <sequenceFlow id="f1" sourceRef="start" targetRef="review" />
    <sequenceFlow id="f2" sourceRef="review" targetRef="done" />
  </process>
</definitions>`

export function DefinitionsPage() {
  const { locale, t, formatDate, formatNumber } = useI18n()
  const statusLabel = (status: string) => locale === 'zh' ? t(`status.${status.toLowerCase()}`) : status
  const navigate = useNavigate()
  const [definitions, setDefinitions] = useState<DefinitionVersion[]>([])
  const [selected, setSelected] = useState<DefinitionVersion | null>(null)
  const [view, setView] = useState<ProcessDefinitionView | null>(null)
  const [versions, setVersions] = useState<DefinitionVersion[]>([])
  const [active, setActive] = useState<DefinitionVersion | null>(null)
  const [xml, setXml] = useState(starterBpmn)
  const [variables, setVariables] = useState('{}')
  const [encrypted, setEncrypted] = useState('')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')
  const [notice, setNotice] = useState('')

  const load = useCallback(async () => {
    try {
      const list = await api.listDefinitions()
      setDefinitions(list)
      if (!selected && list.length) setSelected(list[0])
    } catch (e) { setError(errorMessage(e)) }
  }, [selected])

  useEffect(() => { void load() }, [load])
  useEffect(() => {
    if (!selected) { setView(null); setVersions([]); setActive(null); return }
    setError('')
    Promise.allSettled([
      api.getDefinition(selected.id), api.listDefinitionVersions(selected.key), api.getActiveDefinition(selected.key),
    ]).then(([definitionResult, versionsResult, activeResult]) => {
      if (definitionResult.status === 'fulfilled') setView(definitionResult.value)
      else setError(errorMessage(definitionResult.reason))
      if (versionsResult.status === 'fulfilled') setVersions(versionsResult.value.versions)
      if (activeResult.status === 'fulfilled') setActive(activeResult.value); else setActive(null)
    })
  }, [selected])

  const deploy = async () => {
    setBusy(true); setError(''); setNotice('')
    try {
      const result = await api.deployDefinition(xml)
      setNotice(t('definitions.deployed', { id: result.process_definition_id }))
      setSelected(null); await load()
    } catch (e) { setError(errorMessage(e)) } finally { setBusy(false) }
  }

  const updateStatus = async (mode: 'activate' | 'deprecate') => {
    if (!selected) return
    setBusy(true); setError('')
    try {
      await (mode === 'activate' ? api.activateDefinition(selected.id) : api.deprecateDefinition(selected.id))
      setNotice(t('definitions.statusChanged', { id: selected.id, status: statusLabel(mode === 'activate' ? 'active' : 'deprecated') }))
      await load()
      const refreshed = await api.listDefinitionVersions(selected.key)
      setVersions(refreshed.versions)
      setSelected(refreshed.versions.find((item) => item.id === selected.id) ?? selected)
      setActive(mode === 'activate' ? { ...selected, status: 'active' } : null)
    } catch (e) { setError(errorMessage(e)) } finally { setBusy(false) }
  }

  const start = async () => {
    if (!selected) return
    setBusy(true); setError('')
    try {
      const result = await api.startInstance(selected.id, parseVariables(variables), encrypted.split(',').map((v) => v.trim()).filter(Boolean))
      navigate(`/trace/${encodeURIComponent(result.instance_id)}`)
    } catch (e) { setError(errorMessage(e)) } finally { setBusy(false) }
  }

  return <>
    <PageHeader eyebrow={t('definitions.eyebrow')} title={t('definitions.title')} description={t('definitions.description')} />
    {error && <Notice kind="error">{error}</Notice>}{notice && <Notice kind="success">{notice}</Notice>}
    <div className="split-workspace">
      <Card title={t('definitions.registry')} subtitle={t('definitions.count', { count: definitions.length })} className="list-card">
        {definitions.length === 0 ? <EmptyState title={t('definitions.empty')} detail={t('definitions.emptyHint')} /> : <div className="record-list">{definitions.map((item) => <button key={item.id} className={selected?.id === item.id ? 'selected' : ''} onClick={() => setSelected(item)}><span><strong>{item.key}</strong><small>{item.id}</small></span><span><Badge value={item.status} /><small>v{item.version}</small></span></button>)}</div>}
      </Card>
      <div className="workspace-detail">
        <Card title={selected?.id ?? t('definitions.detail')} subtitle={selected ? t('definitions.created', { date: formatDate(selected.created_at) }) : t('definitions.select')} actions={selected && <div className="button-row"><button className="button secondary" disabled={busy} onClick={() => updateStatus('deprecate')}>{t('definitions.deprecate')}</button><button className="button" disabled={busy} onClick={() => updateStatus('activate')}>{t('definitions.activate')}</button></div>}>
          {!selected ? <EmptyState title={t('definitions.nothing')} detail={t('definitions.nothingHint')} /> : <>
            <div className="detail-grid"><div><span>{t('definitions.activeVersion')}</span><strong>{active?.id ?? t('common.none')}</strong></div><div><span>{t('definitions.startNode')}</span><strong>{view?.start ?? '—'}</strong></div><div><span>{t('definitions.nodes')}</span><strong>{view?.nodes.length ?? 0}</strong></div><div><span>{t('definitions.edges')}</span><strong>{view?.edges.length ?? 0}</strong></div></div>
            <h3>{t('definitions.history')}</h3><div className="version-strip">{versions.map((item) => <button key={item.id} className={item.id === selected.id ? 'active' : ''} onClick={() => setSelected(item)}>v{item.version}<small>{statusLabel(item.status)}</small></button>)}</div>
            <h3>{t('definitions.graph')}</h3><div className="node-chip-list">{view?.nodes.map((node) => <span key={node.id}><b>{node.id}</b>{node.node_type}</span>)}</div>
            <div className="form-grid"><label className="span-2">{t('definitions.initialVariables')}<textarea value={variables} onChange={(e) => setVariables(e.target.value)} rows={4} /></label><label className="span-2">{t('definitions.encrypted')}<input value={encrypted} onChange={(e) => setEncrypted(e.target.value)} placeholder="card_number, secret" /></label></div>
            <button className="button" disabled={busy} onClick={start}>{busy ? <Spinner /> : t('definitions.start')}</button>
          </>}
        </Card>
        <Card title={t('definitions.deployment')} subtitle={t('definitions.deploymentHint')}>
          <textarea className="code-editor" value={xml} onChange={(e) => setXml(e.target.value)} rows={14} spellCheck={false} />
          <div className="card-footer"><span>{t('definitions.characters', { count: formatNumber(xml.length) })}</span><button className="button" disabled={busy || !xml.trim()} onClick={deploy}>{busy ? <Spinner /> : t('definitions.deploy')}</button></div>
        </Card>
      </div>
    </div>
  </>
}
