<template>
  <PageHeader :title="t('AI Providers')">
    <KButton @click="startCreate">
      {{ t('Create Provider') }}
    </KButton>
  </PageHeader>
  <AiGatewayNav />

  <KAlert
    v-if="errorMessage"
    appearance="danger"
    class="ai-gateway-alert"
  >
    {{ errorMessage }}
  </KAlert>

  <KCard
    v-if="formVisible"
    class="ai-gateway-form-card"
    :title="editingId ? t('Edit Provider') : t('Create Provider')"
  >
    <form
      class="ai-gateway-form"
      @submit.prevent="submitProvider"
    >
      <div class="ai-gateway-form-grid">
        <div class="ai-gateway-form-field">
          <label for="ai-provider-name">{{ t('Name') }}</label>
          <input
            id="ai-provider-name"
            v-model.trim="form.name"
            required
          >
        </div>

        <div class="ai-gateway-form-field">
          <label for="ai-provider-type">{{ t('Provider Type') }}</label>
          <select
            id="ai-provider-type"
            v-model="form.providerType"
            required
          >
            <option value="openai">
              OpenAI
            </option>
            <option value="anthropic">
              Anthropic
            </option>
            <option value="gemini">
              Gemini
            </option>
            <option value="openai_compat">
              OpenAI Compatible (DeepSeek, etc.)
            </option>
          </select>
        </div>

        <div class="ai-gateway-form-field">
          <label for="ai-provider-endpoint">{{ t('Endpoint URL') }}</label>
          <input
            id="ai-provider-endpoint"
            v-model.trim="form.endpointUrl"
            :placeholder="form.providerType === 'openai_compat' ? deepSeekEndpoint : ''"
            :required="form.providerType === 'openai_compat'"
            type="url"
          >
        </div>

        <div class="ai-gateway-form-field">
          <label for="ai-provider-default-model">{{ t('Default Model') }}</label>
          <input
            id="ai-provider-default-model"
            v-model.trim="form.defaultModel"
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

      <div class="ai-gateway-form-field">
        <label for="ai-provider-api-key">
          {{ t('API key') }}
          <span v-if="form.providerType === 'openai_compat'">{{ locale === 'zh-CN' ? '（可选）' : '(optional)' }}</span>
        </label>
        <input
          id="ai-provider-api-key"
          v-model="form.apiKey"
          autocomplete="new-password"
          :required="!editingId && form.providerType !== 'openai_compat'"
          type="password"
        >
        <span class="ai-gateway-muted">
          {{ t('Credentials are masked after saving.') }}
          <template v-if="editingId">
            {{ t('Leave this blank to keep the existing key.') }}
          </template>
          <template v-else-if="form.providerType === 'openai_compat'">
            {{ t('Local services such as Ollama may not require a key.') }}
          </template>
        </span>
      </div>

      <div class="ai-gateway-form-field">
        <label for="ai-provider-tags">{{ t('Tags') }}</label>
        <input
          id="ai-provider-tags"
          v-model="form.tags"
        >
      </div>

      <div class="ai-gateway-form-actions">
        <KButton
          type="submit"
          :disabled="saving"
        >
          {{ saving ? t('Saving...') : t('Save Provider') }}
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

  <KCard class="ai-gateway-table-card">
    <KTable
      :key="tableKey"
      :headers="headers"
      :fetcher="fetchProviders"
      :error="!!tableErrorMessage"
      :error-state-message="tableErrorMessage"
      :empty-state-title="t('No AI providers')"
      :empty-state-message="t('Create a provider to connect AI models.')"
      pagination-offset
    >
      <template #name="{ rowValue }">
        <strong>{{ rowValue }}</strong>
      </template>

      <template #enabled="{ rowValue }">
        <KBadge :appearance="rowValue ? 'success' : 'neutral'">
          {{ rowValue ? t('Enabled') : t('Disabled') }}
        </KBadge>
      </template>

      <template #endpoint_url="{ rowValue }">
        <span>{{ rowValue || '-' }}</span>
      </template>

      <template #default_model="{ rowValue }">
        <span>{{ rowValue || '-' }}</span>
      </template>

      <template #created_at="{ rowValue }">
        <span>{{ formatOptionalDate(rowValue) }}</span>
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
            @click="startEdit(row)"
          >
            {{ t('Edit') }}
          </KButton>
          <KButton
            appearance="danger"
            size="small"
            @click="deleteProvider(row)"
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
import { computed, reactive, ref } from 'vue'
import AiGatewayNav from './AiGatewayNav.vue'
import { apiService } from '@/services/apiService'
import { useToaster } from '@/composables/useToaster'
import type { AiProvider, KongPageResponse } from './types'
import type { ProviderType } from './endpointTypes'
import { providerAuthConfig } from './endpointUtils'
import { useAiGatewayI18n } from './useAiGatewayI18n'
import {
  formatOptionalDate,
  formatTags,
  getErrorMessage,
  omitUndefined,
  parseTags,
} from './utils'

interface ProviderFormState {
  name: string
  providerType: string
  endpointUrl: string
  defaultModel: string
  apiKey: string
  enabled: boolean
  tags: string
}

defineOptions({
  name: 'AiGatewayProviders',
})

