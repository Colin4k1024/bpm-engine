import type { ProcessInstance } from '../api/types'

interface InstanceHeaderProps {
  instance: ProcessInstance | null
  loading: boolean
  onRefresh: () => void
}

export function InstanceHeader({ instance, loading, onRefresh }: InstanceHeaderProps) {
  if (loading) {
    return (
      <header className="instance-header">
        <span>Loading...</span>
      </header>
    )
  }
  if (!instance) {
    return (
      <header className="instance-header">
        <span>Instance not found</span>
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
          {instance.status}
        </span>
      </div>
      <div className="instance-header-actions">
        <button type="button" onClick={onRefresh} disabled={loading}>
          Refresh
        </button>
      </div>
    </header>
  )
}
