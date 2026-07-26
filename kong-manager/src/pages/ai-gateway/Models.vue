<template>
  <PageHeader :title="t('AI Models')">
    <KButton
      :disabled="gatewaySaving || providerLoading || providers.length === 0"
      @click="startCreate"
    >
      {{ t('Create Model') }}
    </KButton>
  </PageHeader>
  <AiGatewayNav />

  <KAlert
    v-if="!providerLoading && providers.length === 0 && !errorMessage"
    appearance="info"
    class="ai-gateway-alert"
  >
    {{ locale === 'zh-CN' ? '请先创建 AI 服务商，再添加模型。' : 'Create an AI provider before adding models.' }}
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
    <strong>{{ l('AI proxy route is ready', 'AI 代理路由已就绪') }}</strong>
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
        {{ l('Copy Endpoint', '复制接口') }}
      </KButton>
      <KButton
        appearance="tertiary"
        type="button"
        @click="gatewayEndpoint = ''"
      >
        {{ t('Dismiss') }}
      </KButton>
    </div>
  </section>

  <KCard
    v-if="formVisible"
    class="ai-gateway-form-card"
    :title="editingId ? t('Edit Model') : t('Create Model')"
  >
    <form
      class="ai-gateway-form"
      @submit.prevent="submitModel"
    >
      <div class="ai-gateway-form-grid">
        <div class="ai-gateway-form-field">
          <label for="ai-model-name">{{ t('Group Name') }}</label>
          <input
            id="ai-model-name"
            v-model.trim="form.name"
            required
          >
        </div>

        <div class="ai-gateway-form-field">
          <label for="ai-model-provider">{{ t('Provider') }}</label>
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
          <label for="ai-model-provider-name">{{ t('Provider Model Name') }}</label>
          <input
            id="ai-model-provider-name"
            v-model.trim="form.modelName"
            required
          >
        </div>

        <div class="ai-gateway-form-field">
          <label for="ai-model-priority">{{ t('Priority') }}</label>
          <input
            id="ai-model-priority"
            v-model="form.priority"
            required
            type="number"
          >
        </div>

        <div class="ai-gateway-form-field">
          <label for="ai-model-weight">{{ t('Weight') }}</label>
          <input
            id="ai-model-weight"
            v-model="form.weight"
            max="10000"
            required
            min="0"
            step="1"
            type="number"
          >
        </div>

        <label class="ai-gateway-checkbox">
          <input
            v-model="form.enabled"
            type="checkbox"
          >
          {{ t('Enabled') }}
        </label>
      </div>

      <div class="ai-gateway-form-grid">
        <div class="ai-gateway-form-field">
          <label for="ai-model-input-cost">
            {{ l('Custom input override (USD / 1M tokens)', '自定义 Input 覆盖价（USD / 1M tokens）') }}
          </label>
          <input
            id="ai-model-input-cost"
            v-model="form.inputCost"
            autocomplete="off"
            inputmode="decimal"
            type="text"
          >
          <p class="ai-endpoint-hint">
            {{ l('Leave blank to use the current built-in price; enter 0 to make this direction free.', '留空使用当前内置价格；填写 0 表示该方向免费。') }}
          </p>
        </div>

        <div class="ai-gateway-form-field">
          <label for="ai-model-output-cost">
            {{ l('Custom output override (USD / 1M tokens)', '自定义 Output 覆盖价（USD / 1M tokens）') }}
          </label>
          <input
            id="ai-model-output-cost"
            v-model="form.outputCost"
            autocomplete="off"
            inputmode="decimal"
            type="text"
          >
          <p class="ai-endpoint-hint">
            {{ l('Leave blank to use the current built-in price; enter 0 to make this direction free.', '留空使用当前内置价格；填写 0 表示该方向免费。') }}
          </p>
        </div>

        <div class="ai-gateway-form-field">
          <label for="ai-model-max-tokens">{{ t('Max Tokens') }}</label>
          <input
            id="ai-model-max-tokens"
            v-model="form.maxTokens"
            min="0"
            type="number"
          >
        </div>

        <div class="ai-gateway-form-field">
          <label for="ai-model-max-input-tokens">{{ t('Max Input Tokens') }}</label>
          <input
            id="ai-model-max-input-tokens"
            v-model="form.maxInputTokens"
            min="0"
            type="number"
          >
        </div>
      </div>

      <section
        v-if="editingModel?.effective_pricing"
        class="ai-model-pricing-panel"
        :aria-label="l('Effective pricing', '当前生效价格')"
      >
        <div class="ai-model-pricing-heading">
          <div>
            <strong>{{ l('Effective pricing', '当前生效价格') }}</strong>
            <p>
              {{ l('Resolved by the server from per-direction overrides and the built-in catalog.', '由服务端按方向合并自定义覆盖价与内置价。') }}
            </p>
          </div>
          <KBadge :appearance="pricingStatusAppearance(editingModel.effective_pricing.status)">
            {{ pricingStatusLabel(editingModel.effective_pricing.status) }}
          </KBadge>
        </div>

        <div class="ai-model-pricing-grid">
          <article
            v-for="direction in pricingDirections"
            :key="direction"
            class="ai-model-price-direction"
          >
            <strong>{{ directionLabel(direction) }}</strong>
            <template v-if="editingModel.effective_pricing[direction]">
              <span class="ai-model-price-amount">
                {{ formatEffectiveAmount(editingModel.effective_pricing[direction]?.amount) }}
              </span>
              <KBadge appearance="neutral">
                {{ priceSourceLabel(editingModel.effective_pricing[direction]?.source) }}
              </KBadge>
              <small>
                {{ priceMetadata(editingModel.effective_pricing[direction]) }}
              </small>
              <small v-if="effectivePeriod(editingModel.effective_pricing[direction])">
                {{ effectivePeriod(editingModel.effective_pricing[direction]) }}
              </small>
            </template>
            <template v-else>
              <span class="ai-model-price-amount">—</span>
              <small>{{ l('Unpriced', '未定价') }}</small>
            </template>
          </article>
        </div>

        <p
          v-if="editingModel.effective_pricing.catalog_snapshot_date"
          class="ai-model-catalog-meta"
        >
          {{
            l(
              `Catalog snapshot ${editingModel.effective_pricing.catalog_snapshot_date}${editingModel.effective_pricing.catalog_version ? ` · ${editingModel.effective_pricing.catalog_version}` : ''}`,
              `价表快照 ${editingModel.effective_pricing.catalog_snapshot_date}${editingModel.effective_pricing.catalog_version ? ` · ${editingModel.effective_pricing.catalog_version}` : ''}`,
            )
          }}
        </p>

        <ul
          v-if="editingModel.effective_pricing.conditions.length"
          class="ai-model-pricing-conditions"
        >
          <li
            v-for="condition in editingModel.effective_pricing.conditions"
            :key="`${condition.type}:${condition.value}`"
          >
            {{ conditionLabel(condition) }}
          </li>
        </ul>
      </section>

      <div class="ai-gateway-form-field">
        <label for="ai-model-tags">{{ t('Tags') }}</label>
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
          {{ saving ? t('Saving...') : t('Save Model') }}
        </KButton>
        <KButton
          appearance="secondary"
          type="button"
          @click="cancelForm"
        >
          {{ t('Cancel') }}
        </KButton>
      </div>
    </form>
  </KCard>

  <KCard
    v-if="gatewayModel"
    class="ai-gateway-form-card"
    :title="l('Create AI Proxy Route', '创建 AI 代理路由')"
  >
    <form
      class="ai-gateway-form"
      @submit.prevent="submitGatewayRoute"
    >
      <p class="ai-gateway-muted">
        {{
          l(
            `Expose model group ${gatewayModel.name} through kong-rust. Provider credentials stay in the AI Provider record and are not copied into the plugin.`,
            `通过 kong-rust 发布模型组 ${gatewayModel.name}。服务商凭据保留在 AI 服务商记录中，不会复制到插件。`,
          )
        }}
      </p>

      <div class="ai-gateway-form-grid">
        <div class="ai-gateway-form-field">
          <label for="ai-gateway-service-name">{{ l('Service Name', '服务名称') }}</label>
          <input
            id="ai-gateway-service-name"
            v-model.trim="gatewayForm.serviceName"
            :disabled="gatewaySaving"
            required
          >
        </div>

        <div class="ai-gateway-form-field">
          <label for="ai-gateway-route-name">{{ l('Route Name', '路由名称') }}</label>
          <input
            id="ai-gateway-route-name"
            v-model.trim="gatewayForm.routeName"
            :disabled="gatewaySaving"
            required
          >
        </div>

        <div class="ai-gateway-form-field">
          <label for="ai-gateway-route-path">{{ l('Proxy Path', '代理路径') }}</label>
          <input
            id="ai-gateway-route-path"
            v-model.trim="gatewayForm.path"
            :disabled="gatewaySaving"
            required
          >
        </div>

        <div class="ai-gateway-form-field">
          <label for="ai-gateway-proxy-url">{{ l('Proxy Base URL', '代理基础地址') }}</label>
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
          {{ gatewaySaving ? l('Creating...', '创建中...') : l('Create Proxy Route', '创建代理路由') }}
        </KButton>
        <KButton
          appearance="secondary"
          :disabled="gatewaySaving"
          type="button"
          @click="cancelGatewayRoute"
        >
          {{ t('Cancel') }}
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
      :empty-state-title="t('No AI models')"
      :empty-state-message="t('Create a model to expose an AI model group.')"
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
          {{ rowValue ? t('Enabled') : t('Disabled') }}
        </KBadge>
      </template>

      <template #cost="{ row }">
        <div class="ai-model-table-pricing">
          <div
            v-for="direction in pricingDirections"
            :key="direction"
          >
            <strong>{{ directionLabel(direction) }}</strong>
            <template v-if="row.effective_pricing?.[direction]">
              <span>{{ formatEffectiveAmount(row.effective_pricing[direction]?.amount) }}</span>
              <small>
                {{ priceSourceLabel(row.effective_pricing[direction]?.source) }}
                · {{ row.effective_pricing[direction]?.snapshot_date }}
                · {{ row.effective_pricing[direction]?.version }}
              </small>
            </template>
            <template v-else>
              <span>—</span>
              <small>{{ l('Unpriced', '未定价') }}</small>
            </template>
          </div>
          <small v-if="row.effective_pricing?.catalog_snapshot_date">
            {{
              l(
                `Catalog ${row.effective_pricing.catalog_snapshot_date}${row.effective_pricing.catalog_version ? ` · ${row.effective_pricing.catalog_version}` : ''}`,
                `价表 ${row.effective_pricing.catalog_snapshot_date}${row.effective_pricing.catalog_version ? ` · ${row.effective_pricing.catalog_version}` : ''}`,
              )
            }}
          </small>
          <small
            v-if="row.effective_pricing?.status === 'unsupported'"
            class="ai-model-price-warning"
          >
            {{ l('Current conditions are unsupported for cost calculation', '当前条件不支持成本计算') }}
          </small>
        </div>
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
            size="small"
            @click="viewUsage(row)"
          >
            {{ t('View Usage') }}
          </KButton>
          <KButton
            appearance="secondary"
            :disabled="gatewaySaving"
            size="small"
            @click="startGatewayRoute(row)"
          >
            {{ t('Create Route') }}
          </KButton>
          <KButton
            appearance="secondary"
            :disabled="gatewaySaving"
            size="small"
            @click="startEdit(row)"
          >
            {{ t('Edit') }}
          </KButton>
          <KButton
            appearance="danger"
            :disabled="gatewaySaving"
            size="small"
            @click="deleteModel(row)"
          >
            {{ t('Delete') }}
          </KButton>
        </div>
      </template>
    </KTable>
  </KCard>
