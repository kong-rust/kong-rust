<template>
  <KCard class="ai-usage-filters">
    <form @submit.prevent="apply">
      <div class="ai-usage-filter-grid">
        <div class="ai-gateway-form-field">
          <label for="ai-usage-range">{{ t('aiUsage.filters.range') }}</label>
          <select
            id="ai-usage-range"
            v-model="draft.range"
          >
            <option value="24h">
              {{ t('aiUsage.ranges.24h') }}
            </option>
            <option value="7d">
              {{ t('aiUsage.ranges.7d') }}
            </option>
            <option value="30d">
              {{ t('aiUsage.ranges.30d') }}
            </option>
            <option value="custom">
              {{ t('aiUsage.ranges.custom') }}
            </option>
          </select>
        </div>

        <template v-if="draft.range === 'custom'">
          <div class="ai-gateway-form-field">
            <label for="ai-usage-start">{{ t('aiUsage.filters.start') }}</label>
            <input
              id="ai-usage-start"
              v-model="customStart"
              required
              type="datetime-local"
            >
          </div>
          <div class="ai-gateway-form-field">
            <label for="ai-usage-end">{{ t('aiUsage.filters.end') }}</label>
            <input
              id="ai-usage-end"
              v-model="customEnd"
              required
              type="datetime-local"
            >
          </div>
        </template>

        <div class="ai-gateway-form-field">
          <label for="ai-usage-timezone">{{ t('aiUsage.filters.timezone') }}</label>
          <input
            id="ai-usage-timezone"
            v-model.trim="draft.timezone"
            required
            placeholder="Asia/Shanghai"
          >
        </div>

        <div
          v-for="field in primaryFields"
          :key="field.key"
          class="ai-gateway-form-field"
        >
          <label :for="`ai-usage-${field.key}`">{{ t(field.label) }}</label>
          <input
            :id="`ai-usage-${field.key}`"
            v-model.trim="draft[field.key]"
            :placeholder="t(field.placeholder)"
          >
        </div>

        <div
          v-for="field in primarySelects"
          :key="field.key"
          class="ai-gateway-form-field"
        >
          <label :for="`ai-usage-${field.key}`">{{ t(field.label) }}</label>
          <select
            :id="`ai-usage-${field.key}`"
            v-model="draft[field.key]"
          >
            <option value="">
              {{ t('aiUsage.filters.any') }}
            </option>
            <option
              v-for="option in field.options"
              :key="option"
              :value="option"
            >
              {{ optionLabel(option) }}
            </option>
          </select>
        </div>
      </div>

      <details class="ai-usage-advanced-filters">
        <summary>
          {{ t('aiUsage.filters.advanced') }}
          <span v-if="advancedFilterCount">({{ advancedFilterCount }})</span>
        </summary>
        <div class="ai-usage-filter-grid">
          <div
            v-for="field in advancedFields"
            :key="field.key"
            class="ai-gateway-form-field"
          >
            <label :for="`ai-usage-${field.key}`">{{ t(field.label) }}</label>
            <input
              :id="`ai-usage-${field.key}`"
              v-model.trim="draft[field.key]"
              :placeholder="field.uuid ? 'UUID' : ''"
            >
          </div>

          <div
            v-for="field in advancedSelects"
            :key="field.key"
            class="ai-gateway-form-field"
          >
            <label :for="`ai-usage-${field.key}`">{{ t(field.label) }}</label>
            <select
              :id="`ai-usage-${field.key}`"
              v-model="draft[field.key]"
            >
              <option value="">
                {{ t('aiUsage.filters.any') }}
              </option>
              <option
                v-for="option in field.options"
                :key="option"
                :value="option"
              >
                {{ optionLabel(option) }}
              </option>
            </select>
          </div>
        </div>
      </details>

      <p
        v-if="validationError"
        class="ai-usage-filter-error"
        role="alert"
      >
        {{ validationError }}
      </p>

      <div class="ai-gateway-form-actions">
        <KButton
          :disabled="loading"
          type="submit"
        >
          {{ loading ? t('aiUsage.actions.loading') : t('aiUsage.actions.apply') }}
        </KButton>
        <KButton
          appearance="secondary"
          :disabled="loading"
          type="button"
          @click="reset"
        >
          {{ t('aiUsage.actions.reset') }}
        </KButton>
      </div>
    </form>
  </KCard>
</template>

<script setup lang="ts">
import {
  computed,
  reactive,
  ref,
  watch,
} from 'vue'
import { useI18n } from '@/composables/useI18n'
import type {
  AiUsageApiFilterKey,
  AiUsageFilters,
} from '../aiUsageTypes'
import { statusLabel } from '../aiUsageFormatters'

type FilterKey = AiUsageApiFilterKey

interface TextField {
  key: FilterKey
  label: string
  placeholder: string
  uuid?: boolean
}

interface SelectField {
  key: FilterKey
  label: string
  options: string[]
}

const props = defineProps<{
  filters: AiUsageFilters
  loading: boolean
}>()

const emit = defineEmits<{
  apply: [filters: AiUsageFilters]
}>()

