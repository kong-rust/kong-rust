<template>
  <PageHeader :title="t('AI Virtual Keys')">
    <KButton
      data-tour="create-virtual-key"
      :disabled="mutationPending || !!latestKey"
      @click="startCreate"
    >
      {{ t('Create Virtual Key') }}
    </KButton>
  </PageHeader>
  <AiGatewayNav />

  <KAlert
    appearance="info"
    class="ai-gateway-alert"
  >
    {{ t('Keys authenticate proxy traffic on endpoints with the ai-key-auth plugin, and the allowed-models list is enforced. TPM, RPM, and budget limits are stored but not enforced yet.') }}
  </KAlert>

  <KAlert
    v-if="errorMessage"
    appearance="danger"
    class="ai-gateway-alert"
  >
    {{ errorMessage }}
  </KAlert>

  <section
    v-if="latestKey"
    class="ai-gateway-secret"
  >
    <strong>{{ latestKeyTitle }}</strong>
    <p>
      {{ t('This secret is shown once. Copy it, then dismiss it before creating or rotating another key.') }}
    </p>
    <input
      class="ai-gateway-mono"
      readonly
      :value="latestKey"
    >
    <div class="ai-gateway-key-actions">
      <KButton
        appearance="secondary"
        type="button"
        @click="copyLatestKey"
      >
        {{ t('Copy Key') }}
      </KButton>
      <KButton
        appearance="tertiary"
        type="button"
        @click="clearLatestKey"
      >
        {{ t('Dismiss') }}
      </KButton>
    </div>
  </section>

  <KCard
    v-if="formVisible"
    class="ai-gateway-form-card"
    :title="editingId ? t('Edit Virtual Key') : t('Create Virtual Key')"
  >
    <form
      class="ai-gateway-form"
      @submit.prevent="submitVirtualKey"
    >
      <div class="ai-gateway-form-grid">
        <div class="ai-gateway-form-field">
          <label for="ai-key-name">{{ t('Name') }}</label>
          <input
            id="ai-key-name"
            v-model.trim="form.name"
            required
          >
        </div>

        <div class="ai-gateway-form-field">
          <label for="ai-key-consumer">{{ t('Consumer ID') }}</label>
          <input
            id="ai-key-consumer"
            v-model.trim="form.consumerId"
          >
        </div>

        <div class="ai-gateway-form-field">
          <label for="ai-key-models">{{ t('Allowed Models') }}</label>
          <input
            id="ai-key-models"
            v-model="form.allowedModels"
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
          <label for="ai-key-tpm">{{ t('TPM Limit') }}</label>
          <input
            id="ai-key-tpm"
            v-model="form.tpmLimit"
            min="0"
            type="number"
          >
        </div>

        <div class="ai-gateway-form-field">
          <label for="ai-key-rpm">{{ t('RPM Limit') }}</label>
          <input
            id="ai-key-rpm"
            v-model="form.rpmLimit"
            min="0"
            type="number"
          >
        </div>

        <div class="ai-gateway-form-field">
          <label for="ai-key-budget">{{ t('Budget Limit') }}</label>
          <input
            id="ai-key-budget"
            v-model="form.budgetLimit"
            min="0"
            step="0.01"
            type="number"
          >
        </div>

        <div class="ai-gateway-form-field">
          <label for="ai-key-expires">{{ t('Expires At') }}</label>
          <input
            id="ai-key-expires"
            v-model="form.expiresAt"
            type="datetime-local"
          >
        </div>
      </div>

      <div class="ai-gateway-form-field">
        <label for="ai-key-tags">{{ t('Tags') }}</label>
        <input
          id="ai-key-tags"
          v-model="form.tags"
        >
      </div>

      <div class="ai-gateway-form-actions">
        <KButton
          type="submit"
          :disabled="mutationPending"
        >
          {{ mutationPending ? t('Saving...') : t('Save Virtual Key') }}
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
      :fetcher="fetchVirtualKeys"
      :error="!!tableErrorMessage"
      :error-state-message="tableErrorMessage"
      :empty-state-title="t('No AI virtual keys')"
      :empty-state-message="t('Create virtual-key management metadata.')"
      pagination-offset
    >
      <template #name="{ rowValue }">
        <strong>{{ rowValue }}</strong>
      </template>

      <template #key_prefix="{ rowValue }">
        <span class="ai-gateway-mono">{{ rowValue }}</span>
      </template>

      <template #allowed_models="{ rowValue }">
        <div
          v-if="rowValue?.length"
          class="ai-gateway-badge-list"
        >
          <KBadge
            v-for="model in rowValue"
            :key="model"
            appearance="neutral"
          >
            {{ model }}
          </KBadge>
        </div>
        <span v-else>-</span>
      </template>

      <template #limits="{ row }">
        <span>{{ row.tpm_limit ?? '-' }} TPM / {{ row.rpm_limit ?? '-' }} RPM</span>
      </template>

      <template #budget="{ row }">
        <span>{{ row.budget_used ?? 0 }} / {{ row.budget_limit ?? '-' }}</span>
      </template>

      <template #expires_at="{ rowValue }">
        <span>{{ formatOptionalDate(rowValue) }}</span>
      </template>

      <template #enabled="{ rowValue }">
        <KBadge :appearance="rowValue ? 'success' : 'neutral'">
          {{ rowValue ? t('Enabled') : t('Disabled') }}
        </KBadge>
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
            size="small"
            :disabled="mutationPending"
            @click="startEdit(row)"
          >
            {{ t('Edit') }}
          </KButton>
          <KButton
            appearance="secondary"
            size="small"
            :disabled="mutationPending || !!latestKey"
            @click="rotateVirtualKey(row)"
          >
            {{ t('Rotate') }}
          </KButton>
          <KButton
            appearance="danger"
            size="small"
            :disabled="mutationPending"
            @click="deleteVirtualKey(row)"
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
import { useRouter } from 'vue-router'
import AiGatewayNav from './AiGatewayNav.vue'
import { apiService } from '@/services/apiService'
import { useToaster } from '@/composables/useToaster'
import type { AiVirtualKey, KongPageResponse } from './types'
import { useAiGatewayI18n } from './useAiGatewayI18n'
import {
  formatOptionalDate,
  formatTags,
  fromLocalDateTimeInput,
  getErrorMessage,
  omitUndefined,
  parseOptionalFloat,
  parseOptionalInt,
  parseTags,
  toLocalDateTimeInput,
} from './utils'

