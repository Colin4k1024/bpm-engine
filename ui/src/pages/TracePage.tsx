import { useCallback, useEffect, useRef, useState } from 'react'
import { Link, useParams, useSearchParams } from 'react-router-dom'
import { api } from '../api/client'
import type { HistoryEventResponse, ProcessDefinitionView, ProcessInstance, ReplaySnapshotResponse, ReplayStepResponse, TraceResponse } from '../api/types'
import { InstanceHeader } from '../components/InstanceHeader'
import { ProcessDiagramPanel } from '../components/ProcessDiagramPanel'
import { ExecutionTimelinePanel } from '../components/ExecutionTimelinePanel'
import { EventDetailPanel } from '../components/EventDetailPanel'
import { Card, Notice, Spinner } from '../components/Ui'
import { useTraceStore } from '../store/traceStore'
import { errorMessage } from '../utils'
import { useI18n } from '../i18n'

export function TracePage() {
  const { locale, t } = useI18n()
  const { instanceId } = useParams<{ instanceId: string }>()
  const [searchParams] = useSearchParams()
  const [instance, setInstance] = useState<ProcessInstance | null>(null)
  const [definition, setDefinition] = useState<ProcessDefinitionView | null>(null)
  const [trace, setTrace] = useState<TraceResponse | null>(null)
  const [history, setHistory] = useState<HistoryEventResponse[]>([])
  const [loading, setLoading] = useState(true)
  const [historyLoading, setHistoryLoading] = useState(true)
  const [eventTypeFilter, setEventTypeFilter] = useState('')
  const [error, setError] = useState('')
  const { tokenFilter } = useTraceStore()
  const [sessionId, setSessionId] = useState('')
  const [replay, setReplay] = useState<ReplaySnapshotResponse | null>(null)
  const [lastStep, setLastStep] = useState<ReplayStepResponse | null>(null)
  const [replayBusy, setReplayBusy] = useState(false)

  const load = useCallback(async () => {
    if (!instanceId) return
    setLoading(true); setError('')
    try {
      const traceResult = await api.getTrace(instanceId)
      setTrace(traceResult); setInstance(traceResult.instance)
      setDefinition(await api.getDefinition(traceResult.instance.process_def_id))
    } catch (e) { setInstance(null); setDefinition(null); setError(errorMessage(e)) } finally { setLoading(false) }
  }, [instanceId])

  // Last-request-wins guard: filter changes (user or the E6-2 deep-link
  // effect below) can re-issue the history request while a slower earlier
  // one is still in flight; a stale response must never clobber the
  // newest filter's result.
  const historyRequest = useRef(0)
  const loadHistory = useCallback(async () => {
    if (!instanceId) return
    const request = ++historyRequest.current
    setHistoryLoading(true)
    try {
      const events = await api.getHistory(instanceId, { token_id: tokenFilter ?? undefined, event_type: eventTypeFilter || undefined })
      if (historyRequest.current === request) setHistory(events)
    } catch (e) {
      if (historyRequest.current === request) { setHistory([]); setError(errorMessage(e)) }
    } finally {
      if (historyRequest.current === request) setHistoryLoading(false)
    }
  }, [instanceId, tokenFilter, eventTypeFilter])

  useEffect(() => { void load() }, [load])
  useEffect(() => { void loadHistory() }, [loadHistory])

  // E6-2 deep links (error → history jump from the diagnosis view): apply
  // the `token_id` / `event_type` query params to the history filters once
  // per navigation. Absent params never clobber user-selected filters, but
  // switching to a DIFFERENT instance clears the token filter — token ids
  // are instance-scoped, and a stale deep-link filter from a previous
  // instance must not silently apply to this one.
  const { setTokenFilter } = useTraceStore()
  useEffect(() => {
    setTokenFilter(null)
  }, [instanceId, setTokenFilter])
  useEffect(() => {
    const token = searchParams.get('token_id')
    const type = searchParams.get('event_type')
    if (token !== null) setTokenFilter(token || null)
    if (type !== null) setEventTypeFilter(type)
  }, [searchParams, setTokenFilter])

  const refreshReplay = async (id = sessionId) => { if (id) setReplay(await api.replaySnapshot(id)) }
  const createReplay = async () => {
    if (!instanceId) return
    setReplayBusy(true); setError('')
    try { const created = await api.createReplay(instanceId); setSessionId(created.session_id); await refreshReplay(created.session_id); setLastStep(null) }
    catch (e) { setError(errorMessage(e)) } finally { setReplayBusy(false) }
  }
  const stepReplay = async () => {
    setReplayBusy(true); setError('')
    try { const result = await api.replayStep(sessionId); setLastStep(result); await refreshReplay() }
    catch (e) { setError(errorMessage(e)) } finally { setReplayBusy(false) }
  }
  const seekReplay = async (cursor: number) => {
    setReplayBusy(true); setError('')
    try { await api.replaySeek(sessionId, cursor); setLastStep(null); await refreshReplay() }
    catch (e) { setError(errorMessage(e)) } finally { setReplayBusy(false) }
  }
  const closeReplay = async () => {
    setReplayBusy(true)
    try { await api.deleteReplay(sessionId); setSessionId(''); setReplay(null); setLastStep(null) }
    catch (e) { setError(errorMessage(e)) } finally { setReplayBusy(false) }
  }

  if (!instanceId) return <Notice kind="error">{t('trace.missing')} <Link to="/instances">{t('trace.return')}</Link></Notice>

  return <>
    <div className="trace-breadcrumb"><Link to="/instances">{t('trace.instances')}</Link><span>/</span><code>{instanceId}</code></div>
    {error && <Notice kind="error">{error}</Notice>}
    <InstanceHeader instance={instance} loading={loading} onRefresh={() => { void load(); void loadHistory() }}
      actions={instance && <Link className="header-link" to={`/diagnosis/${encodeURIComponent(instanceId)}`}>{t('diagnosis.openDiagnosis')}</Link>} />
    <div className="trace-layout">
      <Card title={t('trace.topology')} subtitle={t('trace.topologyHint')}><ProcessDiagramPanel definition={definition} tokens={instance?.tokens ?? []} loading={loading} /></Card>
      <Card title={t('trace.history')} subtitle={t('trace.historyHint')}><ExecutionTimelinePanel events={history} loading={historyLoading} tokenFilter={tokenFilter} eventTypeFilter={eventTypeFilter} tokenOptions={instance?.tokens ?? []} onTokenFilterChange={setTokenFilter} onEventTypeFilterChange={setEventTypeFilter} /></Card>
    </div>
    <div className="trace-layout bottom">
      <Card title={t('trace.payload')} subtitle={t('trace.payloadHint')}><EventDetailPanel /></Card>
      <Card title={t('trace.replay')} subtitle={t('trace.replayHint')} actions={!sessionId ? <button className="button" onClick={createReplay}>{replayBusy ? <Spinner /> : t('trace.createReplay')}</button> : <button className="button secondary" onClick={closeReplay}>{t('trace.close')}</button>}>
        {!sessionId ? <div className="replay-placeholder"><strong>{t('trace.timelineCount', { count: trace?.token_timelines.length ?? 0 })}</strong><span>{t('trace.createHint', { count: history.length })}</span></div> : <>
          <div className="replay-controls"><button className="button secondary" disabled={replayBusy || replay?.cursor === 0} onClick={() => seekReplay(Math.max(0, (replay?.cursor ?? 0) - 1))}>{t('trace.back')}</button><button className="button" disabled={replayBusy || replay?.cursor === replay?.total_events} onClick={stepReplay}>{replayBusy ? <Spinner /> : t('trace.step')}</button><button className="button secondary" disabled={replayBusy} onClick={() => seekReplay(replay?.total_events ?? 0)}>{t('trace.seekEnd')}</button></div>
          <div className="replay-progress"><div style={{ width: `${replay?.total_events ? ((replay.cursor / replay.total_events) * 100) : 0}%` }} /><span>{replay?.cursor ?? 0} / {replay?.total_events ?? 0}</span></div>
          {lastStep && <Notice>{lastStep.event.event_type} · {lastStep.event.node_id ?? lastStep.event.token_id ?? t('trace.instanceEvent')}</Notice>}
          <div className="node-chip-list">{replay?.tokens.map((token) => <span key={token.token_id}><b>{token.node_id}</b>{locale === 'zh' ? t(`status.${token.state.toLowerCase()}`) : token.state}</span>)}</div>
        </>}
      </Card>
    </div>
  </>
}
