import type { PropsWithChildren, ReactNode } from 'react'
import { useI18n } from '../i18n'

export function PageHeader({ eyebrow, title, description, actions }: {
  eyebrow: string; title: string; description: string; actions?: ReactNode
}) {
  return <header className="page-header">
    <div><span className="eyebrow">{eyebrow}</span><h1>{title}</h1><p>{description}</p></div>
    {actions && <div className="page-actions">{actions}</div>}
  </header>
}

export function Card({ title, subtitle, actions, className = '', children }: PropsWithChildren<{
  title?: string; subtitle?: string; actions?: ReactNode; className?: string
}>) {
  return <section className={`card ${className}`}>
    {(title || actions) && <header className="card-header">
      <div>{title && <h2>{title}</h2>}{subtitle && <p>{subtitle}</p>}</div>{actions}
    </header>}
    {children}
  </section>
}

export function Badge({ value }: { value: string }) {
  const { locale, t } = useI18n()
  const key = `status.${value.toLowerCase()}`
  return <span className={`badge badge-${value.toLowerCase().replace(/[^a-z]+/g, '-')}`}>{locale === 'zh' ? t(key) : value}</span>
}

export function EmptyState({ title, detail }: { title: string; detail: string }) {
  return <div className="empty-state"><div className="empty-mark">○</div><strong>{title}</strong><p>{detail}</p></div>
}

export function Notice({ kind = 'info', children }: PropsWithChildren<{ kind?: 'info' | 'error' | 'success' }>) {
  return <div className={`notice notice-${kind}`}>{children}</div>
}

export function Spinner() { const { t } = useI18n(); return <span className="spinner" aria-label={t('component.loading')} /> }
