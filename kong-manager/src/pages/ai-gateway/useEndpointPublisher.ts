import { apiService } from '@/services/apiService'
import type { AiModel, AiProvider, KongPageResponse } from './types'
import type {
  AiEndpoint,
  EndpointDraft,
  EndpointModelDraft,
  GatewayPlugin,
  GatewayRoute,
  GatewayService,
} from './endpointTypes'
import {
  endpointDisplayName,
  endpointIdFromTags,
  endpointManagedTag,
  endpointModelGroup,
  endpointPath,
  endpointTags,
  normalizeSlug,
  providerAuthConfig,
  providerEndpointOrigin,
} from './endpointUtils'

interface CreatedResources {
  providers: string[]
  models: string[]
  service: string
  route: string
  plugin: string
}

const listAll = async <T>(endpoint: string) => {
  const records: T[] = []
  const seenOffsets = new Set<string>()
  let offset: string | number | undefined

  while (true) {
    const { data } = await apiService.findAll<KongPageResponse<T>>(endpoint, {
      size: 1000,
      ...(offset === undefined ? {} : { offset }),
    })

    records.push(...data.data)

    if (data.offset === null || data.offset === undefined) {
      return records
    }

    const key = String(data.offset)

    if (seenOffsets.has(key)) {
      throw new Error(`Pagination for ${endpoint} returned a repeated offset`)
    }

    seenOffsets.add(key)
    offset = data.offset
  }
}

const createdId = (value: unknown, label: string) => {
  if (
    typeof value !== 'object'
    || value === null
    || !('id' in value)
    || typeof value.id !== 'string'
    || !value.id
  ) {
    throw new Error(`Created ${label} response did not include an ID`)
  }

  return value.id
}

const resourceName = (slug: string) => `ai-${slug}`

const routeSlug = (route?: GatewayRoute, fallback = 'endpoint') => {
  const path = route?.paths?.[0] ?? ''
  const match = path.match(/^\/ai\/([^/]+)\/v1\/chat\/completions$/)

  return match?.[1] ?? fallback
}

export const loadEndpointResources = async () => {
  const [services, routes, plugins, models, providers] = await Promise.all([
    listAll<GatewayService>('services'),
    listAll<GatewayRoute>('routes'),
    listAll<GatewayPlugin>('plugins'),
    listAll<AiModel>('ai-models'),
    listAll<AiProvider>('ai-providers'),
  ])

  return { services, routes, plugins, models, providers }
}

export const loadEndpoints = async () => {
  const resources = await loadEndpointResources()
  const providerMap = new Map(resources.providers.map(provider => [provider.id, provider]))

  return resources.services
    .filter(service => service.tags?.includes(endpointManagedTag))
    .flatMap<AiEndpoint>(service => {
      const id = endpointIdFromTags(service.tags)

      if (!id) {
        return []
      }

      const route = resources.routes.find(item => (
        item.service?.id === service.id && endpointIdFromTags(item.tags) === id
      ))
      const plugin = resources.plugins.find(item => (
        item.route?.id === route?.id
        && item.name === 'ai-proxy'
        && endpointIdFromTags(item.tags) === id
      ))
      const modelGroup = plugin?.config.model_group ?? endpointModelGroup(id)
      const endpointModels = resources.models.filter(model => (
        endpointIdFromTags(model.tags) === id || model.name === modelGroup
      ))
      const endpointProviders = Array.from(new Set(endpointModels.map(model => model.provider_id)))
        .map(providerId => providerMap.get(providerId))
        .filter((provider): provider is AiProvider => !!provider)
      const slug = routeSlug(route, service.name?.replace(/^ai-/, '') || 'endpoint')
      const complete = !!route
        && !!plugin
        && endpointModels.length > 0
        && endpointProviders.length === new Set(endpointModels.map(model => model.provider_id)).size

      return [{
        id,
        displayName: endpointDisplayName(service.tags, slug),
        slug,
        path: route?.paths?.[0] ?? endpointPath(slug),
        modelGroup,
        enabled: complete
          && service.enabled
          && plugin.enabled
          && endpointModels.some(model => model.enabled),
        complete,
        service,
        route,
        plugin,
        models: endpointModels,
        providers: endpointProviders,
      }]
    })
    .sort((left, right) => left.displayName.localeCompare(right.displayName))
}