</template>

<script setup lang="ts">
import type { TableDataFetcherParams } from '@kong/kongponents'
import { computed, onMounted, reactive, ref } from 'vue'
import { useRouter } from 'vue-router'
import AiGatewayNav from './AiGatewayNav.vue'
import { apiService } from '@/services/apiService'
import { useToaster } from '@/composables/useToaster'
import type {
  AiModel,
  AiModelEffectivePrice,
  AiModelPricingCondition,
  AiProvider,
  KongPageResponse,
} from './types'
import { useAiGatewayI18n } from './useAiGatewayI18n'
import {
  formatTags,
  getErrorMessage,
  omitUndefined,
  parseOptionalDecimal,
  parseOptionalInt,
  parseTags,
} from './utils'

interface ModelFormState {
  name: string
  providerId: string
  modelName: string
  priority: string | number
  weight: string | number
  inputCost: string
  outputCost: string
  maxTokens: string | number
  maxInputTokens: string | number
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
const router = useRouter()
const { l, locale, t } = useAiGatewayI18n()
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
const editingModel = ref<AiModel | null>(null)
const pricingDirections = ['input', 'output'] as const

const providerMap = computed(() => {
  return new Map(providers.value.map(provider => [provider.id, provider]))
})

const headers = computed(() => [
  { label: t('Group Name'), key: 'name' },
  { label: t('Provider'), key: 'provider_id' },
  { label: locale.value === 'zh-CN' ? '服务商模型' : 'Provider Model', key: 'model_name' },
  { label: t('Priority'), key: 'priority' },
  { label: t('Weight'), key: 'weight' },
  { label: locale.value === 'zh-CN' ? '当前 Input / Output 价格' : 'Effective Input / Output Pricing', key: 'cost' },
  { label: locale.value === 'zh-CN' ? '输入 / 总 Token' : 'Input / Total Tokens', key: 'tokens' },
  { label: t('Status'), key: 'enabled' },
  { label: t('Tags'), key: 'tags' },
  { hideLabel: true, key: 'actions' },
])

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
    errorMessage.value = getErrorMessage(
      err,
      l('Unable to load AI providers', '无法加载 AI 服务商'),
    )
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
    tableErrorMessage.value = getErrorMessage(
      err,
      l('Unable to load AI models', '无法加载 AI 模型'),
    )
  }
}

