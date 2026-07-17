<template>
  <PageHeader title="AI Models">
    <KButton
      :disabled="gatewaySaving || providerLoading || providers.length === 0"
      @click="startCreate"
    >
      Create Model
    </KButton>
  </PageHeader>
  <AiGatewayNav />

  <KAlert
    v-if="!providerLoading && providers.length === 0 && !errorMessage"
    appearance="info"
    class="ai-gateway-alert"
  >
    Create an AI provider before adding models.
  </KAlert>

  <KAlert
    v-if="errorMessage"
    appearance="danger"
    class="ai-gateway-alert"
  >
    {{ errorMessage }}
  </KAlert>

  <section
    v-if="gatewayEndpoint"
    class="ai-gateway-secret"
  >
    <strong>AI proxy route is ready</strong>
    <input
      class="ai-gateway-mono"
      readonly
      :value="gatewayEndpoint"
    >
    <div class="ai-gateway-key-actions">
      <KButton
        appearance="secondary"
        type="button"
        @click="copyGatewayEndpoint"
      >
        Copy Endpoint
      </KButton>
      <KButton
        appearance="tertiary"
        type="button"
        @click="gatewayEndpoint = ''"
      >
        Dismiss
      </KButton>
    </div>
  </section>

  <KCard
    v-if="formVisible"
    class="ai-gateway-form-card"
    :title="editingId ? 'Edit Model' : 'Create Model'"
  >
    <form
      class="ai-gateway-form"
      @submit.prevent="submitModel"
    >
      <div class="ai-gateway-form-grid">
        <div class="ai-gateway-form-field">
          <label for="ai-model-name">Group Name</label>
          <input
            id="ai-model-name"
            v-model.trim="form.name"
            required
          >
        </div>

        <div class="ai-gateway-form-field">
          <label for="ai-model-provider">Provider</label>
          <select
            id="ai-model-provider"
            v-model="form.providerId"
            required
          >
            <option
              v-for="provider in providers"
              :key="provider.id"
              :value="provider.id"
            >
              {{ provider.name }} ({{ provider.provider_type }})
            </option>
          </select>
        </div>

        <div class="ai-gateway-form-field">
          <label for="ai-model-provider-name">Provider Model Name</label>
          <input
            id="ai-model-provider-name"
            v-model.trim="form.modelName"
            required
          >
        </div>

        <div class="ai-gateway-form-field">
          <label for="ai-model-priority">Priority</label>
          <input
            id="ai-model-priority"
            v-model="form.priority"
            required
            type="number"
          >
        </div>

        <div class="ai-gateway-form-field">
          <label for="ai-model-weight">Weight</label>
          <input
            id="ai-model-weight"
            v-model="form.weight"
            required
            min="0"
            type="number"
          >
        </div>

        <label class="ai-gateway-checkbox">
          <input
            v-model="form.enabled"
            type="checkbox"
          >
          Enabled
        </label>
      </div>

      <div class="ai-gateway-form-grid">
        <div class="ai-gateway-form-field">
          <label for="ai-model-input-cost">Input Cost</label>
          <input
            id="ai-model-input-cost"
            v-model="form.inputCost"
            min="0"
            step="0.000001"
            type="number"
          >
        </div>

        <div class="ai-gateway-form-field">
          <label for="ai-model-output-cost">Output Cost</label>
          <input
            id="ai-model-output-cost"
            v-model="form.outputCost"
            min="0"
            step="0.000001"
            type="number"
          >
        </div>

        <div class="ai-gateway-form-field">
          <label for="ai-model-max-tokens">Max Tokens</label>
          <input
            id="ai-model-max-tokens"
            v-model="form.maxTokens"
            min="0"
            type="number"
          >
        </div>

        <div class="ai-gateway-form-field">
          <label for="ai-model-max-input-tokens">Max Input Tokens</label>
          <input
            id="ai-model-max-input-tokens"
            v-model="form.maxInputTokens"
            min="0"
            type="number"
          >
        </div>
      </div>

      <div class="ai-gateway-form-field">
        <label for="ai-model-config">Config JSON</label>
        <textarea
          id="ai-model-config"
          v-model="form.configJson"
        />
      </div>

      <div class="ai-gateway-form-field">
        <label for="ai-model-tags">Tags</label>
        <input
          id="ai-model-tags"
          v-model="form.tags"
        >
      </div>

      <div class="ai-gateway-form-actions">
        <KButton
          type="submit"
          :disabled="saving"
        >
          {{ saving ? 'Saving...' : 'Save Model' }}
        </KButton>
        <KButton
          appearance="secondary"
          type="button"
          @click="cancelForm"
        >
          Cancel
        </KButton>
      </div>
    </form>
  </KCard>

  <KCard
    v-if="gatewayModel"
    class="ai-gateway-form-card"
    title="Create AI Proxy Route"
  >
    <form
      class="ai-gateway-form"
      @submit.prevent="submitGatewayRoute"
    >
      <p class="ai-gateway-muted">
        Expose model group <strong>{{ gatewayModel.name }}</strong> through kong-rust.
        Provider credentials stay in the AI Provider record and are not copied into the plugin.
      </p>

      <div class="ai-gateway-form-grid">
        <div class="ai-gateway-form-field">
          <label for="ai-gateway-service-name">Service Name</label>
          <input
            id="ai-gateway-service-name"
            v-model.trim="gatewayForm.serviceName"
            :disabled="gatewaySaving"
            required
          >
        </div>

        <div class="ai-gateway-form-field">
          <label for="ai-gateway-route-name">Route Name</label>
          <input
            id="ai-gateway-route-name"
            v-model.trim="gatewayForm.routeName"
            :disabled="gatewaySaving"
            required
          >
        </div>

        <div class="ai-gateway-form-field">
          <label for="ai-gateway-route-path">Proxy Path</label>
          <input
            id="ai-gateway-route-path"
            v-model.trim="gatewayForm.path"
            :disabled="gatewaySaving"
            required
          >
        </div>

        <div class="ai-gateway-form-field">
          <label for="ai-gateway-proxy-url">Proxy Base URL</label>
          <input
            id="ai-gateway-proxy-url"
            v-model.trim="gatewayForm.proxyUrl"
            :disabled="gatewaySaving"
            required
            type="url"
          >
        </div>
      </div>

      <div class="ai-gateway-form-actions">
        <KButton
          type="submit"
          :disabled="gatewaySaving"
        >
          {{ gatewaySaving ? 'Creating...' : 'Create Proxy Route' }}
        </KButton>
        <KButton
          appearance="secondary"
          :disabled="gatewaySaving"
          type="button"
          @click="cancelGatewayRoute"
        >
          Cancel
        </KButton>
      </div>
    </form>
  </KCard>

  <KCard class="ai-gateway-table-card">
    <KTable
      :key="tableKey"
      :headers="headers"
      :fetcher="fetchModels"
      :error="!!tableErrorMessage"
      :error-state-message="tableErrorMessage"
      empty-state-title="No AI models"
      empty-state-message="Create a model to expose an AI model group."
      pagination-offset
    >
      <template #name="{ rowValue }">
        <strong>{{ rowValue }}</strong>
      </template>

      <template #provider_id="{ rowValue }">
        <span>{{ providerName(rowValue) }}</span>
      </template>

      <template #enabled="{ rowValue }">
        <KBadge :appearance="rowValue ? 'success' : 'neutral'">
          {{ rowValue ? 'Enabled' : 'Disabled' }}
        </KBadge>
      </template>

      <template #cost="{ row }">
        <span>{{ formatCost(row.input_cost) }} / {{ formatCost(row.output_cost) }}</span>
      </template>

      <template #tokens="{ row }">
        <span>{{ row.max_input_tokens ?? '-' }} / {{ row.max_tokens ?? '-' }}</span>
      </template>

      <template #tags="{ rowValue }">
        <div
          v-if="rowValue?.length"
          class="ai-gateway-badge-list"
        >
          <KBadge
            v-for="tag in rowValue"
            :key="tag"
            appearance="neutral"
          >
            {{ tag }}
          </KBadge>
        </div>
        <span v-else>-</span>
      </template>

      <template #actions="{ row }">
        <div class="ai-gateway-row-actions">
          <KButton
            appearance="secondary"
            :disabled="gatewaySaving"
            size="small"
            @click="startGatewayRoute(row)"
          >
            Create Route
          </KButton>
          <KButton
            appearance="secondary"
            :disabled="gatewaySaving"
            size="small"
            @click="startEdit(row)"
          >
            Edit
          </KButton>
          <KButton
            appearance="danger"
            :disabled="gatewaySaving"
            size="small"
            @click="deleteModel(row)"
          >
            Delete
          </KButton>
        </div>
      </template>
    </KTable>
  </KCard>
