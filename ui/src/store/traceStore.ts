import { create } from 'zustand'

export interface HistoryEventPayload {
  [key: string]: unknown
}

export interface HistoryEvent {
  id: string
  instance_id: string
  event_type: string
  payload: HistoryEventPayload
  occurred_at: string
}

interface TraceState {
  selectedEvent: HistoryEvent | null
  tokenFilter: string | null
  setSelectedEvent: (event: HistoryEvent | null) => void
  setTokenFilter: (tokenId: string | null) => void
}

export const useTraceStore = create<TraceState>((set) => ({
  selectedEvent: null,
  tokenFilter: null,
  setSelectedEvent: (event) => set({ selectedEvent: event }),
  setTokenFilter: (tokenId) => set({ tokenFilter: tokenId }),
}))
