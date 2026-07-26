export type AiUsageRangePreset = '24h' | '7d' | '30d' | 'custom'
export type AiUsageMetric = 'cost' | 'tokens'
export type AiUsageTokenMetric = 'prompt' | 'completion' | 'total'
export type AiUsageOutcome =
  | 'success'
  | 'gateway_rejected'
  | 'gateway_error'
  | 'upstream_error'
  | 'client_disconnected'
  | 'stream_interrupted'
export type AiUsageSource = 'provider' | 'estimated' | 'mixed' | 'unavailable'
export type AiPricingStatus = 'matched' | 'unmatched' | 'unsupported' | 'not_applicable'
export type AiCostStatus = 'calculated' | 'estimated' | 'not_incurred' | 'unavailable'
export type AiCacheStatus = 'not_configured' | 'unavailable' | 'bypass' | 'miss' | 'hit'
export type AiUsageBreakdownType =
  | 'hour'
  | 'day'
  | 'provider'
  | 'actual_model'
  | 'model_group'
  | 'virtual_key'
  | 'route'
  | 'service'

export interface AiUsageFilters {
  range: AiUsageRangePreset
  start: string
  end: string
  timezone: string
  metric: AiUsageMetric
  tokenMetric: AiUsageTokenMetric
  request_id: string
  route_id: string
  service_id: string
  provider_id: string
  provider_type: string
  requested_model: string
  model_group: string
  actual_model: string
  virtual_key_id: string
  consumer_id: string
  status_code: string
  outcome: string
  stream: string
  cache_status: string
  usage_source: string
  pricing_status: string
  cost_status: string
}

export type AiUsageApiFilterKey =
  | 'request_id'
  | 'route_id'
  | 'service_id'
  | 'provider_id'
  | 'provider_type'
  | 'requested_model'
  | 'model_group'
  | 'actual_model'
  | 'virtual_key_id'
  | 'consumer_id'
  | 'status_code'
  | 'outcome'
  | 'stream'
  | 'cache_status'
  | 'usage_source'
  | 'pricing_status'
  | 'cost_status'

export const aiUsageApiFilterKeys: AiUsageApiFilterKey[] = [
  'request_id',
  'route_id',
  'service_id',
  'provider_id',
  'provider_type',
  'requested_model',
  'model_group',
  'actual_model',
  'virtual_key_id',
  'consumer_id',
  'status_code',
  'outcome',
  'stream',
  'cache_status',
  'usage_source',
  'pricing_status',
  'cost_status',
]

export interface AiUsageMeta {
  mode: 'postgres' | 'dbless' | 'hybrid' | string
  ephemeral: boolean
  node_id: string | null
  capacity: number | null
  earliest_available_at: string | null
  restart_clears: boolean
}

export interface AiUsageEntitySnapshot {
  id: string | null
  name: string | null
}

export interface AiUsageProviderSnapshot extends AiUsageEntitySnapshot {
  type: string | null
}

export interface AiUsageVirtualKeySnapshot extends AiUsageEntitySnapshot {
  prefix: string | null
}

export interface AiUsageModelSnapshot {
  id: string | null
  requested: string | null
  group: string | null
  actual: string | null
}

export interface AiUsagePriceDirection {
  usd_per_million: string
  source: 'builtin' | 'override' | string
  version: string
  snapshot_date: string
  effective_from: string
  effective_to: string | null
}

export interface AiUsageFact {
  id: string
  request_id: string
  started_at: string
  finished_at: string
  gateway: {
    route: AiUsageEntitySnapshot | null
    service: AiUsageEntitySnapshot | null
  }
  ai: {
    provider: AiUsageProviderSnapshot | null
    model: AiUsageModelSnapshot | null
    attempt_count: number
  }
  identity: {
    virtual_key: AiUsageVirtualKeySnapshot | null
    consumer_id: string | null
  }
  usage: {
    prompt_tokens: number | null
    completion_tokens: number | null
    total_tokens: number | null
    prompt_source: AiUsageSource | null
    completion_source: AiUsageSource | null
    total_source: AiUsageSource | null
    reasoning_tokens: number | null
    cache_read_input_tokens: number | null
    cache_write_input_tokens: number | null
    source: AiUsageSource
    unavailable_reasons: string[]
  }
  pricing: {
    status: AiPricingStatus
    currency: 'USD'
    input: AiUsagePriceDirection | null
    output: AiUsagePriceDirection | null
    unsupported_reasons: string[]
  }
  cost: {
    usd: string | null
    status: AiCostStatus
    unavailable_reasons: string[]
  }
  result: {
    status_code: number | null
    upstream_status_code: number | null
    outcome: AiUsageOutcome
    e2e_ms: number
    ttft_ms: number | null
    upstream_attempted: boolean
    stream: boolean | null
    cache_status: AiCacheStatus
  }
}

export interface AiUsagePage {
  data: AiUsageFact[]
  offset: string | null
  next: string | null
  snapshot: string
  meta: AiUsageMeta
}

export interface AiUsageTokenAggregate {
  known_sum: string
  known_requests: number
  unknown_requests: number
  coverage: string | null
}

export interface AiUsageOutcomeCounts {
  success: number
  gateway_rejected: number
  gateway_error: number
  upstream_error: number
  client_disconnected: number
  stream_interrupted: number
}

export interface AiPricingStatusCounts {
  matched: number
  unmatched: number
  unsupported: number
  not_applicable: number
}

export interface AiCostStatusCounts {
  calculated: number
  estimated: number
  not_incurred: number
  unavailable: number
}

export interface AiUsageAggregateMetrics {
  requests: number
  successful_requests: number
  failed_requests: number
  outcomes: AiUsageOutcomeCounts
  prompt_tokens: AiUsageTokenAggregate
  completion_tokens: AiUsageTokenAggregate
  total_tokens: AiUsageTokenAggregate
  cost_usd_calculable_sum: string
  pricing_status: AiPricingStatusCounts
  cost_status: AiCostStatusCounts
  estimated_usage_ratio: string | null
  pricing_coverage: string | null
  cost_calculable_coverage: string | null
  avg_e2e_ms: string | null
  p95_e2e_ms: string | null
  avg_ttft_ms: string | null
  cache_hits: number
}

export interface AiUsageBreakdownDimension {
  id: string | null
  name: string | null
  type: string | null
  prefix: string | null
}

export interface AiUsageBreakdownItem {
  key: string | null
  label: string | null
  is_other: boolean
  bucket_start: string | null
  bucket_end: string | null
  dimension: AiUsageBreakdownDimension | null
  metrics: AiUsageAggregateMetrics
}

export interface AiUsageBreakdown {
  type: AiUsageBreakdownType
  timezone: string | null
  order_by: 'cost_usd' | 'total_tokens' | 'requests' | null
  limit: number | null
  items: AiUsageBreakdownItem[]
  other: AiUsageBreakdownItem | null
}

export interface AiUsageSummary {
  snapshot: string
  meta: AiUsageMeta
  totals: AiUsageAggregateMetrics
  breakdown: AiUsageBreakdown | null
}

export interface AiUsageApiErrorBody {
  message?: string
  error_code?: string
  fields?: Record<string, unknown>
}

export type AiUsageViewState =
  | 'loading'
  | 'ready'
  | 'empty-window'
  | 'empty-filter'
  | 'snapshot-expired'
  | 'unsupported'
  | 'error'