</template>

<script setup lang="ts">
import type { TableDataFetcherParams } from '@kong/kongponents'
import { computed, onMounted, reactive, ref } from 'vue'
import AiGatewayNav from './AiGatewayNav.vue'
import { apiService } from '@/services/apiService'
import { useToaster } from '@/composables/useToaster'
import type { AiModel, AiProvider, KongPageResponse } from './types'
import {
  emptyJsonObject,
  formatTags,
  getErrorMessage,
  omitUndefined,
  parseJsonObject,
  parseOptionalFloat,
  parseOptionalInt,
  parseTags,
  stringifyJson,
} from './utils'

interface ModelFormState {
  name: string
  providerId: string
  modelName: string
  priority: string | number
  weight: string | number
  inputCost: string | number
  outputCost: string | number
  maxTokens: string | number
  maxInputTokens: string | number
  configJson: string
  enabled: boolean
  tags: string
}

interface GatewayFormState {
  serviceName: string
  routeName: string
  path: string
  proxyUrl: string
}

interface CreatedGatewayResources {
  pluginId: string
  routeId: string
  serviceId: string
}

defineOptions({
  name: 'AiGatewayModels',
})

const toaster = useToaster()
const tableKey = ref(0)
const formVisible = ref(false)
const saving = ref(false)
const editingId = ref('')
const errorMessage = ref('')
const tableErrorMessage = ref('')
const providers = ref<AiProvider[]>([])
const providerLoading = ref(true)
const gatewayModel = ref<AiModel | null>(null)
const gatewaySaving = ref(false)
const gatewayEndpoint = ref('')

