import { getConnectionConfig } from './config'
import type {
  DeadLetterEntry,
  DefinitionVersion,
  ExternalTask,
  HealthResponse,
  HistoryEventResponse,
  InvariantCheckResult,
  ProcessDefinitionView,
  ProcessInstance,
  ReadinessResponse,
  ReplayCreateResponse,
  ReplaySeekResponse,
  ReplaySnapshotResponse,
  ReplayStepResponse,
  TaskForm,
  TaskListItem,
  TraceResponse,
  Variables,
} from './types'

const API_BASE = '/api/v1'

export class ApiError extends Error {
  constructor(message: string, public status: number, public details?: unknown) {
    super(message)
    this.name = 'ApiError'
  }
}

async function request<T>(path: string, init: RequestInit = {}, api = true): Promise<T> {
  const { apiKey, tenantId } = getConnectionConfig()
  const headers = new Headers(init.headers)
  if (apiKey) headers.set('X-API-Key', apiKey)
  if (tenantId) headers.set('X-Tenant-Id', tenantId)
  const response = await fetch(`${api ? API_BASE : ''}${path}`, { ...init, headers })
  if (!response.ok) {
    const details = await response.json().catch(() => ({ error: response.statusText }))
    const message = typeof details === 'object' && details && 'error' in details
      ? String((details as { error: unknown }).error)
      : response.statusText
    throw new ApiError(message || `HTTP ${response.status}`, response.status, details)
  }
  if (response.status === 204) return undefined as T
  return response.json() as Promise<T>
}

function json(body: unknown): RequestInit {
  return { headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body) }
}

const encode = encodeURIComponent

export const api = {
  health: () => request<HealthResponse>('/health', {}, false),
  readiness: () => request<ReadinessResponse>('/ready', {}, false),
  invariantCheck: () => request<InvariantCheckResult>('/invariants/check'),

  listInstances: async () => (await request<{ instances: ProcessInstance[] }>('/process-instances')).instances,
  startInstance: (process_def_id: string, variables: Variables, encrypted: string[] = []) =>
    request<{ instance_id: string; status: string }>('/process-instances', {
      method: 'POST', ...json({ process_def_id, variables, encrypted }),
    }),
  getInstance: (id: string) => request<ProcessInstance>(`/process-instances/${encode(id)}`),
  getTrace: (id: string) => request<TraceResponse>(`/process-instances/${encode(id)}/trace`),
  getHistory: async (id: string, filters?: { token_id?: string; event_type?: string }) => {
    const query = new URLSearchParams()
    if (filters?.token_id) query.set('token_id', filters.token_id)
    if (filters?.event_type) query.set('event_type', filters.event_type)
    const suffix = query.size ? `?${query}` : ''
    const response = await request<{ instance_id: string; events: Omit<HistoryEventResponse, 'instance_id'>[] }>(
      `/process-instances/${encode(id)}/history${suffix}`,
    )
    return response.events.map((event) => ({ ...event, instance_id: response.instance_id }))
  },

  listDefinitions: async () => (await request<{ definitions: DefinitionVersion[] }>('/process-definitions')).definitions,
  getDefinition: (id: string) => request<ProcessDefinitionView>(`/process-definitions/${encode(id)}`),
  deployDefinition: (xml: string) => request<{ process_definition_id: string }>('/process-definitions/deploy', {
    method: 'POST', headers: { 'Content-Type': 'application/xml' }, body: xml,
  }),
  listDefinitionVersions: (key: string) => request<{ key: string; versions: DefinitionVersion[] }>(
    `/process-definitions/versions/${encode(key)}`,
  ),
  getActiveDefinition: (key: string) => request<DefinitionVersion>(`/process-definitions/active/${encode(key)}`),
  activateDefinition: (id: string) => request<{ id: string; status: string }>(
    `/process-definitions/${encode(id)}/activate`, { method: 'PUT' },
  ),
  deprecateDefinition: (id: string) => request<{ id: string; status: string }>(
    `/process-definitions/${encode(id)}/deprecate`, { method: 'PUT' },
  ),

  listTasks: (type?: 'user' | 'external') => request<TaskListItem[]>(`/tasks${type ? `?type=${type}` : ''}`),
  getTaskForm: (taskId: string) => request<TaskForm>(`/tasks/${encode(taskId)}/form`),
  completeTask: (taskId: string, variables: Variables) => request<{ status: string }>(
    `/tasks/${encode(taskId)}/complete`, { method: 'POST', ...json({ variables }) },
  ),

  fetchAndLock: (worker_id: string, task_types: string[], max_tasks: number, lock_duration_ms: number) =>
    request<ExternalTask[]>('/external-tasks/fetch-and-lock', {
      method: 'POST', ...json({ worker_id, task_types, max_tasks, lock_duration_ms }),
    }),
  completeExternalTask: (taskId: string, worker_id: string, variables: Variables) =>
    request<{ status: string }>(`/external-tasks/${encode(taskId)}/complete`, {
      method: 'POST', ...json({ worker_id, variables }),
    }),
  failExternalTask: (taskId: string, worker_id: string, error: string, retry_after_ms?: number) =>
    request<{ status: string }>(`/external-tasks/${encode(taskId)}/fail`, {
      method: 'POST', ...json({ worker_id, error, retry_after_ms }),
    }),
  extendExternalTaskLock: (taskId: string, worker_id: string, extension_ms: number) =>
    request<{ status: string }>(`/external-tasks/${encode(taskId)}/extend-lock`, {
      method: 'POST', ...json({ worker_id, extension_ms }),
    }),

  createReplay: (instanceId: string) => request<ReplayCreateResponse>(
    `/process-instances/${encode(instanceId)}/replay`, { method: 'POST' },
  ),
  replayStep: (sessionId: string) => request<ReplayStepResponse>(
    `/replay/${encode(sessionId)}/step`, { method: 'POST' },
  ),
  replaySeek: (sessionId: string, cursor: number) => request<ReplaySeekResponse>(
    `/replay/${encode(sessionId)}/seek`, { method: 'POST', ...json({ cursor }) },
  ),
  replaySnapshot: (sessionId: string) => request<ReplaySnapshotResponse>(
    `/replay/${encode(sessionId)}/snapshot`,
  ),
  deleteReplay: (sessionId: string) => request<void>(`/replay/${encode(sessionId)}`, { method: 'DELETE' }),

  listDeadLetters: async () => (await request<{ entries: DeadLetterEntry[] }>('/dead-letters')).entries,
  getDeadLetter: async (id: string) => (await request<{ entry: DeadLetterEntry }>(`/dead-letters/${encode(id)}`)).entry,
  requeueDeadLetter: (id: string) => request<{ status: string; task_id: string }>(
    `/dead-letters/${encode(id)}/requeue`, { method: 'POST' },
  ),
  deleteDeadLetter: (id: string) => request<void>(`/dead-letters/${encode(id)}`, { method: 'DELETE' }),
}

export const fetchInstance = api.getInstance
export const fetchProcessDefinition = api.getDefinition
export const fetchHistory = api.getHistory
