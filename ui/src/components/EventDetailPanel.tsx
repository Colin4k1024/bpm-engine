import { useTraceStore } from '../store/traceStore'
import { useI18n } from '../i18n'

export function EventDetailPanel() {
  const { selectedEvent } = useTraceStore()
  const { t } = useI18n()

  if (!selectedEvent) {
    return (
      <div className="event-detail-panel">
        <div className="panel-placeholder">{t('component.clickEvent')}</div>
      </div>
    )
  }

  const detail = {
    ...selectedEvent.payload,
    event_type: selectedEvent.event_type,
    occurred_at: selectedEvent.occurred_at,
  }

  return (
    <div className="event-detail-panel">
      <h3 className="event-detail-title">{t('component.eventDetails')}</h3>
      <pre className="event-detail-json">{JSON.stringify(detail, null, 2)}</pre>
    </div>
  )
}
