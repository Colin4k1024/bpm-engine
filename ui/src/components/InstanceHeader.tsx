import type { ReactNode } from 'react'
import type { ProcessInstance } from '../api/types'
import { useI18n } from '../i18n'

interface InstanceHeaderProps {
  instance: ProcessInstance | null
  loading: boolean
  onRefresh: () => void
  /** Extra controls rendered next to the refresh button (e.g. the E6-2
   * diagnosis-view link on the trace page). */
  actions?: ReactNode
}

export function InstanceHeader({ instance, loading, onRefresh, actions }: InstanceHeaderProps) {
  const { locale, t } = useI18n()
  if (loading) {
    return (
      <header className="instance-header">
        <span>{t('component.loading')}</span>
      </header>
    )
  }
  if (!instance) {
    return (
      <header className="instance-header">
        <span>{t('component.instanceNotFound')}</span>
      </header>
    )
  }
  const copyId = () => {
    navigator.clipboard.writeText(instance.instance_id)
  }
  return (
    <header className="instance-header">
      <div className="instance-header-main">
        <span className="process-name">{instance.process_def_id}</span>
        <span className="instance-id" title={instance.instance_id} onClick={copyId}>
          #{instance.instance_id.slice(0, 8)}...
        </span>
        <span className={`status-badge status-${instance.status.toLowerCase()}`}>
          {locale === 'zh' ? t(`status.${instance.status.toLowerCase()}`) : instance.status}
        </span>
      </div>
      <div className="instance-header-actions">
        {actions}
        <button type="button" onClick={onRefresh} disabled={loading}>
          {t('common.refresh')}
        </button>
      </div>
    </header>
  )
}
