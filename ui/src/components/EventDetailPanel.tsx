import { useTraceStore } from '../store/traceStore'

export function EventDetailPanel() {
  const { selectedEvent } = useTraceStore()

  if (!selectedEvent) {
    return (
      <div className="event-detail-panel">
        <div className="panel-placeholder">Click a timeline item to see details</div>
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
      <h3 className="event-detail-title">Event Details</h3>
      <pre className="event-detail-json">{JSON.stringify(detail, null, 2)}</pre>
    </div>
  )
}
