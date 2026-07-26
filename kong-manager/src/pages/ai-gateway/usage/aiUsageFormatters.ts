import type { AiUsageEntitySnapshot, AiUsageTokenAggregate } from './aiUsageTypes'

const groupInteger = (value: string) => value.replace(/\B(?=(\d{3})+(?!\d))/g, ',')

export const formatIntegerString = (value: string | number | null | undefined) => {
  if (value === null || value === undefined || value === '') {
    return '—'
  }

  const text = String(value)
  const match = text.match(/^(-?)(\d+)$/)

  if (!match) {
    return text
  }

  return `${match[1] ?? ''}${groupInteger(match[2] ?? '0')}`
}

export const formatDecimalString = (value: string | null | undefined) => {
  if (value === null || value === undefined || value === '') {
    return '—'
  }

  const match = value.match(/^(-?)(\d+)(?:\.(\d+))?$/)

  if (!match) {
    return value
  }

  const [, sign = '', integer = '0', fraction = ''] = match
  const trimmedFraction = fraction.replace(/0+$/, '')

  return `${sign}${groupInteger(integer)}${trimmedFraction ? `.${trimmedFraction}` : ''}`
}

export const formatUsd = (value: string | null | undefined) => {
  const formatted = formatDecimalString(value)

  return formatted === '—' ? formatted : `$${formatted}`
}

export const formatCoverage = (value: string | null | undefined) => {
  if (value === null || value === undefined || value === '') {
    return '—'
  }

  const match = value.match(/^(\d+)(?:\.(\d+))?$/)
  if (!match) {
    return value
  }

  const [, integer = '0', fraction = ''] = match
  const paddedFraction = fraction.padEnd(4, '0')
  const whole = Number(integer) * 100 + Number(paddedFraction.slice(0, 2))
  const decimals = paddedFraction.slice(2, 4).replace(/0+$/, '')

  return `${whole}${decimals ? `.${decimals}` : ''}%`
}

export const formatTokenAggregate = (aggregate: AiUsageTokenAggregate) => (
  formatIntegerString(aggregate.known_sum)
)

export const formatLatency = (value: string | number | null | undefined) => {
  if (value === null || value === undefined || value === '') {
    return '—'
  }

  return `${formatDecimalString(String(value))} ms`
}

export const formatTimestamp = (
  value: string | null | undefined,
  timezone?: string,
) => {
  if (!value) {
    return '—'
  }

  const date = new Date(value)
  if (Number.isNaN(date.getTime())) {
    return value
  }

  try {
    return new Intl.DateTimeFormat(undefined, {
      dateStyle: 'medium',
      timeStyle: 'medium',
      ...(timezone ? { timeZone: timezone } : {}),
    }).format(date)
  } catch {
    return date.toLocaleString()
  }
}

export const snapshotLabel = (snapshot: AiUsageEntitySnapshot | null | undefined) => {
  if (!snapshot) {
    return '—'
  }

  return snapshot.name || snapshot.id || '—'
}

export const compactReasons = (reasons: string[] | null | undefined) => (
  reasons?.length ? reasons.join(', ') : '—'
)

export const statusLabel = (value: string) => value
  .split('_')
  .map(part => part.charAt(0).toUpperCase() + part.slice(1))
  .join(' ')
