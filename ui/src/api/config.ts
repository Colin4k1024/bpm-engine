const API_KEY_STORAGE = 'bpm-console-api-key'
const TENANT_STORAGE = 'bpm-console-tenant'

export function getConnectionConfig() {
  return {
    apiKey: localStorage.getItem(API_KEY_STORAGE) ?? '',
    tenantId: localStorage.getItem(TENANT_STORAGE) ?? '',
  }
}

export function saveConnectionConfig(apiKey: string, tenantId: string) {
  localStorage.setItem(API_KEY_STORAGE, apiKey.trim())
  localStorage.setItem(TENANT_STORAGE, tenantId.trim())
  window.dispatchEvent(new Event('bpm-config-changed'))
}
