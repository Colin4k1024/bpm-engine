export interface Token {
  id: string
  node_id: string
  status: string
  mode?: string
  version?: number
  attempt?: number
  parallel_group_id?: string | null
  updated_at?: string | null
}

export interface ProcessInstance {
  instance_id: string
  process_def_id: string
  status: string
  current_nodes: string[]
  tokens: Token[]
}

export interface NodeView {
  id: string
  node_type: string
}

export interface EdgeView {
  source: string
  target: string
}

export interface ProcessDefinitionView {
  id: string
  start: string
  nodes: NodeView[]
  edges: EdgeView[]
}

export interface HistoryEventPayload {
  [key: string]: unknown
}

export interface HistoryEventResponse {
  id: string
  instance_id: string
  event_type: string
  payload: HistoryEventPayload
  occurred_at: string
}
