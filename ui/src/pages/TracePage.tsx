import { useEffect, useState, useCallback } from 'react'
import { useParams, useNavigate } from 'react-router-dom'
import { fetchInstance, fetchProcessDefinition, fetchHistory } from '../api/client'
import type { ProcessInstance, ProcessDefinitionView, HistoryEventResponse } from '../api/types'
import { InstanceHeader } from '../components/InstanceHeader'
import { ProcessDiagramPanel } from '../components/ProcessDiagramPanel'
import { ExecutionTimelinePanel } from '../components/ExecutionTimelinePanel'
import { EventDetailPanel } from '../components/EventDetailPanel'
import { useTraceStore } from '../store/traceStore'

export function TracePage() {
  const { instanceId } = useParams<{ instanceId: string }>()
  const navigate = useNavigate()
  const [instance, setInstance] = useState<ProcessInstance | null>(null)
  const [definition, setDefinition] = useState<ProcessDefinitionView | null>(null)
  const [history, setHistory] = useState<HistoryEventResponse[]>([])
  const [loading, setLoading] = useState(true)
  const [historyLoading, setHistoryLoading] = useState(true)
  const [eventTypeFilter, setEventTypeFilter] = useState('')
  const { tokenFilter, setTokenFilter } = useTraceStore()

  const load = useCallback(async () => {
    if (!instanceId) return
    setLoading(true)
    try {
      const inst = await fetchInstance(instanceId)
      setInstance(inst)
      if (inst.process_def_id) {
        const def = await fetchProcessDefinition(inst.process_def_id)
        setDefinition(def)
      } else {
        setDefinition(null)
      }
    } catch (e) {
      setInstance(null)
      setDefinition(null)
      console.error(e)
    } finally {
      setLoading(false)
    }
  }, [instanceId])

  const loadHistory = useCallback(async () => {
    if (!instanceId) return
    setHistoryLoading(true)
    try {
      const params: { token_id?: string; event_type?: string } = {}
      if (tokenFilter) params.token_id = tokenFilter
      if (eventTypeFilter) params.event_type = eventTypeFilter
      const list = await fetchHistory(instanceId, params)
      setHistory(list)
    } catch (e) {
      setHistory([])
      console.error(e)
    } finally {
      setHistoryLoading(false)
    }
  }, [instanceId, tokenFilter, eventTypeFilter])

  useEffect(() => {
    load()
  }, [load])

  useEffect(() => {
    loadHistory()
  }, [loadHistory])


  if (!instanceId) {
    navigate('/')
    return null
  }

  return (
    <div className="trace-page">
      <InstanceHeader instance={instance} loading={loading} onRefresh={load} />
      <div className="trace-main">
        <div className="trace-left">
          <ProcessDiagramPanel
            definition={definition}
            tokens={instance?.tokens ?? []}
            loading={loading}
          />
        </div>
        <div className="trace-right">
          <ExecutionTimelinePanel
            events={history}
            loading={historyLoading}
            tokenFilter={tokenFilter}
            eventTypeFilter={eventTypeFilter}
            tokenOptions={instance?.tokens ?? []}
            onTokenFilterChange={setTokenFilter}
            onEventTypeFilterChange={setEventTypeFilter}
          />
        </div>
      </div>
      <div className="trace-bottom">
        <EventDetailPanel />
      </div>
    </div>
  )
}
