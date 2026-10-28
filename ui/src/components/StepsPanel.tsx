import { useState } from 'react'
import { Link } from 'react-router-dom'
import type { StepAttemptView, StepArtifactRef, StepEnvelopeSummary, StepView, StepsResponse } from '../api/types'
import { Badge } from './Ui'
import { useI18n } from '../i18n'

// The E6-2 diagnosis view body: renders the E6-1 step timeline
// (`GET /api/v1/process-instances/:id/steps`) — steps with kind badges,
// worker lease attempts with outcome badges/timings/retries/errors, the
// AI envelope metadata (model, tools, cost, artifacts), waiting and
// approval states, and the two recovery jumps: error → instance history
// (token/event-type filtered) and terminal failure → dead letters.

/** Step-level lifecycle status. An approval is pending when the step's
 * node has a WAITING user task (the live task inbox — this engine build
 * does not append `UserTaskCreated` history events, so the inbox is the
 * honest signal; the event check covers future engines) or when a
 * user-task-created event has no completion yet. */
type StepStatus = 'completed' | 'failed' | 'waiting' | 'in_flight' | 'approval-required'

function stepStatus(step: StepView, pendingUserTaskNodes: ReadonlySet<string>): StepStatus {
  const types = step.events.map((event) => event.event_type)
  if (types.includes('TokenFailed')) return 'failed'
  if (types.includes('UserTaskCreated') && !types.includes('UserTaskCompleted')) return 'approval-required'
  if (pendingUserTaskNodes.has(step.node_id)) return 'approval-required'
  if (step.attempts?.some((attempt) => attempt.outcome === 'in_flight')) return 'in_flight'
  if (types.includes('TokenCompleted')) return 'completed'
  return 'waiting'
}

/** A failed attempt that terminally exhausted its recovery options: the
 * retry counter reached zero (legacy exhaustion) or the worker reported a
 * `permanent` retry category (E3-2 fast path) — both route the task to
 * the dead-letter queue. */
function attemptWentToDeadLetters(attempt: StepAttemptView): boolean {
  if (attempt.outcome !== 'failed') return false
  if (attempt.retries_remaining === 0) return true
  if (attempt.envelope?.retry_category === 'permanent') return true
  // Best-effort legacy fallback: E3-1's v1 recorded the category as a JSON
  // prefix on the persisted error message. Coupled to that format on purpose;
  // when the server grows a first-class field, drop this substring probe.
  return attempt.error?.includes('"retry_category":"permanent"') ?? false
}

/** Deep link into the instance history view (TracePage) with the
 * token/event-type filters the backend history endpoint supports. */
function historyLink(instanceId: string, tokenId: string, eventType: string): string {
  const query = new URLSearchParams()
  if (tokenId) query.set('token_id', tokenId)
  if (eventType) query.set('event_type', eventType)
  return `/trace/${encodeURIComponent(instanceId)}?${query}`
}

/** Micro-unit integer → human amount (never a raw float of money). */
function formatAmount(micros: number): string {
  return String(Number((micros / 1_000_000).toFixed(6)))
}

function truncate(value: string, max: number): string {
  return value.length > max ? `${value.slice(0, max - 1)}…` : value
}

function KindBadge({ kind }: { kind: string }) {
  const { t } = useI18n()
  const key = `diagnosis.kind.${kind}`
  // Fall back to the raw kind value for unknown/backward-compat kinds
  // (never leak the lookup key itself onto the screen).
  const label = t(key)
  return <span className={`badge badge-kind badge-${kind.replace(/_/g, '-')}`}>{label === key ? kind : label}</span>
}

function ArtifactRefView({ artifact }: { artifact: StepArtifactRef }) {
  const { t, formatNumber } = useI18n()
  const [copied, setCopied] = useState(false)
  const copy = async () => {
    try {
      await navigator.clipboard.writeText(artifact.uri)
      setCopied(true)
      setTimeout(() => setCopied(false), 1500)
    } catch { /* clipboard unavailable — the inline code still shows the URI */ }
  }
  return <span className="artifact-ref">
    <code title={artifact.uri}>{artifact.uri}</code>
    <button type="button" className="copy-button" onClick={() => void copy()} aria-label={t('common.copy')} title={t('common.copy')}>{copied ? '✓' : '⧉'}</button>
    {artifact.size != null && <small title={t('diagnosis.artifactSize')}>{formatNumber(artifact.size)} B</small>}
    {artifact.media_type && <small>{artifact.media_type}</small>}
    {artifact.sha256 && <small title={artifact.sha256}>{artifact.sha256.slice(0, 10)}…</small>}
  </span>
}

