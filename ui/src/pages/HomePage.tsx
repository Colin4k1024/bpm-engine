import { useCallback, useEffect, useState } from 'react'
import { Link } from 'react-router-dom'
import { api } from '../api/client'
import type { InvariantCheckResult, ReadinessResponse } from '../api/types'
import { Card, Notice, PageHeader, Spinner } from '../components/Ui'
import { errorMessage } from '../utils'
import { useI18n } from '../i18n'

export function HomePage() {
  const { locale, t } = useI18n()
  const statusLabel = (status: string) => locale === 'zh' ? t(`status.${status.toLowerCase()}`) : status
  const componentLabel = (name: string) => locale === 'zh' ? t(`component.${name}`) : name
  const [counts, setCounts] = useState({ definitions: 0, instances: 0, tasks: 0, deadLetters: 0 })
  const [readiness, setReadiness] = useState<ReadinessResponse | null>(null)
  const [invariants, setInvariants] = useState<InvariantCheckResult | null>(null)
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState('')

  const load = useCallback(async () => {
    setLoading(true); setError('')
    try {
      const [definitions, instances, tasks, deadLetters, ready, check] = await Promise.all([
        api.listDefinitions(), api.listInstances(), api.listTasks(), api.listDeadLetters(), api.readiness(), api.invariantCheck(),
      ])
      setCounts({ definitions: definitions.length, instances: instances.length, tasks: tasks.length, deadLetters: deadLetters.length })
      setReadiness(ready); setInvariants(check)
    } catch (e) { setError(errorMessage(e)) } finally { setLoading(false) }
  }, [])

  useEffect(() => { void load() }, [load])

  return <>
    <PageHeader eyebrow={t('home.eyebrow')} title={t('home.title')} description={t('home.description')} actions={<button className="button" onClick={load}>{loading ? <Spinner /> : t('home.refresh')}</button>} />
    {error && <Notice kind="error">{error}</Notice>}
    <div className="metric-grid">
      <Link className="metric-card" to="/definitions"><span>{t('home.definitions')}</span><strong>{counts.definitions}</strong><small>{t('home.definitionHint')}</small></Link>
      <Link className="metric-card" to="/instances"><span>{t('home.instances')}</span><strong>{counts.instances}</strong><small>{t('home.instanceHint')}</small></Link>
      <Link className="metric-card" to="/tasks"><span>{t('home.openTasks')}</span><strong>{counts.tasks}</strong><small>{t('home.taskHint')}</small></Link>
      <Link className="metric-card danger-accent" to="/dead-letters"><span>{t('home.deadLetters')}</span><strong>{counts.deadLetters}</strong><small>{t('home.deadHint')}</small></Link>
    </div>
    <div className="two-column">
      <Card title={t('home.readiness')} subtitle={t('home.readinessHint')}>
        <div className="health-summary"><span className={`health-orb ${readiness?.status === 'ok' ? 'healthy' : ''}`} /><div><strong>{readiness?.status ? statusLabel(readiness.status) : t('home.unknown')}</strong><span>{t('home.serviceState')}</span></div></div>
        <div className="check-list">{Object.entries(readiness?.checks ?? {}).map(([name, status]) => <div key={name}><span>{componentLabel(name)}</span><b className={status === 'ok' ? 'text-success' : 'text-danger'}>{statusLabel(status)}</b></div>)}</div>
      </Card>
      <Card title={t('home.invariants')} subtitle={t('home.invariantsHint')}>
        <div className="health-summary"><span className={`health-orb ${invariants?.passed ? 'healthy' : 'failed'}`} /><div><strong>{invariants?.passed ? t('home.allPassed') : t('home.violations', { count: invariants?.violations.length ?? 0 })}</strong><span>{t('home.validationTime', { ms: invariants?.stats.duration_ms ?? 0 })}</span></div></div>
        <div className="mini-stats"><div><strong>{invariants?.stats.instances_checked ?? 0}</strong><span>{t('home.instances')}</span></div><div><strong>{invariants?.stats.tokens_checked ?? 0}</strong><span>{t('instances.tokens')}</span></div><div><strong>{invariants?.stats.external_tasks_checked ?? 0}</strong><span>{t('home.external')}</span></div></div>
      </Card>
    </div>
    <Card title={t('home.quick')} subtitle={t('home.quickHint')}>
      <div className="quick-actions"><Link to="/definitions" className="action-tile"><strong>{t('home.deploy')}</strong><span>{t('home.deployHint')}</span></Link><Link to="/instances" className="action-tile"><strong>{t('home.start')}</strong><span>{t('home.startHint')}</span></Link><Link to="/workers" className="action-tile"><strong>{t('home.claim')}</strong><span>{t('home.claimHint')}</span></Link></div>
    </Card>
  </>
}
