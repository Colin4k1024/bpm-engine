import { useCallback, useEffect, useState } from 'react'
import { api } from '../api/client'
import type { HealthResponse, InvariantCheckResult, ReadinessResponse } from '../api/types'
import { Badge, Card, EmptyState, Notice, PageHeader, Spinner } from '../components/Ui'
import { errorMessage } from '../utils'
import { useI18n } from '../i18n'

export function DiagnosticsPage() {
  const { locale, t } = useI18n()
  const componentLabel = (name: string) => locale === 'zh' ? t(`component.${name}`) : name
  const [health, setHealth] = useState<HealthResponse | null>(null)
  const [ready, setReady] = useState<ReadinessResponse | null>(null)
  const [check, setCheck] = useState<InvariantCheckResult | null>(null)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')
  const load = useCallback(async () => {
    setBusy(true); setError('')
    try { const [h, r, i] = await Promise.all([api.health(), api.readiness(), api.invariantCheck()]); setHealth(h); setReady(r); setCheck(i) } catch (e) { setError(errorMessage(e)) } finally { setBusy(false) }
  }, [])
  useEffect(() => { void load() }, [load])

  return <>
    <PageHeader eyebrow={t('diagnostics.eyebrow')} title={t('diagnostics.title')} description={t('diagnostics.description')} actions={<button className="button" onClick={load}>{busy ? <Spinner /> : t('diagnostics.run')}</button>} />
    {error && <Notice kind="error">{error}</Notice>}
    <div className="metric-grid compact"><div className="metric-card"><span>{t('diagnostics.liveness')}</span><strong>{health?.status ?? '—'}</strong><small>/health</small></div><div className="metric-card"><span>{t('diagnostics.readiness')}</span><strong>{ready?.status ?? '—'}</strong><small>/ready</small></div><div className="metric-card"><span>{t('diagnostics.result')}</span><strong>{check?.passed ? 'PASS' : 'FAIL'}</strong><small>{t('diagnostics.violations', { count: check?.violations.length ?? 0 })}</small></div><div className="metric-card"><span>{t('diagnostics.latency')}</span><strong>{check?.stats.duration_ms ?? 0} ms</strong><small>{t('diagnostics.scan')}</small></div></div>
    <div className="two-column"><Card title={t('diagnostics.readinessChecks')} subtitle={t('diagnostics.readinessHint')}><div className="check-list">{Object.entries(ready?.checks ?? {}).map(([name, status]) => <div key={name}><span>{componentLabel(name)}</span><Badge value={status} /></div>)}</div></Card><Card title={t('diagnostics.entities')} subtitle={t('diagnostics.entitiesHint')}><div className="mini-stats vertical"><div><strong>{check?.stats.instances_checked ?? 0}</strong><span>{t('home.instances')}</span></div><div><strong>{check?.stats.tokens_checked ?? 0}</strong><span>{t('instances.tokens')}</span></div><div><strong>{check?.stats.external_tasks_checked ?? 0}</strong><span>{t('diagnostics.externalTasks')}</span></div><div><strong>{check?.stats.timers_checked ?? 0}</strong><span>{t('diagnostics.timers')}</span></div></div></Card></div>
    <Card title={t('diagnostics.invariantViolations')} subtitle={t('diagnostics.invariantHint')}>{!check?.violations.length ? <EmptyState title={t('diagnostics.noViolations')} detail={t('diagnostics.noViolationsHint')} /> : <div className="table-wrap"><table><thead><tr><th>{t('diagnostics.severity')}</th><th>{t('diagnostics.invariant')}</th><th>{t('diagnostics.entity')}</th><th>{t('diagnostics.details')}</th></tr></thead><tbody>{check.violations.map((item, index) => <tr key={`${item.entity_id}-${index}`}><td><Badge value={item.severity} /></td><td>{item.invariant}</td><td><code>{item.entity_id}</code></td><td>{item.description}</td></tr>)}</tbody></table></div>}</Card>
  </>
}