function EnvelopeView({ envelope, title }: { envelope: StepEnvelopeSummary; title: string }) {
  const { t, formatNumber } = useI18n()
  if (envelope.opaque) {
    return <div className="envelope-meta"><span className="envelope-opaque">{title} · {t('diagnosis.opaqueEnvelope')}</span></div>
  }
  const cost = envelope.cost
  return <div className="envelope-meta">
    <span className="envelope-title">{title}</span>
    {envelope.model && <span>{t('diagnosis.model')}: <strong>{envelope.model.provider}/{envelope.model.name}{envelope.model.version ? ` (${envelope.model.version})` : ''}</strong></span>}
    {envelope.tools && envelope.tools.length > 0 && <span>{t('diagnosis.tools')}: {envelope.tools.map((tool) => `${tool.name}${tool.version ? `@${tool.version}` : ''}`).join(', ')}</span>}
    {cost && <span>{t('diagnosis.cost')}: {cost.amount_micros != null && <strong>{cost.currency ? `${cost.currency} ${formatAmount(cost.amount_micros)}` : formatNumber(cost.amount_micros)}</strong>}
      {(cost.tokens_in != null || cost.tokens_out != null) && <> · {t('diagnosis.tokens', { in: cost.tokens_in ?? 0, out: cost.tokens_out ?? 0 })}</>}
      {cost.duration_ms != null && <> · {formatNumber(cost.duration_ms)} ms</>}</span>}
    {envelope.execution_id && <span>{t('diagnosis.executionId')}: <code>{envelope.execution_id}</code></span>}
    {envelope.retry_category && <span>{t('diagnosis.retryCategory')}: <code>{envelope.retry_category}</code></span>}
    {envelope.artifacts && envelope.artifacts.length > 0 && <span className="artifact-list">{envelope.artifacts.map((artifact) => <ArtifactRefView key={artifact.uri} artifact={artifact} />)}</span>}
  </div>
}

function AttemptView({ instanceId, step, attempt, index }: {
  instanceId: string
  step: StepView
  attempt: StepAttemptView
  index: number
}) {
  const { t, formatDate } = useI18n()
  const started = Number(attempt.locked_at)
  const finished = attempt.completed_or_failed != null ? Number(attempt.completed_or_failed) : null
  const duration = finished != null && Number.isFinite(started) && Number.isFinite(finished) ? finished - started : null
  const deadLettered = attemptWentToDeadLetters(attempt)
  return <div className={`attempt attempt-${attempt.outcome}`}>
    <div className="attempt-main">
      <span>#{index + 1} <code>{attempt.worker_id}</code></span>
      <Badge value={attempt.outcome} />
      <span className="attempt-times">
        {t('diagnosis.lockedAt')} {formatDate(attempt.locked_at)}
        {attempt.completed_or_failed != null && <> → {formatDate(attempt.completed_or_failed)}</>}
        {duration != null && <> · {t('diagnosis.duration', { count: duration })}</>}
      </span>
      {attempt.retries_remaining != null && <span className="attempt-retries">{t('diagnosis.retriesRemaining')}: {attempt.retries_remaining}</span>}
      {attempt.outcome === 'in_flight' && <span className="attempt-note">{t('diagnosis.inFlightNote')}</span>}
      {attempt.outcome === 'expired' && <span className="attempt-note">{t('diagnosis.expiredNote')}</span>}
    </div>
    {attempt.error && <code className="attempt-error" title={attempt.error}>{attempt.error}</code>}
    {attempt.envelope && <EnvelopeView envelope={attempt.envelope} title={t('diagnosis.envelope')} />}
    {attempt.outcome === 'failed' && <div className="attempt-actions">
      <Link className="text-link" to={historyLink(instanceId, step.token_id, 'ExternalTaskFailed')}>{t('diagnosis.viewHistory')}</Link>
      {deadLettered && <Link className="text-link" to={`/dead-letters?instance_id=${encodeURIComponent(instanceId)}`}>{t('diagnosis.viewDeadLetters')}</Link>}
    </div>}
  </div>
}