const providerMap = computed(() => {
  return new Map(providers.value.map(provider => [provider.id, provider]))
})

const headers = [
  { label: 'Group Name', key: 'name' },
  { label: 'Provider', key: 'provider_id' },
  { label: 'Provider Model', key: 'model_name' },
  { label: 'Priority', key: 'priority' },
  { label: 'Weight', key: 'weight' },
  { label: 'Input / Output Cost', key: 'cost' },
  { label: 'Input / Total Tokens', key: 'tokens' },
  { label: 'Status', key: 'enabled' },
  { label: 'Tags', key: 'tags' },
  { hideLabel: true, key: 'actions' },
]

const form = reactive<ModelFormState>({
  name: '',
  providerId: '',
  modelName: '',
  priority: '0',
  weight: '100',
  inputCost: '',
  outputCost: '',
  maxTokens: '',
  maxInputTokens: '',
  configJson: emptyJsonObject,
  enabled: true,
  tags: '',
})

const gatewayForm = reactive<GatewayFormState>({
  serviceName: '',
  routeName: '',
  path: '',
  proxyUrl: '',
})

const resetForm = () => {
  form.name = ''
  form.providerId = providers.value[0]?.id ?? ''
  form.modelName = ''
  form.priority = '0'
  form.weight = '100'
  form.inputCost = ''
  form.outputCost = ''
  form.maxTokens = ''
  form.maxInputTokens = ''
  form.configJson = emptyJsonObject
  form.enabled = true
  form.tags = ''
}