const providerName = (providerId: string) => {
  const provider = providerMap.value.get(providerId)

  return provider ? `${provider.name} (${provider.provider_type})` : providerId
}

const directionLabel = (direction: typeof pricingDirections[number]) => {
  return direction === 'input' ? 'Input' : 'Output'
}

const compactDecimal = (value: string) => {
  const [rawInteger = '0', rawFraction = ''] = value.split('.')
  const integer = rawInteger.replace(/^0+(?=\d)/, '') || '0'
  const fraction = rawFraction.replace(/0+$/, '')

  return fraction ? `${integer}.${fraction}` : integer
}

const formatEffectiveAmount = (value?: string | null) => {
  if (value === null || value === undefined || value === '') {
    return '—'
  }

  return `$${compactDecimal(value)} / 1M tokens`
}

const priceSourceLabel = (source?: string | null) => {
  if (source === 'model_override' || source === 'override') {
    return l('Custom override', '自定义覆盖')
  }
  if (source === 'builtin') {
    return l('Built-in price', '内置价')
  }

  return source || l('Unknown source', '未知来源')
}

const priceMetadata = (price?: AiModelEffectivePrice | null) => {
  if (!price) {
    return ''
  }

  return l(
    `Snapshot ${price.snapshot_date} · ${price.version}`,
    `快照 ${price.snapshot_date} · ${price.version}`,
  )
}