interface VirtualKeyFormState {
  name: string
  consumerId: string
  allowedModels: string
  tpmLimit: string | number
  rpmLimit: string | number
  budgetLimit: string | number
  expiresAt: string
  enabled: boolean
  tags: string
}

defineOptions({
  name: 'AiGatewayVirtualKeys',
})

const toaster = useToaster()
const router = useRouter()
const { l, locale, t } = useAiGatewayI18n()
const tableKey = ref(0)
const formVisible = ref(false)
const mutationPending = ref(false)
const editingId = ref('')
const errorMessage = ref('')
const tableErrorMessage = ref('')
const latestKey = ref('')
const latestKeyTitle = ref('')

const headers = computed(() => [
  { label: t('Name'), key: 'name' },
  { label: locale.value === 'zh-CN' ? '前缀' : 'Prefix', key: 'key_prefix' },
  { label: t('Allowed Models'), key: 'allowed_models' },
  { label: locale.value === 'zh-CN' ? '速率限制' : 'Rate Limits', key: 'limits' },
  { label: locale.value === 'zh-CN' ? '已用 / 预算上限' : 'Budget Used / Limit', key: 'budget' },
  { label: locale.value === 'zh-CN' ? '过期时间' : 'Expires', key: 'expires_at' },
  { label: t('Status'), key: 'enabled' },
  { hideLabel: true, key: 'actions' },
])

const form = reactive<VirtualKeyFormState>({
  name: '',
  consumerId: '',
  allowedModels: '',
  tpmLimit: '',
  rpmLimit: '',
  budgetLimit: '',
  expiresAt: '',
  enabled: true,
  tags: '',
})

const resetForm = () => {
  form.name = ''
  form.consumerId = ''
  form.allowedModels = ''
  form.tpmLimit = ''
  form.rpmLimit = ''
  form.budgetLimit = ''
  form.expiresAt = ''
  form.enabled = true
  form.tags = ''
}

const viewUsage = (virtualKey: AiVirtualKey) => {
  void router.push({
    name: 'ai-usage-overview',
    query: {
      range: '24h',
      timezone: Intl.DateTimeFormat().resolvedOptions().timeZone || 'UTC',
      virtual_key_id: virtualKey.id,
    },
  })
}