const toaster = useToaster()
const { l, locale, t } = useAiGatewayI18n()
const tableKey = ref(0)
const formVisible = ref(false)
const saving = ref(false)
const editingId = ref('')
const errorMessage = ref('')
const tableErrorMessage = ref('')
const deepSeekEndpoint = 'https://api.deepseek.com/v1/chat/completions'

const headers = computed(() => [
  { label: t('Name'), key: 'name' },
  { label: t('Type'), key: 'provider_type' },
  { label: t('Endpoint'), key: 'endpoint_url' },
  { label: t('Default Model'), key: 'default_model' },
  { label: t('Status'), key: 'enabled' },
  { label: t('Created'), key: 'created_at' },
  { label: t('Tags'), key: 'tags' },
  { hideLabel: true, key: 'actions' },
])

const form = reactive<ProviderFormState>({
  name: '',
  providerType: 'openai',
  endpointUrl: '',
  defaultModel: '',
  apiKey: '',
  enabled: true,
  tags: '',
})

const resetForm = () => {
  form.name = ''
  form.providerType = 'openai'
  form.endpointUrl = ''
  form.defaultModel = ''
  form.apiKey = ''
  form.enabled = true
  form.tags = ''
}

const fetchProviders = async (props: TableDataFetcherParams) => {
  tableErrorMessage.value = ''

  try {
    const { data } = await apiService.findAll<KongPageResponse<AiProvider>>('ai-providers', {
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
      l('Unable to load AI providers', '无法加载 AI 服务商'),
    )
  }
}

const startCreate = () => {
  errorMessage.value = ''
  editingId.value = ''
  resetForm()
  formVisible.value = true
}

const startEdit = (provider: AiProvider) => {
  errorMessage.value = ''
  editingId.value = provider.id
  form.name = provider.name
  form.providerType = provider.provider_type
  form.endpointUrl = provider.endpoint_url ?? ''
  form.defaultModel = provider.default_model ?? ''
  form.apiKey = ''
  form.enabled = provider.enabled
  form.tags = formatTags(provider.tags)
  formVisible.value = true
}

const cancelForm = () => {
  errorMessage.value = ''
  formVisible.value = false
  editingId.value = ''
  resetForm()
}

const submitProvider = async () => {
  saving.value = true
  errorMessage.value = ''

  try {
    const body = omitUndefined({
      name: form.name,
      provider_type: form.providerType,
      endpoint_url: form.endpointUrl || null,
      default_model: form.defaultModel || null,
      ...(!editingId.value ? { config: {} } : {}),
      enabled: form.enabled,
      tags: parseTags(form.tags) ?? (editingId.value ? null : undefined),
      ...(!editingId.value || form.apiKey.trim()
        ? {
          auth_config: providerAuthConfig(
            form.providerType as ProviderType,
            form.apiKey.trim(),
          ),
        }
        : {}),
    })

    if (editingId.value) {
      await apiService.patch(`ai-providers/${editingId.value}`, body)
      toaster.open({
        appearance: 'success',
        message: l(`Updated provider ${form.name}`, `已更新服务商 ${form.name}`),
      })
    } else {
      await apiService.post('ai-providers', body)
      toaster.open({
        appearance: 'success',
        message: l(`Created provider ${form.name}`, `已创建服务商 ${form.name}`),
      })
    }

    cancelForm()
    tableKey.value += 1
  } catch (err) {
    errorMessage.value = getErrorMessage(
      err,
      l('Unable to save AI provider', '无法保存 AI 服务商'),
    )
  } finally {
    saving.value = false
  }
}

const countProviderModels = async (providerId: string) => {
  const endpoint = `ai-providers/${providerId}/ai-models`
  const seenOffsets = new Set<string>()
  let count = 0
  let offset: string | number | undefined

  while (true) {
    const { data } = await apiService.findAll<KongPageResponse<unknown>>(endpoint, {
      size: 1000,
      ...(offset === undefined ? {} : { offset }),
    })

    count += data.data.length

    if (data.offset === null || data.offset === undefined) {
      return count
    }

    const offsetKey = String(data.offset)
    if (seenOffsets.has(offsetKey)) {
      throw new Error(`Pagination for ${endpoint} returned a repeated offset`)
    }

    seenOffsets.add(offsetKey)
    offset = data.offset
  }
}

const deleteProvider = async (provider: AiProvider) => {
  errorMessage.value = ''

  try {
    const modelCount = await countProviderModels(provider.id)

    if (modelCount > 0) {
      const modelLabel = modelCount === 1 ? 'model' : 'models'
      errorMessage.value = l(
        `Cannot delete provider "${provider.name}" while ${modelCount} dependent AI ${modelLabel} remain. Delete or reassign them first.`,
        `服务商“${provider.name}”仍被 ${modelCount} 个 AI 模型使用，请先删除模型或更换服务商。`,
      )
      return
    }

    if (!window.confirm(l(
      `Delete AI provider "${provider.name}"?`,
      `删除 AI 服务商“${provider.name}”？`,
    ))) {
      return
    }

    await apiService.delete(`ai-providers/${provider.id}`)
    toaster.open({
      appearance: 'success',
      message: l(`Deleted provider ${provider.name}`, `已删除服务商 ${provider.name}`),
    })
    tableKey.value += 1
  } catch (err) {
    errorMessage.value = getErrorMessage(
      err,
      l('Unable to delete AI provider', '无法删除 AI 服务商'),
    )
  }
}
</script>