const validateDraft = (draft: EndpointDraft) => {
  const slug = normalizeSlug(draft.slug)

  if (!draft.displayName.trim()) {
    throw new Error('Endpoint name is required')
  }

  if (!slug) {
    throw new Error('Endpoint path is required')
  }

  if (draft.models.length === 0) {
    throw new Error('Add at least one model')
  }

  if (draft.models.some(model => !model.modelName.trim())) {
    throw new Error('Every model needs a model name')
  }

  if (draft.models.some(model => model.providerMode === 'existing' && !model.providerId)) {
    throw new Error('Select a provider for every model')
  }

  if (draft.models.some(model => (
    model.providerMode === 'new'
    && (!model.providerName.trim() || (model.providerType === 'openai_compat' && !model.endpointUrl.trim()))
  ))) {
    throw new Error('New provider connections need a name and, for compatible services, a URL')
  }

  const totalWeight = draft.models.reduce((sum, model) => sum + Number(model.weight), 0)

  if (draft.models.some(model => !Number.isInteger(Number(model.weight)) || Number(model.weight) < 0)) {
    throw new Error('Traffic percentages must be whole numbers of zero or greater')
  }

  if (totalWeight !== 100) {
    throw new Error('Traffic percentages must add up to 100%')
  }

  return slug
}

const createProvider = async (model: EndpointModelDraft) => {
  const { data } = await apiService.post('ai-providers', {
    name: model.providerName.trim(),
    provider_type: model.providerType,
    endpoint_url: model.endpointUrl.trim() || null,
    default_model: model.modelName.trim(),
    auth_config: providerAuthConfig(model.providerType, model.apiKey.trim()),
    config: {},
    enabled: true,
    tags: ['kr-ai-provider'],
  })

  return createdId(data, 'provider')
}

const resolveProviders = async (
  models: EndpointModelDraft[],
  providers: AiProvider[],
  created: CreatedResources,
) => {
  const providerMap = new Map(providers.map(provider => [provider.id, provider]))
  const resolved: Array<{ draft: EndpointModelDraft, provider: AiProvider }> = []

  for (const model of models) {
    let provider: AiProvider | undefined

    if (model.providerMode === 'existing') {
      provider = providerMap.get(model.providerId)
    } else {
      const providerId = await createProvider(model)
      created.providers.push(providerId)
      const { data } = await apiService.findRecord<AiProvider>('ai-providers', providerId)
      provider = data
      providerMap.set(providerId, provider)
    }

    if (!provider) {
      throw new Error(`Provider for model ${model.modelName} is not available`)
    }

    resolved.push({ draft: model, provider })
  }

  return resolved
}

const createModels = async (
  id: string,
  displayName: string,
  modelGroup: string,
  models: Array<{ draft: EndpointModelDraft, provider: AiProvider }>,
  created: CreatedResources,
) => {
  for (const { draft, provider } of models) {
    const { data } = await apiService.post('ai-models', {
      name: modelGroup,
      provider_id: provider.id,
      model_name: draft.modelName.trim(),
      priority: 0,
      weight: Number(draft.weight),
      config: {},
      enabled: true,
      tags: endpointTags(id, displayName),
    })

    created.models.push(createdId(data, 'model'))
  }
}

const serviceFieldsForProvider = (provider: AiProvider) => {
  const origin = providerEndpointOrigin(
    provider.provider_type as EndpointModelDraft['providerType'],
    provider.endpoint_url,
  )
  const url = new URL(origin)

  return {
    protocol: url.protocol.slice(0, -1),
    host: url.hostname,
    port: url.port ? Number(url.port) : (url.protocol === 'https:' ? 443 : 80),
    path: null,
  }
}

const rollbackCreated = async (created: CreatedResources) => {
  const steps = [
    ...(created.plugin ? [{ endpoint: 'plugins', id: created.plugin }] : []),
    ...(created.route ? [{ endpoint: 'routes', id: created.route }] : []),
    ...(created.service ? [{ endpoint: 'services', id: created.service }] : []),
    ...created.models.map(id => ({ endpoint: 'ai-models', id })).reverse(),
    ...created.providers.map(id => ({ endpoint: 'ai-providers', id })).reverse(),
  ]

  const failures: string[] = []

  for (const step of steps) {
    try {
      await apiService.delete(`${step.endpoint}/${step.id}`)
    } catch {
      failures.push(`${step.endpoint}/${step.id}`)
    }
  }

  return failures
}

const emptyCreatedResources = (): CreatedResources => ({
  providers: [],
  models: [],
  service: '',
  route: '',
  plugin: '',
})