const effectivePeriod = (price?: AiModelEffectivePrice | null) => {
  if (!price?.effective_from && !price?.effective_to) {
    return ''
  }

  return l(
    `Effective ${price.effective_from ?? '—'} → ${price.effective_to ?? 'ongoing'}`,
    `生效期 ${price.effective_from ?? '—'} → ${price.effective_to ?? '持续有效'}`,
  )
}

const pricingStatusLabel = (status: string) => {
  const labels: Record<string, [string, string]> = {
    matched: ['Matched', '已匹配'],
    unmatched: ['Unpriced', '未定价'],
    unsupported: ['Unsupported conditions', '当前条件不支持计价'],
    not_applicable: ['Not applicable', '不适用'],
  }
  const label = labels[status]

  return label ? l(...label) : status
}

const pricingStatusAppearance = (status: string) => {
  if (status === 'matched') {
    return 'success' as const
  }
  if (status === 'unsupported') {
    return 'warning' as const
  }

  return 'neutral' as const
}

const conditionLabel = (condition: AiModelPricingCondition) => {
  if (condition.type === 'max_prompt_tokens') {
    const value = typeof condition.value === 'number'
      ? condition.value.toLocaleString(locale.value)
      : condition.value

    return l(`Prompt tokens ≤ ${value}`, `Prompt Token ≤ ${value}`)
  }

  return `${condition.type}: ${condition.value}`
}