const loadProviders = async () => {
  providerLoading.value = true

  try {
    const selectedProviderId = form.providerId
    const loadedProviders: AiProvider[] = []
    const seenOffsets = new Set<string>()
    let offset: string | number | undefined

    while (true) {
      const { data } = await apiService.findAll<KongPageResponse<AiProvider>>('ai-providers', {
        size: 1000,
        ...(offset === undefined ? {} : { offset }),
      })

      loadedProviders.push(...data.data)

      if (data.offset === null || data.offset === undefined) {
        break
      }

      const offsetKey = String(data.offset)
      if (seenOffsets.has(offsetKey)) {
        throw new Error('Pagination for ai-providers returned a repeated offset')
      }

      seenOffsets.add(offsetKey)
      offset = data.offset
    }

    providers.value = loadedProviders
    if (selectedProviderId) {
      form.providerId = selectedProviderId
    } else {
      form.providerId = providers.value[0]?.id ?? ''
    }
  } catch (err) {
    errorMessage.value = getErrorMessage(err, 'Unable to load AI providers')
  } finally {
    providerLoading.value = false
  }
}

const fetchModels = async (props: TableDataFetcherParams) => {
  tableErrorMessage.value = ''

  try {
    const { data } = await apiService.findAll<KongPageResponse<AiModel>>('ai-models', {
      size: props.pageSize,
      offset: props.page === 1 ? undefined : props.offset,
    })

    return {
      data: data.data,
      ...(data.offset ? { pagination: { offset: data.offset } } : null),
    }
  } catch (err) {
    tableErrorMessage.value = getErrorMessage(err, 'Unable to load AI models')
  }
}

const providerName = (providerId: string) => {
  const provider = providerMap.value.get(providerId)

  return provider ? `${provider.name} (${provider.provider_type})` : providerId
}

const formatCost = (value?: number | null) => value ?? '-'

const modelSlug = (name: string) => {
  return name
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/^-+|-+$/g, '') || 'model'
}

const defaultProxyUrl = () => {
  const port = window.location.protocol === 'https:' ? 8443 : 8000

  return `${window.location.protocol}//${window.location.hostname}:${port}`
}

const startCreate = () => {
  if (providerLoading.value || providers.value.length === 0) {
    return
  }

  errorMessage.value = ''
  gatewayModel.value = null
  editingId.value = ''
  resetForm()
  formVisible.value = true
}

const startEdit = (model: AiModel) => {
  errorMessage.value = ''
  gatewayModel.value = null
  editingId.value = model.id
  form.name = model.name
  form.providerId = model.provider_id
  form.modelName = model.model_name
  form.priority = String(model.priority)
  form.weight = String(model.weight)
  form.inputCost = model.input_cost === null || model.input_cost === undefined ? '' : String(model.input_cost)
  form.outputCost = model.output_cost === null || model.output_cost === undefined ? '' : String(model.output_cost)
  form.maxTokens = model.max_tokens === null || model.max_tokens === undefined ? '' : String(model.max_tokens)
  form.maxInputTokens = model.max_input_tokens === null || model.max_input_tokens === undefined ? '' : String(model.max_input_tokens)
  form.configJson = stringifyJson(model.config)
  form.enabled = model.enabled
  form.tags = formatTags(model.tags)
  formVisible.value = true
}

const startGatewayRoute = (model: AiModel) => {
  if (gatewaySaving.value) {
    return
  }

  const slug = modelSlug(model.name)

  errorMessage.value = ''
  formVisible.value = false
  gatewayEndpoint.value = ''
  gatewayModel.value = model
  gatewayForm.serviceName = `ai-${slug}`
  gatewayForm.routeName = `ai-${slug}`
  gatewayForm.path = `/ai/${slug}/v1/chat/completions`
  gatewayForm.proxyUrl = defaultProxyUrl()
}

