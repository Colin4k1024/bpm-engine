import { useEffect, useState, type PropsWithChildren } from 'react'
import { NavLink } from 'react-router-dom'
import { api } from '../api/client'
import { getConnectionConfig, saveConnectionConfig } from '../api/config'
import { useI18n, type Locale } from '../i18n'

const navigation = [
  ['/', 'shell.overview'],
  ['/definitions', 'shell.definitions'],
  ['/instances', 'shell.instances'],
  ['/tasks', 'shell.tasks'],
  ['/workers', 'shell.workers'],
  ['/dead-letters', 'shell.deadLetters'],
  ['/diagnostics', 'shell.diagnostics'],
] as const

export function AppShell({ children }: PropsWithChildren) {
  const { locale, setLocale, t } = useI18n()
  const initial = getConnectionConfig()
  const [apiKey, setApiKey] = useState(initial.apiKey)
  const [tenantId, setTenantId] = useState(initial.tenantId)
  const [healthy, setHealthy] = useState<boolean | null>(null)

  useEffect(() => {
    api.health().then(() => setHealthy(true)).catch(() => setHealthy(false))
  }, [])

  const save = () => {
    saveConnectionConfig(apiKey, tenantId)
    api.health().then(() => setHealthy(true)).catch(() => setHealthy(false))
  }

  return <div className="app-shell">
    <aside className="sidebar">
      <div className="brand"><span className="brand-mark">B</span><div><strong>BPM Console</strong><small>{t('shell.tagline')}</small></div></div>
      <nav>{navigation.map(([to, label]) => <NavLink key={to} to={to} end={to === '/'}>{t(label)}</NavLink>)}</nav>
      <label className="language-switcher">{t('language.label')}<select aria-label={t('language.label')} value={locale} onChange={(e) => setLocale(e.target.value as Locale)}><option value="en">{t('language.en')}</option><option value="zh">{t('language.zh')}</option></select></label>
      <div className="connection-panel">
        <div className="connection-status"><span className={`status-dot ${healthy ? 'online' : healthy === false ? 'offline' : ''}`} />{healthy ? t('shell.online') : healthy === false ? t('shell.offline') : t('shell.checking')}</div>
        <label>{t('shell.tenant')}<input value={tenantId} onChange={(e) => setTenantId(e.target.value)} placeholder={t('shell.default')} /></label>
        <label>{t('shell.apiKey')}<input type="password" value={apiKey} onChange={(e) => setApiKey(e.target.value)} placeholder={t('shell.optional')} /></label>
        <button className="button secondary full" type="button" onClick={save}>{t('shell.save')}</button>
      </div>
    </aside>
    <main className="content">{children}</main>
  </div>
}
