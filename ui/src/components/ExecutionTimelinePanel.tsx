import type { HistoryEventResponse } from '../api/types'
import { useTraceStore } from '../store/traceStore'
import { useI18n } from '../i18n'

const EVENT_ICON: Record<string, string> = {
  ProcessStarted: '⚪',
  TokenArrived: '⚪',
  TokenCompleted: '⚪',
  UserTaskCreated: '📦',
  UserTaskCompleted: '✅',
  TimerFired: '⏱',
  TimerScheduled: '⏱',
  TokenFailed: '❌',
  SagaStarted: '🔱',
  SagaCompleted: '✅',
  ProcessCompleted: '⚪',
}

const EVENT_COLOR: Record<string, string> = {
  ProcessStarted: 'gray',
  TokenArrived: 'gray',
  TokenCompleted: 'gray',
  UserTaskCreated: 'blue',
  UserTaskCompleted: 'green',
  TimerFired: 'yellow',
  TimerScheduled: 'yellow',
  TokenFailed: 'red',
  SagaStarted: 'purple',
  SagaCompleted: 'green',
  ProcessCompleted: 'gray',
}

interface ExecutionTimelinePanelProps {
  events: HistoryEventResponse[]
  loading: boolean
  tokenFilter: string | null
  eventTypeFilter: string
  tokenOptions: { id: string }[]
  onTokenFilterChange: (tokenId: string | null) => void
  onEventTypeFilterChange: (eventType: string) => void
}

export function ExecutionTimelinePanel({
  events,
  loading,
  tokenFilter,
  eventTypeFilter,
  tokenOptions,
  onTokenFilterChange,
  onEventTypeFilterChange,
}: ExecutionTimelinePanelProps) {
  const { setSelectedEvent } = useTraceStore()
  const { t, formatDate } = useI18n()

  const tokenIds = tokenOptions.length > 0 ? tokenOptions.map((t) => t.id) : [...new Set(events.map((e) => e.payload?.token_id).filter(Boolean))] as string[]
  const eventTypes = [...new Set(events.map((e) => e.event_type))]

  const formatTime = (occurred_at: string) => {
    const n = parseInt(occurred_at, 10)
    if (!Number.isNaN(n)) {
      return formatDate(n * 1000)
    }
    return occurred_at
  }

  if (loading) {
    return (
      <div className="execution-timeline-panel">
        <div className="panel-placeholder">{t('component.loadingTimeline')}</div>
      </div>
    )
  }

  return (
    <div className="execution-timeline-panel">
      <div className="timeline-toolbar">
        <select
          value={tokenFilter ?? ''}
          onChange={(e) => onTokenFilterChange(e.target.value || null)}
          aria-label={t('component.filterToken')}
        >
          <option value="">{t('component.allTokens')}</option>
          {tokenIds.map((id) => (
            <option key={id} value={id}>
              {id}
            </option>
          ))}
        </select>
        <select
          value={eventTypeFilter}
          onChange={(e) => onEventTypeFilterChange(e.target.value)}
          aria-label={t('component.filterType')}
        >
          <option value="">{t('component.allTypes')}</option>
          {eventTypes.map((t) => (
            <option key={t} value={t}>
              {t}
            </option>
          ))}
        </select>
      </div>
      <div className="timeline-list">
        {events.length === 0 ? (
          <div className="timeline-empty">{t('component.noEvents')}</div>
        ) : (
          events.map((ev) => (
            <button
              key={ev.id}
              type="button"
              className="timeline-item"
              onClick={() =>
                setSelectedEvent({
                  id: ev.id,
                  instance_id: ev.instance_id,
                  event_type: ev.event_type,
                  payload: ev.payload ?? {},
                  occurred_at: ev.occurred_at,
                })
              }
            >
              <span className="timeline-item-icon" style={{ color: EVENT_COLOR[ev.event_type] ?? 'gray' }}>
                {EVENT_ICON[ev.event_type] ?? '•'}
              </span>
              <span className="timeline-item-time">{formatTime(ev.occurred_at)}</span>
              <span className="timeline-item-type">{ev.event_type}</span>
              {Boolean(ev.payload?.token_id ?? ev.payload?.node_id) && (
                <span className="timeline-item-meta">
                  {[ev.payload?.token_id, ev.payload?.node_id].filter(Boolean).map(String).join(' · ')}
                </span>
              )}
            </button>
          ))
        )}
      </div>
    </div>
  )
}
