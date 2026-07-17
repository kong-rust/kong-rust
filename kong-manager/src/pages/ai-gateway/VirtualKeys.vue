<template>
  <PageHeader title="AI Virtual Keys">
    <KButton
      :disabled="mutationPending || !!latestKey"
      @click="startCreate"
    >
      Create Virtual Key
    </KButton>
  </PageHeader>
  <AiGatewayNav />

  <KAlert
    appearance="warning"
    class="ai-gateway-alert"
  >
    Virtual keys currently store management metadata only. They are not wired into proxy
    authentication, rate-limit, or budget enforcement.
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
      This secret is shown once. Copy it, then dismiss it before creating or rotating another key.
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
        Copy Key
      </KButton>
      <KButton
        appearance="tertiary"
        type="button"
        @click="clearLatestKey"
      >
        Dismiss
      </KButton>
    </div>
  </section>

  <KCard
    v-if="formVisible"
    class="ai-gateway-form-card"
    :title="editingId ? 'Edit Virtual Key' : 'Create Virtual Key'"
  >
    <form
      class="ai-gateway-form"
      @submit.prevent="submitVirtualKey"
    >
      <div class="ai-gateway-form-grid">
        <div class="ai-gateway-form-field">
          <label for="ai-key-name">Name</label>
          <input
            id="ai-key-name"
            v-model.trim="form.name"
            required
          >
        </div>

        <div class="ai-gateway-form-field">
          <label for="ai-key-consumer">Consumer ID</label>
          <input
            id="ai-key-consumer"
            v-model.trim="form.consumerId"
          >
        </div>

        <div class="ai-gateway-form-field">
          <label for="ai-key-models">Allowed Models</label>
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
          Enabled
        </label>
      </div>

      <div class="ai-gateway-form-grid">
        <div class="ai-gateway-form-field">
          <label for="ai-key-tpm">TPM Limit</label>
          <input
            id="ai-key-tpm"
            v-model="form.tpmLimit"
            min="0"
            type="number"
          >
        </div>

        <div class="ai-gateway-form-field">
          <label for="ai-key-rpm">RPM Limit</label>
          <input
            id="ai-key-rpm"
            v-model="form.rpmLimit"
            min="0"
            type="number"
          >
        </div>

        <div class="ai-gateway-form-field">
          <label for="ai-key-budget">Budget Limit</label>
          <input
            id="ai-key-budget"
            v-model="form.budgetLimit"
            min="0"
            step="0.01"
            type="number"
          >
        </div>

        <div class="ai-gateway-form-field">
          <label for="ai-key-expires">Expires At</label>
          <input
            id="ai-key-expires"
            v-model="form.expiresAt"
            type="datetime-local"
          >
        </div>
      </div>

      <div class="ai-gateway-form-field">
        <label for="ai-key-tags">Tags</label>
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
          {{ mutationPending ? 'Saving...' : 'Save Virtual Key' }}
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

  <KCard class="ai-gateway-table-card">
    <KTable
      :key="tableKey"
      :headers="headers"
      :fetcher="fetchVirtualKeys"
      :error="!!tableErrorMessage"
      :error-state-message="tableErrorMessage"
      empty-state-title="No AI virtual keys"
      empty-state-message="Create virtual-key management metadata."
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
          {{ rowValue ? 'Enabled' : 'Disabled' }}
        </KBadge>
      </template>

      <template #actions="{ row }">
        <div class="ai-gateway-row-actions">
          <KButton
            appearance="secondary"
            size="small"
            :disabled="mutationPending"
            @click="startEdit(row)"
          >
            Edit
          </KButton>
          <KButton
            appearance="secondary"
            size="small"
            :disabled="mutationPending || !!latestKey"
            @click="rotateVirtualKey(row)"
          >
            Rotate
          </KButton>
          <KButton
            appearance="danger"
            size="small"
            :disabled="mutationPending"
            @click="deleteVirtualKey(row)"
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
import { reactive, ref } from 'vue'
import AiGatewayNav from './AiGatewayNav.vue'
import { apiService } from '@/services/apiService'
import { useToaster } from '@/composables/useToaster'
import type { AiVirtualKey, KongPageResponse } from './types'
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
const tableKey = ref(0)
const formVisible = ref(false)
const mutationPending = ref(false)
const editingId = ref('')
const errorMessage = ref('')
const tableErrorMessage = ref('')
const latestKey = ref('')
const latestKeyTitle = ref('')

const headers = [
  { label: 'Name', key: 'name' },
  { label: 'Prefix', key: 'key_prefix' },
  { label: 'Allowed Models', key: 'allowed_models' },
  { label: 'Rate Limits', key: 'limits' },
  { label: 'Budget Used / Limit', key: 'budget' },
  { label: 'Expires', key: 'expires_at' },
  { label: 'Status', key: 'enabled' },
  { hideLabel: true, key: 'actions' },
]

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
    tableErrorMessage.value = getErrorMessage(err, 'Unable to load AI virtual keys')
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
      toaster.open({ appearance: 'success', message: `Updated virtual key ${form.name}` })
    } else {
      const { data } = await apiService.post('ai-virtual-keys', body)
      const created = data as AiVirtualKey
      if (!created.key) {
        throw new Error('The server did not return the newly created virtual key')
      }

      latestKey.value = created.key
      latestKeyTitle.value = `Created virtual key ${created.name}`
      toaster.open({ appearance: 'success', message: `Created virtual key ${form.name}` })
    }

    cancelForm()
    tableKey.value += 1
  } catch (err) {
    errorMessage.value = getErrorMessage(err, 'Unable to save AI virtual key')
  } finally {
    mutationPending.value = false
  }
}

const rotateVirtualKey = async (virtualKey: AiVirtualKey) => {
  if (mutationPending.value || latestKey.value) {
    return
  }

  if (!window.confirm(`Rotate AI virtual key "${virtualKey.name}"?`)) {
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
    latestKeyTitle.value = `Rotated virtual key ${virtualKey.name}`
    toaster.open({ appearance: 'success', message: `Rotated virtual key ${virtualKey.name}` })
    tableKey.value += 1
  } catch (err) {
    errorMessage.value = getErrorMessage(err, 'Unable to rotate AI virtual key')
  } finally {
    mutationPending.value = false
  }
}

const deleteVirtualKey = async (virtualKey: AiVirtualKey) => {
  if (mutationPending.value) {
    return
  }

  if (!window.confirm(`Delete AI virtual key "${virtualKey.name}"?`)) {
    return
  }

  mutationPending.value = true
  errorMessage.value = ''

  try {
    await apiService.delete(`ai-virtual-keys/${virtualKey.id}`)
    toaster.open({ appearance: 'success', message: `Deleted virtual key ${virtualKey.name}` })
    tableKey.value += 1
  } catch (err) {
    errorMessage.value = getErrorMessage(err, 'Unable to delete AI virtual key')
  } finally {
    mutationPending.value = false
  }
}

const copyLatestKey = async () => {
  await navigator.clipboard.writeText(latestKey.value)
  toaster.open({ appearance: 'success', message: 'Copied virtual key' })
}

const clearLatestKey = () => {
  latestKey.value = ''
  latestKeyTitle.value = ''
}
</script>
