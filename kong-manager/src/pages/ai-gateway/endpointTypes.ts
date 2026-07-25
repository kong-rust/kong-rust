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
  /** Require a virtual key on this endpoint — 该接口是否要求虚拟密钥 */
  requireAuth: boolean
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
    key_header?: string
    error_format?: string
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
  /** Virtual key authentication is enforced on this endpoint — 该接口已启用虚拟密钥认证 */
  requireAuth: boolean
  service: GatewayService
  route?: GatewayRoute
  plugin?: GatewayPlugin
  /** ai-key-auth plugin, present when authentication is enabled — 启用认证时存在的 ai-key-auth 插件 */
  authPlugin?: GatewayPlugin
  models: AiModel[]
  providers: AiProvider[]
}
