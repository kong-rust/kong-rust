import type { AiModel, AiProvider } from './types'

export type ProviderType = 'openai' | 'anthropic' | 'gemini' | 'openai_compat'

export interface EndpointModelDraft {
  clientId: string
  providerMode: 'existing' | 'new'
  providerId: string
  providerName: string
  providerType: ProviderType
  endpointUrl: string
  apiKey: string
  modelName: string
  weight: number
}

export interface EndpointDraft {
  id?: string
  displayName: string
  slug: string
  enabled: boolean
  models: EndpointModelDraft[]
}

export interface GatewayService {
  id: string
  name?: string | null
  protocol: string
  host: string
  port: number
  path?: string | null
  enabled: boolean
  tags?: string[] | null
}

export interface GatewayRoute {
  id: string
  name?: string | null
  paths?: string[] | null
  methods?: string[] | null
  response_buffering: boolean
  service?: { id: string } | null
  tags?: string[] | null
}

export interface GatewayPlugin {
  id: string
  name: string
  enabled: boolean
  route?: { id: string } | null
  service?: { id: string } | null
  config: {
    model_group?: string
    model_source?: string
    route_type?: string
    client_protocol?: string
    response_streaming?: string
  }
  tags?: string[] | null
}

export interface AiEndpoint {
  id: string
  displayName: string
  slug: string
  path: string
  modelGroup: string
  enabled: boolean
  complete: boolean
  service: GatewayService
  route?: GatewayRoute
  plugin?: GatewayPlugin
  models: AiModel[]
  providers: AiProvider[]
}