function StepItem({ instanceId, step, pendingUserTaskNodes }: { instanceId: string; step: StepView; pendingUserTaskNodes: ReadonlySet<string> }) {
  const { t, formatDate } = useI18n()
  const [expanded, setExpanded] = useState(false)
  const status = stepStatus(step, pendingUserTaskNodes)
  // A step whose last failed attempt exhausted its retries also jumped to
  // the dead-letter queue: offer the jump at step level too.
  const stepDeadLettered = step.attempts?.some((attempt) => attemptWentToDeadLetters(attempt)) ?? false
  return <li className={`step-item step-${status}`}>
    <div className="step-header">
      <span className="step-no">#{step.step_no}</span>
      <strong className="step-node" title={step.node_id}>{step.node_id}</strong>
      <KindBadge kind={step.kind} />
      <Badge value={status} />
      {step.token_id && <span className="step-token" title={step.token_id}><code>{truncate(step.token_id, 14)}</code></span>}
      <button type="button" className="step-events-toggle" onClick={() => setExpanded(!expanded)}>
        {t('diagnosis.eventsCount', { count: step.events.length })} {expanded ? '▾' : '▸'}
      </button>
    </div>
    {expanded && <div className="step-events">
      {step.events.map((event, i) => <span key={`${event.event_type}-${i}`}>{formatDate(event.occurred_at)} · {event.event_type}</span>)}
    </div>}
    {step.attempts && step.attempts.length > 0 && <div className="step-attempts">
      {step.attempts.map((attempt, index) => <AttemptView key={`${attempt.worker_id}-${attempt.locked_at}-${index}`} instanceId={instanceId} step={step} attempt={attempt} index={index} />)}
    </div>}
    {/* Step-level envelope: the LAST parsable envelope of the step — render
        it only when no attempt already carries one (otherwise it is a
        duplicate of the latest attempt's metadata). */}
    {step.envelope_summary && !(step.attempts?.some((attempt) => attempt.envelope) ?? false) && (
      <EnvelopeView envelope={step.envelope_summary} title={t('diagnosis.envelopeSummary')} />
    )}
    {step.variables_touched && step.variables_touched.length > 0 && <div className="variable-chips">
      {step.variables_touched.map((name) => <span key={name}>{name}</span>)}
    </div>}
    {stepDeadLettered && <div className="attempt-actions">
      <Link className="text-link" to={`/dead-letters?instance_id=${encodeURIComponent(instanceId)}`}>{t('diagnosis.viewDeadLetters')}</Link>
    </div>}
  </li>
}

export function StepsPanel({ steps, pendingUserTaskNodes = new Set<string>() }: {
  steps: StepsResponse
  pendingUserTaskNodes?: ReadonlySet<string>
}) {
  const { t } = useI18n()
  return <div className="steps-panel">
    <div className="detail-grid steps-summary">
      <div><span>{t('instances.definitionColumn')}</span><strong>{steps.process_def_id}</strong></div>
      <div><span>{t('common.status')}</span><Badge value={steps.state} /></div>
      <div><span>{t('diagnosis.traceId')}</span><strong title={steps.trace_id}>{truncate(steps.trace_id, 18)}</strong></div>
      <div><span>{t('diagnosis.stepsCount')}</span><strong>{steps.steps.length}</strong></div>
    </div>
    {steps.steps.length === 0
      ? <div className="timeline-empty">{t('diagnosis.empty')}</div>
      : <ol className="step-list">
          {steps.steps.map((step) => <StepItem key={`${step.step_no}-${step.node_id}-${step.token_id}`} instanceId={steps.instance_id} step={step} pendingUserTaskNodes={pendingUserTaskNodes} />)}
        </ol>}
  </div>
}