const fetchVirtualKeys = async (props: TableDataFetcherParams) => {
  tableErrorMessage.value = ''

  try {
    const { data } = await apiService.findAll<KongPageResponse<AiVirtualKey>>('ai-virtual-keys', {
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
      l('Unable to load AI virtual keys', '无法加载 AI 虚拟密钥'),
    )
  }
}

const startCreate = () => {
  if (mutationPending.value || latestKey.value) {
    return
  }

  errorMessage.value = ''
  editingId.value = ''
  resetForm()
  formVisible.value = true
}

const startEdit = (virtualKey: AiVirtualKey) => {
  if (mutationPending.value) {
    return
  }

  errorMessage.value = ''
  editingId.value = virtualKey.id
  form.name = virtualKey.name
  form.consumerId = virtualKey.consumer_id ?? ''
  form.allowedModels = virtualKey.allowed_models?.join(', ') ?? ''
  form.tpmLimit = virtualKey.tpm_limit === null || virtualKey.tpm_limit === undefined ? '' : String(virtualKey.tpm_limit)
  form.rpmLimit = virtualKey.rpm_limit === null || virtualKey.rpm_limit === undefined ? '' : String(virtualKey.rpm_limit)
  form.budgetLimit = virtualKey.budget_limit === null || virtualKey.budget_limit === undefined ? '' : String(virtualKey.budget_limit)
  form.expiresAt = toLocalDateTimeInput(virtualKey.expires_at)
  form.enabled = virtualKey.enabled
  form.tags = formatTags(virtualKey.tags)
  formVisible.value = true
}

const cancelForm = () => {
  errorMessage.value = ''
  formVisible.value = false
  editingId.value = ''
  resetForm()
}

const optionalFieldValue = <T>(value: T | undefined) => {
  if (value !== undefined) {
    return value
  }

  // PATCH must send an explicit null to clear a previously configured limit.
  return editingId.value ? null : undefined
}

const submitVirtualKey = async () => {
  if (mutationPending.value || (!editingId.value && latestKey.value)) {
    return
  }

  mutationPending.value = true
  errorMessage.value = ''

  try {
    const body = omitUndefined({
      name: form.name,
      consumer_id: form.consumerId || null,
      allowed_models: parseTags(form.allowedModels) ?? [],
      tpm_limit: optionalFieldValue(parseOptionalInt(form.tpmLimit, 'TPM limit')),
      rpm_limit: optionalFieldValue(parseOptionalInt(form.rpmLimit, 'RPM limit')),
      budget_limit: optionalFieldValue(parseOptionalFloat(form.budgetLimit, 'Budget limit')),
      expires_at: optionalFieldValue(fromLocalDateTimeInput(form.expiresAt)),
      enabled: form.enabled,
      tags: optionalFieldValue(parseTags(form.tags)),
    })

    if (editingId.value) {
      await apiService.patch(`ai-virtual-keys/${editingId.value}`, body)
      toaster.open({
        appearance: 'success',
        message: l(`Updated virtual key ${form.name}`, `已更新虚拟密钥 ${form.name}`),
      })
    } else {
      const { data } = await apiService.post('ai-virtual-keys', body)
      const created = data as AiVirtualKey
      if (!created.key) {
        throw new Error('The server did not return the newly created virtual key')
      }

      latestKey.value = created.key
      latestKeyTitle.value = l(
        `Created virtual key ${created.name}`,
        `已创建虚拟密钥 ${created.name}`,
      )
      toaster.open({
        appearance: 'success',
        message: l(`Created virtual key ${form.name}`, `已创建虚拟密钥 ${form.name}`),
      })
    }

    cancelForm()
    tableKey.value += 1
  } catch (err) {
    errorMessage.value = getErrorMessage(
      err,
      l('Unable to save AI virtual key', '无法保存 AI 虚拟密钥'),
    )
  } finally {
    mutationPending.value = false
  }
}

const rotateVirtualKey = async (virtualKey: AiVirtualKey) => {
  if (mutationPending.value || latestKey.value) {
    return
  }

  if (!window.confirm(l(
    `Rotate AI virtual key "${virtualKey.name}"?`,
    `轮换 AI 虚拟密钥“${virtualKey.name}”？`,
  ))) {
    return
  }

  mutationPending.value = true
  errorMessage.value = ''

  try {
    const { data } = await apiService.post(`ai-virtual-keys/${virtualKey.id}/rotate`)
    const rotated = data as AiVirtualKey

    if (!rotated.key) {
      throw new Error('The server did not return the rotated virtual key')
    }

    latestKey.value = rotated.key
    latestKeyTitle.value = l(
      `Rotated virtual key ${virtualKey.name}`,
      `已轮换虚拟密钥 ${virtualKey.name}`,
    )
    toaster.open({
      appearance: 'success',
      message: l(`Rotated virtual key ${virtualKey.name}`, `已轮换虚拟密钥 ${virtualKey.name}`),
    })
    tableKey.value += 1
  } catch (err) {
    errorMessage.value = getErrorMessage(
      err,
      l('Unable to rotate AI virtual key', '无法轮换 AI 虚拟密钥'),
    )
  } finally {
    mutationPending.value = false
  }
}

const deleteVirtualKey = async (virtualKey: AiVirtualKey) => {
  if (mutationPending.value) {
    return
  }

  if (!window.confirm(l(
    `Delete AI virtual key "${virtualKey.name}"?`,
    `删除 AI 虚拟密钥“${virtualKey.name}”？`,
  ))) {
    return
  }

  mutationPending.value = true
  errorMessage.value = ''

  try {
    await apiService.delete(`ai-virtual-keys/${virtualKey.id}`)
    toaster.open({
      appearance: 'success',
      message: l(`Deleted virtual key ${virtualKey.name}`, `已删除虚拟密钥 ${virtualKey.name}`),
    })
    tableKey.value += 1
  } catch (err) {
    errorMessage.value = getErrorMessage(
      err,
      l('Unable to delete AI virtual key', '无法删除 AI 虚拟密钥'),
    )
  } finally {
    mutationPending.value = false
  }
}

const copyLatestKey = async () => {
  await navigator.clipboard.writeText(latestKey.value)
  toaster.open({ appearance: 'success', message: l('Copied virtual key', '已复制虚拟密钥') })
}

const clearLatestKey = () => {
  latestKey.value = ''
  latestKeyTitle.value = ''
}
</script>
