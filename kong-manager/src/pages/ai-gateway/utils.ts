import type { AxiosError } from 'axios'
import { formatDate } from '@/utils'

export const emptyJsonObject = '{\n  \n}'

export const stringifyJson = (value: unknown) => {
  return JSON.stringify(value ?? {}, null, 2)
}

export const parseJsonObject = (value: string, fieldName: string) => {
  const trimmed = value.trim()

  if (!trimmed) {
    return {}
  }

  let parsed: unknown
  try {
    parsed = JSON.parse(trimmed)
  } catch {
    throw new Error(`${fieldName} must be valid JSON`)
  }

  if (typeof parsed !== 'object' || parsed === null || Array.isArray(parsed)) {
    throw new Error(`${fieldName} must be a JSON object`)
  }

  return parsed as Record<string, unknown>
}

export const parseTags = (value: string) => {
  const tags = value
    .split(',')
    .map(tag => tag.trim())
    .filter(Boolean)

  return tags.length ? tags : undefined
}

export const formatTags = (tags?: string[] | null) => tags?.join(', ') ?? ''

type NumericInput = string | number | null | undefined

const normalizeNumericInput = (value: NumericInput) => {
  return typeof value === 'string' ? value.trim() : value
}

export const parseOptionalInt = (value: NumericInput, fieldName: string) => {
  const normalized = normalizeNumericInput(value)

  if (normalized === '' || normalized === null || normalized === undefined) {
    return undefined
  }

  const parsed = Number(normalized)

  if (!Number.isInteger(parsed)) {
    throw new Error(`${fieldName} must be an integer`)
  }

  return parsed
}

export const parseOptionalFloat = (value: NumericInput, fieldName: string) => {
  const normalized = normalizeNumericInput(value)

  if (normalized === '' || normalized === null || normalized === undefined) {
    return undefined
  }

  const parsed = Number(normalized)

  if (!Number.isFinite(parsed)) {
    throw new Error(`${fieldName} must be a number`)
  }

  return parsed
}

export const parseOptionalDecimal = (value: NumericInput, fieldName: string) => {
  const normalized = normalizeNumericInput(value)

  if (normalized === '' || normalized === null || normalized === undefined) {
    return undefined
  }

  const match = String(normalized).match(/^(\d+)(?:\.(\d+))?$/)
  if (!match) {
    throw new Error(`${fieldName} must be a non-negative decimal without an exponent`)
  }

  // NUMERIC(28,12) 最多容纳 16 位整数和 12 位小数。
  const integer = (match[1] ?? '0').replace(/^0+(?=\d)/, '')
  const rawFraction = match[2] ?? ''
  const significantFraction = rawFraction.replace(/0+$/, '')

  if (integer.length > 16 || significantFraction.length > 12) {
    throw new Error(`${fieldName} supports up to 16 integer digits and 12 decimal places`)
  }

  const fraction = rawFraction.slice(0, 12).replace(/0+$/, '')

  return fraction ? `${integer}.${fraction}` : integer
}

export const omitUndefined = (value: Record<string, unknown>) => {
  return Object.fromEntries(
    Object.entries(value).filter(([, entryValue]) => entryValue !== undefined),
  )
}

export const formatOptionalDate = (timestamp?: number | null) => {
  return timestamp ? formatDate(timestamp) : '-'
}

export const getErrorMessage = (err: unknown, fallback: string) => {
  const axiosError = err as AxiosError<{ message?: string }>

  return axiosError.response?.data?.message || (err instanceof Error ? err.message : fallback)
}

export const toLocalDateTimeInput = (timestamp?: number | null) => {
  if (!timestamp) {
    return ''
  }

  const date = new Date(timestamp * 1000)
  const offsetMs = date.getTimezoneOffset() * 60 * 1000

  return new Date(date.getTime() - offsetMs).toISOString().slice(0, 16)
}

export const fromLocalDateTimeInput = (value: string) => {
  if (!value) {
    return undefined
  }

  return Math.floor(new Date(value).getTime() / 1000)
}
