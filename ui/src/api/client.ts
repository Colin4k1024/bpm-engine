import type {
  ProcessInstance,
  ProcessDefinitionView,
  HistoryEventResponse,
} from './types'

const API_BASE = '/api/v1'

async function get<T>(path: string): Promise<T> {
  const res = await fetch(`${API_BASE}${path}`)
  if (!res.ok) {
    const err = await res.json().catch(() => ({ error: res.statusText }))
    throw new Error((err as { error?: string }).error ?? res.statusText)
  }
  return res.json()
}

export async function fetchInstance(instanceId: string): Promise<ProcessInstance> {
  return get<ProcessInstance>(`/process-instances/${instanceId}`)
}

export async function fetchProcessDefinition(defId: string): Promise<ProcessDefinitionView> {
  return get<ProcessDefinitionView>(`/process-definitions/${defId}`)
}

export async function fetchHistory(
  instanceId: string,
  params?: { token_id?: string; event_type?: string }
): Promise<HistoryEventResponse[]> {
  const q = new URLSearchParams()
  if (params?.token_id) q.set('token_id', params.token_id)
  if (params?.event_type) q.set('event_type', params.event_type)
  const query = q.toString()
  const path = `/process-instances/${encodeURIComponent(instanceId)}/history${query ? `?${query}` : ''}`
  return get<HistoryEventResponse[]>(path)
}