const cancelGatewayRoute = () => {
  errorMessage.value = ''
  gatewayModel.value = null
  gatewayForm.serviceName = ''
  gatewayForm.routeName = ''
  gatewayForm.path = ''
  gatewayForm.proxyUrl = ''
}

const cancelForm = () => {
  errorMessage.value = ''
  formVisible.value = false
  editingId.value = ''
  resetForm()
}

const submitModel = async () => {
  saving.value = true
  errorMessage.value = ''

  try {
    if (!form.providerId || !providerMap.value.has(form.providerId)) {
      throw new Error('Select an available AI provider')
    }

    const priority = parseOptionalInt(form.priority, 'Priority')
    const weight = parseOptionalInt(form.weight, 'Weight')

    if (priority === undefined || weight === undefined) {
      throw new Error('Priority and weight are required')
    }

    const body = omitUndefined({
      name: form.name,
      provider_id: form.providerId,
      model_name: form.modelName,
      priority,
      weight,
      input_cost: parseOptionalFloat(form.inputCost, 'Input cost') ?? (editingId.value ? null : undefined),
      output_cost: parseOptionalFloat(form.outputCost, 'Output cost') ?? (editingId.value ? null : undefined),
      max_tokens: parseOptionalInt(form.maxTokens, 'Max tokens') ?? (editingId.value ? null : undefined),
      max_input_tokens: parseOptionalInt(form.maxInputTokens, 'Max input tokens') ?? (editingId.value ? null : undefined),
      config: parseJsonObject(form.configJson, 'Config'),
      enabled: form.enabled,
      tags: parseTags(form.tags) ?? (editingId.value ? null : undefined),
    })

    if (editingId.value) {
      await apiService.patch(`ai-models/${editingId.value}`, body)
      toaster.open({ appearance: 'success', message: `Updated model ${form.name}` })
    } else {
      await apiService.post('ai-models', body)
      toaster.open({ appearance: 'success', message: `Created model ${form.name}` })
    }

    cancelForm()
    tableKey.value += 1
  } catch (err) {
    errorMessage.value = getErrorMessage(err, 'Unable to save AI model')
  } finally {
    saving.value = false
  }
}

const serviceUrlFor = (model: AiModel) => {
  const provider = providerMap.value.get(model.provider_id)

  if (!provider) {
    throw new Error(`Provider ${model.provider_id} is not available`)
  }

  const builtInEndpoints: Record<string, string> = {
    openai: 'https://api.openai.com',
    anthropic: 'https://api.anthropic.com',
    gemini: 'https://generativelanguage.googleapis.com',
  }
  const endpoint = provider.endpoint_url?.trim() || builtInEndpoints[provider.provider_type]

  if (!endpoint) {
    throw new Error(`Provider ${provider.name} must have an endpoint URL before creating a proxy route`)
  }

  let url: URL

  try {
    url = new URL(endpoint)
  } catch {
    throw new Error(`Provider ${provider.name} has an invalid endpoint URL`)
  }

  if (!['http:', 'https:'].includes(url.protocol)) {
    throw new Error(`Provider ${provider.name} endpoint URL must use HTTP or HTTPS`)
  }

  return url.origin
}

const createdEntityId = (value: unknown, entityName: string) => {
  if (
    typeof value !== 'object'
    || value === null
    || !('id' in value)
    || typeof value.id !== 'string'
    || !value.id
  ) {
    throw new Error(`Created ${entityName} response did not include an ID`)
  }

  return value.id
}

const proxyEndpointFor = (baseUrl: string, path: string) => {
  let url: URL

  try {
    url = new URL(baseUrl)
  } catch {
    throw new Error('Proxy base URL must be a valid URL')
  }

  if (!['http:', 'https:'].includes(url.protocol) || url.search || url.hash) {
    throw new Error('Proxy base URL must use HTTP or HTTPS without a query or fragment')
  }

  const basePath = url.pathname === '/' ? '' : url.pathname.replace(/\/+$/, '')

  return `${url.origin}${basePath}${path}`
}

