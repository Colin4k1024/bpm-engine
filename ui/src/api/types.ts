export type Variables = Record<string, string>

export interface Token {
  id: string
  node_id: string
  status: string
  mode: string
  version: number
  attempt: number
  parallel_group_id?: string | null
  updated_at?: string | null
}

export interface ProcessInstance {
  instance_id: string
  process_def_id: string
  status: string
  current_nodes: string[]
  tokens: Token[]
  variables?: Variables
}

export interface NodeView { id: string; node_type: string }
export interface EdgeView { source: string; target: string }
export interface ProcessDefinitionView {
  id: string
  start: string
  nodes: NodeView[]
  edges: EdgeView[]
}

export interface DefinitionVersion {
  id: string
  key: string
  version: number
  status: string
  created_at: string
}

export interface HistoryEventResponse {
  sequence: number
  id: string
  instance_id: string
  event_type: string
  category: string
  payload: Record<string, unknown>
  occurred_at: string
}

export interface TraceEvent {
  event_type: string
  occurred_at: string
  payload?: Record<string, unknown>
}

export interface TokenTimeline {
  token_id: string
  node_id: string
  status: string
  events: TraceEvent[]
}

export interface ExternalTaskHistoryEntry {
  task_id: string
  token_id: string
  process_instance_id: string
  events: TraceEvent[]
}

export interface TraceResponse {
  instance: ProcessInstance
  token_timelines: TokenTimeline[]
  external_task_history: ExternalTaskHistoryEntry[]
}

export interface TaskListItem {
  task_id: string
  node_id: string
  instance_id: string
  task_type: 'user' | 'external'
}

export interface FormField {
  id: string
  label: string
  type: 'string' | 'number' | 'boolean' | 'enum'
  required: boolean
  default_value?: string
  options?: string[]
}

export interface TaskForm {
  task_id: string
  node_id: string
  form_key?: string
  fields?: FormField[]
}

export interface ExternalTask {
  task_id: string
  token_id: string
  process_instance_id: string
  task_type: string
  variables: Variables
}

export interface ReplayToken { token_id: string; node_id: string; state: string }
export interface ReplaySnapshot { completed: boolean; tokens: ReplayToken[] }
export interface ReplayCreateResponse { session_id: string; instance_id: string; total_events: number }
export interface ReplayStepResponse {
  cursor: number
  event: { event_type: string; occurred_at: string; token_id?: string; node_id?: string }
  snapshot: ReplaySnapshot
}
export interface ReplaySeekResponse { cursor: number; snapshot: ReplaySnapshot }
export interface ReplaySnapshotResponse extends ReplaySnapshot { cursor: number; total_events: number }

export interface DeadLetterEntry {
  id: string
  task_id: string
  token_id: string
  process_instance_id: string
  task_type: string
  error_message: string
  variables: string
  tenant_id?: string | null
  created_at: string
}

export interface InvariantViolation {
  invariant: string
  description: string
  entity_id: string
  severity: string
}

export interface InvariantCheckResult {
  passed: boolean
  violations: InvariantViolation[]
  stats: {
    instances_checked: number
    tokens_checked: number
    external_tasks_checked: number
    timers_checked: number
    duration_ms: number
  }
}

export interface HealthResponse { status: string }
export interface ReadinessResponse { status: string; checks: Record<string, string> }
