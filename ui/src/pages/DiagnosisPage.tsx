import { useCallback, useEffect, useState } from 'react'
import { Link, useParams } from 'react-router-dom'
import { api } from '../api/client'
import type { StepsResponse } from '../api/types'
import { StepsPanel } from '../components/StepsPanel'
import { Card, EmptyState, Notice, PageHeader, Spinner } from '../components/Ui'
import { errorMessage } from '../utils'
import { useI18n } from '../i18n'

// E6-2 (issue #27): the per-instance AI diagnosis view — the E6-1 step
// timeline (`/steps`) rendered as an explainable surface: steps, worker
// lease attempts, retry counters, errors, envelope metadata (model, tools,
// cost, artifacts), waiting/approval states, plus the recovery jumps
// (error → filtered history, terminal failure → dead letters). See
// docs/ui/diagnosis_view.md.
export function DiagnosisPage() {
  const { t } = useI18n()
  const { instanceId } = useParams<{ instanceId: string }>()
  const [steps, setSteps] = useState<StepsResponse | null>(null)
  const [pendingUserTaskNodes, setPendingUserTaskNodes] = useState<Set<string>>(new Set())
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState('')

  const load = useCallback(async () => {
    if (!instanceId) return
    setLoading(true); setError('')
    try {
      // The live approval signal: the instance's WAITING user tasks. The
      // current engine does not append `UserTaskCreated` history events
      // yet, so the task inbox is the honest source for "approval
      // required" (the event check in StepsPanel covers future engines).
      const [stepsResult, tasks] = await Promise.all([api.getInstanceSteps(instanceId), api.listTasks('user')])
      setSteps(stepsResult)
      setPendingUserTaskNodes(new Set(tasks.filter((task) => task.instance_id === instanceId).map((task) => task.node_id)))
    } catch (e) { setSteps(null); setError(errorMessage(e)) } finally { setLoading(false) }
  }, [instanceId])

  useEffect(() => { void load() }, [load])

  if (!instanceId) return <Notice kind="error">{t('trace.missing')} <Link to="/instances">{t('trace.return')}</Link></Notice>

  return <>
    <div className="trace-breadcrumb">
      <Link to="/instances">{t('trace.instances')}</Link><span>/</span><code>{instanceId}</code>
      <Link className="text-link" to={`/trace/${encodeURIComponent(instanceId)}`}>{t('diagnosis.openTrace')}</Link>
    </div>
    {error && <Notice kind="error">{error}</Notice>}
    <PageHeader
      eyebrow={t('diagnosis.eyebrow')}
      title={t('diagnosis.title')}
      description={t('diagnosis.description')}
      actions={<button className="button secondary" onClick={load}>{loading ? <Spinner /> : t('common.refresh')}</button>}
    />
    <Card title={t('diagnosis.steps')} subtitle={t('diagnosis.stepsHint')}>
      {loading ? <div className="panel-placeholder">{t('component.loading')}</div>
        : steps ? <StepsPanel steps={steps} pendingUserTaskNodes={pendingUserTaskNodes} />
        : <EmptyState title={t('diagnosis.notFound')} detail={t('diagnosis.notFoundHint')} />}
    </Card>
  </>
}