export const createEndpoint = async (draft: EndpointDraft) => {
  const slug = validateDraft(draft)
  const id = crypto.randomUUID()
  const modelGroup = endpointModelGroup(id)
  const tags = endpointTags(id, draft.displayName.trim())
  const created = emptyCreatedResources()

  try {
    const { providers } = await loadEndpointResources()
    const resolvedModels = await resolveProviders(draft.models, providers, created)
    await createModels(id, draft.displayName.trim(), modelGroup, resolvedModels, created)
    const primaryModel = resolvedModels[0]

    if (!primaryModel) {
      throw new Error('Add at least one model')
    }

    const serviceResponse = await apiService.post('services', {
      name: resourceName(slug),
      ...serviceFieldsForProvider(primaryModel.provider),
      enabled: draft.enabled,
      tags,
    })
    created.service = createdId(serviceResponse.data, 'service')

    const routeResponse = await apiService.post(`services/${created.service}/routes`, {
      name: resourceName(slug),
      paths: [endpointPath(slug)],
      methods: ['POST'],
      strip_path: false,
      response_buffering: false,
      tags,
    })
    created.route = createdId(routeResponse.data, 'route')

    const pluginResponse = await apiService.post('plugins', {
      name: 'ai-proxy',
      instance_name: `kr-ai-endpoint-${id}`,
      route: { id: created.route },
      enabled: draft.enabled,
      tags,
      config: {
        model_group: modelGroup,
        model_source: 'config',
        route_type: 'llm/v1/chat',
        client_protocol: 'openai',
        response_streaming: 'allow',
      },
    })
    created.plugin = createdId(pluginResponse.data, 'plugin')

    return id
  } catch (error) {
    const failures = await rollbackCreated(created)
    const suffix = failures.length
      ? ` Cleanup failed for ${failures.join(', ')}.`
      : ''

    throw new Error(`${error instanceof Error ? error.message : 'Unable to publish endpoint'}.${suffix}`)
  }
}

export const updateEndpoint = async (endpoint: AiEndpoint, draft: EndpointDraft) => {
  if (!endpoint.route || !endpoint.plugin) {
    throw new Error('Incomplete endpoints must be repaired in Advanced Resources')
  }

  const slug = validateDraft(draft)
  const displayName = draft.displayName.trim()
  const tags = endpointTags(endpoint.id, displayName)
  const created = emptyCreatedResources()
  const oldService = endpoint.service
  const oldRoute = endpoint.route
  const oldPlugin = endpoint.plugin

  try {
    const { providers } = await loadEndpointResources()
    const resolvedModels = await resolveProviders(draft.models, providers, created)
    await createModels(endpoint.id, displayName, endpoint.modelGroup, resolvedModels, created)
    const primaryModel = resolvedModels[0]

    if (!primaryModel) {
      throw new Error('Add at least one model')
    }

    await apiService.patch(`services/${endpoint.service.id}`, {
      name: resourceName(slug),
      ...serviceFieldsForProvider(primaryModel.provider),
      enabled: draft.enabled,
      tags,
    })
    await apiService.patch(`routes/${endpoint.route.id}`, {
      name: resourceName(slug),
      paths: [endpointPath(slug)],
      methods: ['POST'],
      strip_path: false,
      response_buffering: false,
      tags,
    })
    await apiService.patch(`plugins/${endpoint.plugin.id}`, {
      enabled: draft.enabled,
      tags,
      config: {
        ...endpoint.plugin.config,
        model_group: endpoint.modelGroup,
        model_source: 'config',
        route_type: 'llm/v1/chat',
        client_protocol: 'openai',
        response_streaming: 'allow',
      },
    })

    for (const model of endpoint.models) {
      await apiService.delete(`ai-models/${model.id}`)
    }
  } catch (error) {
    await Promise.allSettled([
      apiService.patch(`services/${endpoint.service.id}`, {
        name: oldService.name,
        protocol: oldService.protocol,
        host: oldService.host,
        port: oldService.port,
        path: oldService.path ?? null,
        enabled: oldService.enabled,
        tags: oldService.tags ?? null,
      }),
      apiService.patch(`routes/${endpoint.route.id}`, {
        name: oldRoute.name,
        paths: oldRoute.paths ?? null,
        methods: oldRoute.methods ?? null,
        response_buffering: oldRoute.response_buffering,
        tags: oldRoute.tags ?? null,
      }),
      apiService.patch(`plugins/${endpoint.plugin.id}`, {
        enabled: oldPlugin.enabled,
        config: oldPlugin.config,
        tags: oldPlugin.tags ?? null,
      }),
    ])
    const failures = await rollbackCreated(created)
    const suffix = failures.length
      ? ` Cleanup failed for ${failures.join(', ')}.`
      : ''

    throw new Error(`${error instanceof Error ? error.message : 'Unable to update endpoint'}.${suffix}`)
  }
}

export const deleteEndpoint = async (endpoint: AiEndpoint) => {
  const steps = [
    ...(endpoint.plugin ? [{ endpoint: 'plugins', id: endpoint.plugin.id }] : []),
    ...(endpoint.route ? [{ endpoint: 'routes', id: endpoint.route.id }] : []),
    ...endpoint.models.map(model => ({ endpoint: 'ai-models', id: model.id })),
    { endpoint: 'services', id: endpoint.service.id },
  ]
  const failures: string[] = []

  for (const step of steps) {
    try {
      await apiService.delete(`${step.endpoint}/${step.id}`)
    } catch {
      failures.push(`${step.endpoint}/${step.id}`)
    }
  }

  if (failures.length) {
    throw new Error(`Unable to delete: ${failures.join(', ')}`)
  }
}
