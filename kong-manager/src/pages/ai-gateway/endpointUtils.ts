import type {
  AiEndpoint,
  EndpointDraft,
  EndpointModelDraft,
  ProviderType,
} from './endpointTypes'

export const endpointManagedTag = 'kr-ai-endpoint-v1'
export const endpointIdTagPrefix = 'kr-ai-endpoint:'
export const endpointNameTagPrefix = 'kr-ai-name:'

export const providerLabels: Record<ProviderType, string> = {
  openai: 'OpenAI',
  anthropic: 'Anthropic',
  gemini: 'Google Gemini',
  openai_compat: 'OpenAI Compatible',
}

export const providerDefaultEndpoints: Record<Exclude<ProviderType, 'openai_compat'>, string> = {
  openai: 'https://api.openai.com',
  anthropic: 'https://api.anthropic.com',
  gemini: 'https://generativelanguage.googleapis.com',
}

const bytesToBase64Url = (bytes: Uint8Array) => {
  let binary = ''

  for (const byte of bytes) {
    binary += String.fromCharCode(byte)
  }

  return btoa(binary)
    .replace(/\+/g, '-')
    .replace(/\//g, '_')
    .replace(/=+$/, '')
}

const base64UrlToBytes = (value: string) => {
  const base64 = value.replace(/-/g, '+').replace(/_/g, '/')
  const padded = base64.padEnd(Math.ceil(base64.length / 4) * 4, '=')
  const binary = atob(padded)

  return Uint8Array.from(binary, char => char.charCodeAt(0))
}

export const endpointNameTag = (name: string) => {
  return `${endpointNameTagPrefix}${bytesToBase64Url(new TextEncoder().encode(name))}`
}

export const endpointDisplayName = (tags: string[] | null | undefined, fallback: string) => {
  const encoded = tags?.find(tag => tag.startsWith(endpointNameTagPrefix))
    ?.slice(endpointNameTagPrefix.length)

  if (!encoded) {
    return fallback
  }

  try {
    return new TextDecoder().decode(base64UrlToBytes(encoded))
  } catch {
    return fallback
  }
}

export const endpointIdFromTags = (tags?: string[] | null) => {
  return tags?.find(tag => tag.startsWith(endpointIdTagPrefix))
    ?.slice(endpointIdTagPrefix.length)
}

export const endpointTags = (id: string, displayName: string) => [
  endpointManagedTag,
  `${endpointIdTagPrefix}${id}`,
  endpointNameTag(displayName),
]

export const endpointPath = (slug: string) => `/ai/${slug}/v1/chat/completions`

export const endpointModelGroup = (id: string) => `kr-ai-${id.replace(/-/g, '')}`

export const normalizeSlug = (value: string) => {
  return value
    .toLowerCase()
    .trim()
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/^-+|-+$/g, '')
}

export const newModelDraft = (): EndpointModelDraft => ({
  clientId: crypto.randomUUID(),
  providerMode: 'existing',
  providerId: '',
  providerName: '',
  providerType: 'openai',
  endpointUrl: '',
  apiKey: '',
  modelName: '',
  weight: 100,
})

export const providerAuthConfig = (providerType: ProviderType, apiKey: string) => {
  if (!apiKey) {
    return {}
  }

  if (providerType === 'gemini') {
    return {
      param_name: 'key',
      param_value: apiKey,
    }
  }

  return {
    header_name: providerType === 'anthropic' ? 'x-api-key' : 'Authorization',
    header_value: apiKey,
  }
}

export const providerEndpointOrigin = (providerType: ProviderType, endpointUrl?: string | null) => {
  const endpoint = endpointUrl?.trim()
    || (providerType === 'openai_compat' ? '' : providerDefaultEndpoints[providerType])

  if (!endpoint) {
    throw new Error('OpenAI-compatible providers require a service URL')
  }

  const url = new URL(endpoint)

  if (!['http:', 'https:'].includes(url.protocol)) {
    throw new Error('Provider service URL must use HTTP or HTTPS')
  }

  return url.origin
}

export const endpointToDraft = (endpoint: AiEndpoint): EndpointDraft => ({
  id: endpoint.id,
  displayName: endpoint.displayName,
  slug: endpoint.slug,
  enabled: endpoint.enabled,
  models: endpoint.models.map(model => ({
    clientId: model.id,
    providerMode: 'existing',
    providerId: model.provider_id,
    providerName: '',
    providerType: 'openai',
    endpointUrl: '',
    apiKey: '',
    modelName: model.model_name,
    weight: model.weight,
  })),
})
