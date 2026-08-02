<template>
  <section class="ai-endpoint-section">
    <div class="ai-endpoint-section-heading">
      <span class="ai-endpoint-step">4</span>
      <div>
        <h3>{{ t('Context compression') }}</h3>
        <p>{{ t('Reduce long prompts with Headroom and keep compressed content retrievable through transparent CCR.') }}</p>
      </div>
    </div>

    <label class="ai-gateway-checkbox">
      <input
        id="ai-endpoint-context-compression"
        :checked="modelValue.enabled"
        type="checkbox"
        @change="updateEnabled"
      >
      {{ t('Enable Headroom context compression and CCR') }}
    </label>

    <div
      v-if="modelValue.enabled"
      class="ai-gateway-form-grid"
    >
      <div class="ai-gateway-form-field">
        <label for="ai-context-min-tokens">{{ t('Minimum input tokens') }}</label>
        <input
          id="ai-context-min-tokens"
          :value="modelValue.minInputTokens"
          max="2147483647"
          min="0"
          required
          step="1"
          type="number"
          @input="updateMinInputTokens"
        >
      </div>

      <div class="ai-gateway-form-field">
        <label for="ai-context-max-bytes">{{ t('Maximum input bytes') }}</label>
        <input
          id="ai-context-max-bytes"
          :value="modelValue.maxInputBytes"
          max="16777216"
          min="1"
          required
          step="1"
          type="number"
          @input="updateMaxInputBytes"
        >
      </div>

      <div class="ai-gateway-form-field">
        <label for="ai-context-unavailable">{{ t('When Headroom is unavailable') }}</label>
        <select
          id="ai-context-unavailable"
          :value="modelValue.onUnavailable"
          @change="updateUnavailablePolicy"
        >
          <option value="pass_through">
            {{ t('Pass through to the provider') }}
          </option>
          <option value="reject">
            {{ t('Reject with 503') }}
          </option>
        </select>
      </div>

      <div class="ai-gateway-form-field ai-context-compression-metrics">
        <label class="ai-gateway-checkbox">
          <input
            :checked="modelValue.exposeMetricsHeaders"
            type="checkbox"
            @change="updateExposeMetricsHeaders"
          >
          {{ t('Expose stable token-savings response headers') }}
        </label>
      </div>
    </div>

    <p class="ai-endpoint-hint">
      {{ t('Only non-streaming OpenAI Responses and Anthropic Messages use transparent CCR in this release. OpenAI Chat, streaming, and constrained tool_choice requests bypass compression. Admission and rate limits use the original token estimate; the Headroom sidecar controls CCR retention TTL.') }}
    </p>
    <p class="ai-endpoint-hint">
      {{ t('This publisher currently creates an OpenAI Chat endpoint, so calls to its published URL bypass this policy. Attach the plugin to a Responses or Anthropic Messages route to activate compression.') }}
    </p>
  </section>
</template>

<script setup lang="ts">
import type {
  ContextCompressionDraft,
  ContextCompressionUnavailablePolicy,
} from '../endpointTypes'
import { useAiGatewayI18n } from '../useAiGatewayI18n'

const props = defineProps<{
  modelValue: ContextCompressionDraft
}>()

const emit = defineEmits<{
  'update:modelValue': [value: ContextCompressionDraft]
}>()

const { t } = useAiGatewayI18n()

const update = (value: Partial<ContextCompressionDraft>) => {
  emit('update:modelValue', { ...props.modelValue, ...value })
}

const updateEnabled = (event: Event) => {
  update({ enabled: (event.target as HTMLInputElement).checked })
}

const updateMinInputTokens = (event: Event) => {
  update({ minInputTokens: (event.target as HTMLInputElement).valueAsNumber })
}

const updateMaxInputBytes = (event: Event) => {
  update({ maxInputBytes: (event.target as HTMLInputElement).valueAsNumber })
}

const updateUnavailablePolicy = (event: Event) => {
  update({
    onUnavailable: (event.target as HTMLSelectElement)
      .value as ContextCompressionUnavailablePolicy,
  })
}

const updateExposeMetricsHeaders = (event: Event) => {
  update({ exposeMetricsHeaders: (event.target as HTMLInputElement).checked })
}
</script>