const viewUsage = (model: AiModel) => {
  void router.push({
    name: 'ai-usage-overview',
    query: {
      actual_model: model.model_name,
      model_group: model.name,
      provider_id: model.provider_id,
      range: '24h',
      timezone: Intl.DateTimeFormat().resolvedOptions().timeZone || 'UTC',
    },
  })
}

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
  editingModel.value = null
  resetForm()
  formVisible.value = true
}

const startEdit = (model: AiModel) => {
  errorMessage.value = ''
  gatewayModel.value = null
  editingId.value = model.id
  editingModel.value = model
  form.name = model.name
  form.providerId = model.provider_id
  form.modelName = model.model_name
  form.priority = String(model.priority)
  form.weight = String(model.weight)
  form.inputCost = model.input_cost_decimal
    ?? (model.input_cost === null || model.input_cost === undefined ? '' : String(model.input_cost))
  form.outputCost = model.output_cost_decimal
    ?? (model.output_cost === null || model.output_cost === undefined ? '' : String(model.output_cost))
  form.maxTokens = model.max_tokens === null || model.max_tokens === undefined ? '' : String(model.max_tokens)
  form.maxInputTokens = model.max_input_tokens === null || model.max_input_tokens === undefined ? '' : String(model.max_input_tokens)
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
  editingModel.value = null
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
  editingModel.value = null
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
    if (weight < 0 || weight > 10_000) {
      throw new Error('Weight must be between 0 and 10000')
    }

    const body = omitUndefined({
      name: form.name,
      provider_id: form.providerId,
      model_name: form.modelName,
      priority,
      weight,
      input_cost: parseOptionalDecimal(form.inputCost, 'Input cost') ?? null,
      output_cost: parseOptionalDecimal(form.outputCost, 'Output cost') ?? null,
      max_tokens: parseOptionalInt(form.maxTokens, 'Max tokens') ?? (editingId.value ? null : undefined),
      max_input_tokens: parseOptionalInt(form.maxInputTokens, 'Max input tokens') ?? (editingId.value ? null : undefined),
      ...(!editingId.value ? { config: {} } : {}),
      enabled: form.enabled,
      tags: parseTags(form.tags) ?? (editingId.value ? null : undefined),
    })

    if (editingId.value) {
      await apiService.patch(`ai-models/${editingId.value}`, body)
      toaster.open({
        appearance: 'success',
        message: l(`Updated model ${form.name}`, `已更新模型 ${form.name}`),
      })
    } else {
      await apiService.post('ai-models', body)
      toaster.open({
        appearance: 'success',
        message: l(`Created model ${form.name}`, `已创建模型 ${form.name}`),
      })
    }

    cancelForm()
    tableKey.value += 1
  } catch (err) {
    errorMessage.value = getErrorMessage(
      err,
      l('Unable to save AI model', '无法保存 AI 模型'),
    )
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
      response_buffering: false,
    })
    resources.routeId = createdEntityId(routeResponse.data, 'route')

    const pluginResponse = await apiService.post('plugins', {
      name: 'ai-proxy',
      route: { id: resources.routeId },
      config: {
        model_group: model.name,
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
    message: l(`Created proxy route ${routeName}`, `已创建代理路由 ${routeName}`),
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
    toaster.open({
      appearance: 'success',
      message: l('Copied proxy endpoint', '已复制代理接口'),
    })
  } catch (err) {
    errorMessage.value = getErrorMessage(
      err,
      l('Unable to copy proxy endpoint', '无法复制代理接口'),
    )
  }
}

const deleteModel = async (model: AiModel) => {
  if (!window.confirm(l(
    `Delete AI model "${model.name}"?`,
    `删除 AI 模型“${model.name}”？`,
  ))) {
    return
  }

  errorMessage.value = ''

  try {
    await apiService.delete(`ai-models/${model.id}`)
    toaster.open({
      appearance: 'success',
      message: l(`Deleted model ${model.name}`, `已删除模型 ${model.name}`),
    })
    tableKey.value += 1
  } catch (err) {
    errorMessage.value = getErrorMessage(
      err,
      l('Unable to delete AI model', '无法删除 AI 模型'),
    )
  }
}

onMounted(loadProviders)
</script>
