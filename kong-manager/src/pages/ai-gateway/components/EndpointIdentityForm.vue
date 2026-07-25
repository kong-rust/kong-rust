<template>
  <section class="ai-endpoint-section">
    <div class="ai-endpoint-section-heading">
      <span class="ai-endpoint-step">1</span>
      <div>
        <h3>{{ t('Endpoint details') }}</h3>
        <p>{{ t('Name the endpoint and choose the public path your applications will call.') }}</p>
      </div>
    </div>

    <div class="ai-gateway-form-grid">
      <div class="ai-gateway-form-field">
        <label for="ai-endpoint-name">{{ t('Endpoint name') }}</label>
        <input
          id="ai-endpoint-name"
          :value="displayName"
          required
          :placeholder="l('Customer support', '客户支持')"
          @input="updateDisplayName"
        >
      </div>

      <div class="ai-gateway-form-field">
        <label for="ai-endpoint-slug">{{ t('Path name') }}</label>
        <div class="ai-endpoint-path-input">
          <span>/ai/</span>
          <input
            id="ai-endpoint-slug"
            :value="slug"
            required
            :placeholder="l('customer-support', 'customer-support')"
            @input="updateSlug"
          >
          <span>/v1/chat/completions</span>
        </div>
      </div>
    </div>

    <div class="ai-endpoint-preview">
      <span>POST</span>
      <code>{{ previewPath }}</code>
    </div>

    <label class="ai-gateway-checkbox">
      <input
        :checked="enabled"
        type="checkbox"
        @change="updateEnabled"
      >
      {{ t('Enable this endpoint after publishing') }}
    </label>

    <label class="ai-gateway-checkbox">
      <input
        :checked="requireAuth"
        type="checkbox"
        @change="updateRequireAuth"
      >
      {{ t('Require a virtual key to call this endpoint') }}
    </label>
    <p class="ai-endpoint-hint">
      {{ t('Callers send the key as Authorization: Bearer, x-api-key, or X-AI-Key. Manage keys under Virtual Keys.') }}
    </p>
  </section>
</template>

<script setup lang="ts">
import { computed } from 'vue'
import { endpointPath, normalizeSlug } from '../endpointUtils'
import { useAiGatewayI18n } from '../useAiGatewayI18n'

const props = defineProps<{
  displayName: string
  slug: string
  enabled: boolean
  requireAuth: boolean
}>()

const emit = defineEmits<{
  'update:displayName': [value: string]
  'update:slug': [value: string]
  'update:enabled': [value: boolean]
  'update:requireAuth': [value: boolean]
}>()
const { l, t } = useAiGatewayI18n()

const previewPath = computed(() => endpointPath(normalizeSlug(props.slug) || 'your-endpoint'))

const updateDisplayName = (event: Event) => {
  emit('update:displayName', (event.target as HTMLInputElement).value)
}

const updateSlug = (event: Event) => {
  emit('update:slug', normalizeSlug((event.target as HTMLInputElement).value))
}

const updateEnabled = (event: Event) => {
  emit('update:enabled', (event.target as HTMLInputElement).checked)
}

const updateRequireAuth = (event: Event) => {
  emit('update:requireAuth', (event.target as HTMLInputElement).checked)
}
</script>
