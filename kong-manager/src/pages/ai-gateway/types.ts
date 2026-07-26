export interface KongPageResponse<T> {
  data: T[]
  offset?: string | number | null
  next?: string | null
}

export interface AiProvider {
  id: string
  name: string
  provider_type: string
  endpoint_url?: string | null
  auth_config: Record<string, unknown>
  default_model?: string | null
  config: Record<string, unknown>
  enabled: boolean
  created_at?: number | null
  updated_at?: number | null
  tags?: string[] | null
}

export interface AiModelEffectivePrice {
  amount: string
  source: 'builtin' | 'override' | string
  version: string
  snapshot_date: string
  effective_from?: string | null
  effective_to?: string | null
}

export interface AiModelPricingCondition {
  type: string
  value: number | string
}

export interface AiModelEffectivePricing {
  currency: string
  unit: string
  status: 'matched' | 'unmatched' | 'unsupported' | 'not_applicable' | string
  catalog_version?: string | null
  catalog_snapshot_date?: string | null
  input: AiModelEffectivePrice | null
  output: AiModelEffectivePrice | null
  conditions: AiModelPricingCondition[]
}

export interface AiModel {
  id: string
  name: string
  provider_id: string
  model_name: string
  priority: number
  weight: number
  input_cost?: number | null
  output_cost?: number | null
  input_cost_decimal?: string | null
  output_cost_decimal?: string | null
  effective_pricing?: AiModelEffectivePricing | null
  max_tokens?: number | null
  max_input_tokens?: number | null
  config: Record<string, unknown>
  enabled: boolean
  created_at?: number | null
  updated_at?: number | null
  tags?: string[] | null
}

export interface AiModelGroup {
  name: string
}

export interface AiVirtualKey {
  id: string
  name: string
  key_prefix: string
  key?: string
  consumer_id?: string | null
  allowed_models?: string[] | null
  tpm_limit?: number | null
  rpm_limit?: number | null
  budget_limit?: number | null
  budget_used: number
  enabled: boolean
  expires_at?: number | null
  created_at?: number | null
  updated_at?: number | null
  tags?: string[] | null
}
