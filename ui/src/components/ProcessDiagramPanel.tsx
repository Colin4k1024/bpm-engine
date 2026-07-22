import type { ProcessDefinitionView } from '../api/types'
import type { Token } from '../api/types'
import { useTraceStore } from '../store/traceStore'
import { useI18n } from '../i18n'

interface ProcessDiagramPanelProps {
  definition: ProcessDefinitionView | null
  tokens: Token[]
  loading: boolean
}

const TOKEN_STATUS_COLOR: Record<string, string> = {
  Ready: 'var(--token-ready)',
  Executing: 'var(--token-executing)',
  Waiting: 'var(--token-waiting)',
  Completed: 'var(--token-completed)',
  Terminated: 'var(--token-terminated)',
  Created: 'var(--token-created)',
  Suspended: 'var(--token-suspended)',
  READY: 'var(--token-ready)',
  EXECUTING: 'var(--token-executing)',
  WAITING: 'var(--token-waiting)',
  COMPLETED: 'var(--token-completed)',
  TERMINATED: 'var(--token-terminated)',
  CREATED: 'var(--token-created)',
  SUSPENDED: 'var(--token-suspended)',
}

export function ProcessDiagramPanel({ definition, tokens, loading }: ProcessDiagramPanelProps) {
  const { setTokenFilter } = useTraceStore()
  const { locale, t: translate } = useI18n()

  if (loading) {
    return (
      <div className="process-diagram-panel">
        <div className="panel-placeholder">{translate('component.loadingDiagram')}</div>
      </div>
    )
  }
  if (!definition) {
    return (
      <div className="process-diagram-panel">
        <div className="panel-placeholder">{translate('component.noDefinition')}</div>
      </div>
    )
  }

  const nodeIds = new Set(definition.nodes.map((n) => n.id))
  const tokensByNode = tokens.reduce<Record<string, Token[]>>((acc, t) => {
    if (nodeIds.has(t.node_id)) {
      acc[t.node_id] = acc[t.node_id] ?? []
      acc[t.node_id].push(t)
    }
    return acc
  }, {})

  return (
    <div className="process-diagram-panel">
      <div className="diagram-graph">
        <div className="diagram-nodes">
          {definition.nodes.map((node) => {
            const nodeTokens = tokensByNode[node.id] ?? []
            const hasToken = nodeTokens.length > 0
            return (
              <div
                key={node.id}
                className={`diagram-node ${hasToken ? 'has-token' : ''}`}
                data-node-id={node.id}
              >
                <span className="node-type">{node.node_type}</span>
                <span className="node-id">{node.id}</span>
                {nodeTokens.length > 0 && (
                  <div className="token-badges">
                    {nodeTokens.map((t) => (
                      <button
                        key={t.id}
                        type="button"
                        className="token-badge"
                        style={{
                          borderColor: TOKEN_STATUS_COLOR[t.status] ?? '#888',
                        }}
                        onClick={() => setTokenFilter(t.id)}
                        title={`${t.id} (${locale === 'zh' ? translate(`status.${t.status.toLowerCase()}`) : t.status})`}
                      >
                        {t.id.slice(0, 8)} {locale === 'zh' ? translate(`status.${t.status.toLowerCase()}`) : t.status}
                      </button>
                    ))}
                  </div>
                )}
              </div>
            )
          })}
        </div>
        <div className="diagram-edges">
          {definition.edges.map((e, i) => (
            <div
              key={`${e.source}-${e.target}-${i}`}
              className="diagram-edge"
              data-source={e.source}
              data-target={e.target}
            >
              {e.source} → {e.target}
            </div>
          ))}
        </div>
      </div>
    </div>
  )
}
