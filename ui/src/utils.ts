import type { Variables } from './api/types'

export function parseVariables(value: string): Variables {
  if (!value.trim()) return {}
  const parsed: unknown = JSON.parse(value)
  if (!parsed || Array.isArray(parsed) || typeof parsed !== 'object') {
    throw new Error('Variables must be a JSON object')
  }
  return Object.fromEntries(Object.entries(parsed).map(([key, item]) => [key, String(item)]))
}

export function formatTimestamp(value: string) {
  const numeric = Number(value)
  const date = Number.isFinite(numeric) ? new Date(numeric * 1000) : new Date(value)
  return Number.isNaN(date.getTime()) ? value : date.toLocaleString()
}

export function errorMessage(error: unknown) {
  return error instanceof Error ? error.message : String(error)
}