const { t } = useI18n()
const draft = reactive<AiUsageFilters>({ ...props.filters })
const customStart = ref('')
const customEnd = ref('')
const validationError = ref('')

const optionLabel = (value: string) => {
  const key = `aiUsage.values.${value}`
  const translated = t(key)

  return translated === key ? statusLabel(value) : translated
}

const primaryFields: TextField[] = [
  {
    key: 'request_id',
    label: 'aiUsage.filters.requestId',
    placeholder: 'aiUsage.filters.requestIdPlaceholder',
  },
  {
    key: 'requested_model',
    label: 'aiUsage.filters.requestedModel',
    placeholder: 'aiUsage.filters.modelPlaceholder',
  },
  {
    key: 'actual_model',
    label: 'aiUsage.filters.actualModel',
    placeholder: 'aiUsage.filters.modelPlaceholder',
  },
  {
    key: 'model_group',
    label: 'aiUsage.filters.modelGroup',
    placeholder: 'aiUsage.filters.modelGroupPlaceholder',
  },
  {
    key: 'provider_type',
    label: 'aiUsage.filters.providerType',
    placeholder: 'aiUsage.filters.providerTypePlaceholder',
  },
  {
    key: 'status_code',
    label: 'aiUsage.filters.statusCode',
    placeholder: 'aiUsage.filters.statusCodePlaceholder',
  },
]

const primarySelects: SelectField[] = [{
  key: 'outcome',
  label: 'aiUsage.filters.outcome',
  options: [
    'success',
    'gateway_rejected',
    'gateway_error',
    'upstream_error',
    'client_disconnected',
    'stream_interrupted',
  ],
}]

const advancedFields: TextField[] = [
  { key: 'route_id', label: 'aiUsage.filters.routeId', placeholder: '', uuid: true },
  { key: 'service_id', label: 'aiUsage.filters.serviceId', placeholder: '', uuid: true },
  { key: 'provider_id', label: 'aiUsage.filters.providerId', placeholder: '', uuid: true },
  { key: 'virtual_key_id', label: 'aiUsage.filters.virtualKeyId', placeholder: '', uuid: true },
  { key: 'consumer_id', label: 'aiUsage.filters.consumerId', placeholder: '', uuid: true },
]

const advancedSelects: SelectField[] = [
  { key: 'stream', label: 'aiUsage.filters.stream', options: ['true', 'false'] },
  {
    key: 'cache_status',
    label: 'aiUsage.filters.cacheStatus',
    options: ['not_configured', 'unavailable', 'bypass', 'miss', 'hit'],
  },
  {
    key: 'usage_source',
    label: 'aiUsage.filters.usageSource',
    options: ['provider', 'estimated', 'mixed', 'unavailable'],
  },
  {
    key: 'pricing_status',
    label: 'aiUsage.filters.pricingStatus',
    options: ['matched', 'unmatched', 'unsupported', 'not_applicable'],
  },
  {
    key: 'cost_status',
    label: 'aiUsage.filters.costStatus',
    options: ['calculated', 'estimated', 'not_incurred', 'unavailable'],
  },
]

const toLocalDateTime = (value: string) => {
  if (!value) {
    return ''
  }

  const date = new Date(value)
  if (Number.isNaN(date.getTime())) {
    return ''
  }

  const offset = date.getTimezoneOffset() * 60 * 1000

  return new Date(date.getTime() - offset).toISOString().slice(0, 16)
}

const syncDraft = () => {
  Object.assign(draft, props.filters)
  customStart.value = toLocalDateTime(props.filters.start)
  customEnd.value = toLocalDateTime(props.filters.end)
  validationError.value = ''
}

watch(() => props.filters, syncDraft, { deep: true, immediate: true })

const advancedFilterCount = computed(() => [
  ...advancedFields,
  ...advancedSelects,
].filter(field => Boolean(draft[field.key])).length)

const apply = () => {
  validationError.value = ''
  const next = { ...draft }

  if (next.range === 'custom') {
    const start = new Date(customStart.value)
    const end = new Date(customEnd.value)

    if (
      !customStart.value
      || !customEnd.value
      || Number.isNaN(start.getTime())
      || Number.isNaN(end.getTime())
      || start >= end
    ) {
      validationError.value = t('aiUsage.filters.invalidRange')
      return
    }

    const maxRange = 90 * 24 * 60 * 60 * 1000
    if (end.getTime() - start.getTime() > maxRange) {
      validationError.value = t('aiUsage.filters.rangeTooLong')
      return
    }

    next.start = start.toISOString()
    next.end = end.toISOString()
  } else {
    next.start = ''
    next.end = ''
  }

  emit('apply', next)
}

const reset = () => {
  const next: AiUsageFilters = {
    ...props.filters,
    request_id: '',
    route_id: '',
    service_id: '',
    provider_id: '',
    provider_type: '',
    requested_model: '',
    model_group: '',
    actual_model: '',
    virtual_key_id: '',
    consumer_id: '',
    status_code: '',
    outcome: '',
    stream: '',
    cache_status: '',
    usage_source: '',
    pricing_status: '',
    cost_status: '',
  }

  Object.assign(draft, next)
  emit('apply', next)
}
</script>