const rollbackGatewayResources = async (resources: CreatedGatewayResources) => {
  const failures: string[] = []
  const rollbackSteps = [
    { id: resources.pluginId, label: 'plugin', path: 'plugins' },
    { id: resources.routeId, label: 'route', path: 'routes' },
    { id: resources.serviceId, label: 'service', path: 'services' },
  ]

  for (const step of rollbackSteps) {
    if (!step.id) {
      continue
    }

    try {
      await apiService.delete(`${step.path}/${step.id}`)
    } catch {
      failures.push(step.label)
    }
  }

  return failures
}

const submitGatewayRoute = async () => {
  if (!gatewayModel.value) {
    return
  }

  const model = gatewayModel.value
  const path = gatewayForm.path.startsWith('/') ? gatewayForm.path : `/${gatewayForm.path}`
  const serviceName = gatewayForm.serviceName
  const routeName = gatewayForm.routeName
  let serviceUrl = ''
  let proxyEndpoint = ''

  try {
    serviceUrl = serviceUrlFor(model)
    proxyEndpoint = proxyEndpointFor(gatewayForm.proxyUrl, path)
  } catch (err) {
    errorMessage.value = getErrorMessage(err, 'Unable to create AI proxy route')
    return
  }

  const resources: CreatedGatewayResources = {
    pluginId: '',
    routeId: '',
    serviceId: '',
  }

  gatewaySaving.value = true
  errorMessage.value = ''

  try {
    const serviceResponse = await apiService.post('services', {
      name: serviceName,
      url: serviceUrl,
    })
    resources.serviceId = createdEntityId(serviceResponse.data, 'service')

    const routeResponse = await apiService.post(`services/${resources.serviceId}/routes`, {
      name: routeName,
      paths: [path],
      methods: ['POST'],
      strip_path: false,
    })
    resources.routeId = createdEntityId(routeResponse.data, 'route')

    const pluginResponse = await apiService.post('plugins', {
      name: 'ai-proxy',
      route: { id: resources.routeId },
      config: {
        model: model.name,
        model_source: 'config',
        route_type: 'llm/v1/chat',
        client_protocol: 'openai',
        response_streaming: 'allow',
      },
    })
    resources.pluginId = createdEntityId(pluginResponse.data, 'plugin')
  } catch (err) {
    const rollbackFailures = await rollbackGatewayResources(resources)
    const cleanupMessage = rollbackFailures.length
      ? ` Automatic cleanup failed for: ${rollbackFailures.join(', ')}. Remove these resources manually.`
      : ''

    errorMessage.value = `${getErrorMessage(err, 'Unable to create AI proxy route')}${cleanupMessage}`
    return
  } finally {
    gatewaySaving.value = false
  }

  gatewayEndpoint.value = proxyEndpoint
  toaster.open({
    appearance: 'success',
    message: `Created proxy route ${routeName}`,
  })
  gatewayModel.value = null
  gatewayForm.serviceName = ''
  gatewayForm.routeName = ''
  gatewayForm.path = ''
  gatewayForm.proxyUrl = ''
}

const copyGatewayEndpoint = async () => {
  try {
    await navigator.clipboard.writeText(gatewayEndpoint.value)
    toaster.open({ appearance: 'success', message: 'Copied proxy endpoint' })
  } catch (err) {
    errorMessage.value = getErrorMessage(err, 'Unable to copy proxy endpoint')
  }
}

const deleteModel = async (model: AiModel) => {
  if (!window.confirm(`Delete AI model "${model.name}"?`)) {
    return
  }

  errorMessage.value = ''

  try {
    await apiService.delete(`ai-models/${model.id}`)
    toaster.open({ appearance: 'success', message: `Deleted model ${model.name}` })
    tableKey.value += 1
  } catch (err) {
    errorMessage.value = getErrorMessage(err, 'Unable to delete AI model')
  }
}

onMounted(loadProviders)
</script>
