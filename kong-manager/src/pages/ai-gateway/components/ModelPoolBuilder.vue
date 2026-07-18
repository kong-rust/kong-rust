<template>
  <section class="ai-endpoint-section">
    <div class="ai-endpoint-section-heading">
      <span class="ai-endpoint-step">2</span>
      <div>
        <h3>{{ t('Choose models') }}</h3>
        <p>{{ t('Select a saved provider connection or add one here. Credentials stay with the provider.') }}</p>
      </div>
    </div>

    <div class="ai-endpoint-model-list">
      <article
        v-for="(model, index) in models"
        :key="model.clientId"
        class="ai-endpoint-model-card"
      >
        <div class="ai-endpoint-model-heading">
          <strong>{{ modelLabel(index) }}</strong>
          <KButton
            v-if="models.length > 1"
            appearance="tertiary"
            size="small"
            type="button"
            @click="removeModel(index)"
          >
            {{ t('Remove') }}
          </KButton>
        </div>

        <div class="ai-gateway-form-grid">
          <div class="ai-gateway-form-field">
            <label :for="`ai-model-connection-mode-${model.clientId}`">{{ t('Provider connection') }}</label>
            <select
              :id="`ai-model-connection-mode-${model.clientId}`"
              :value="model.providerMode"
              @change="updateField(index, 'providerMode', eventValue($event))"
            >
              <option value="existing">
                {{ t('Use a saved connection') }}
              </option>
              <option value="new">
                {{ t('Add a new connection') }}
              </option>
            </select>
          </div>

          <div
            v-if="model.providerMode === 'existing'"
            class="ai-gateway-form-field"
          >
            <label :for="`ai-model-provider-${model.clientId}`">{{ t('Provider') }}</label>
            <select
              :id="`ai-model-provider-${model.clientId}`"
              :value="model.providerId"
              required
              @change="updateField(index, 'providerId', eventValue($event))"
            >
              <option
                disabled
                value=""
              >
                Select a provider
              </option>
              <option
                v-for="provider in providers"
                :key="provider.id"
                :value="provider.id"
              >
                {{ provider.name }} · {{ providerLabels[provider.provider_type as ProviderType] || provider.provider_type }}
              </option>
            </select>
            <span
              v-if="providers.length === 0"
              class="ai-gateway-muted"
            >
              {{ t('No saved connections yet. Choose “Add a new connection”.') }}
            </span>
          </div>

          <template v-else>
            <div class="ai-gateway-form-field">
              <label :for="`ai-model-provider-name-${model.clientId}`">{{ t('Connection name') }}</label>
              <input
                :id="`ai-model-provider-name-${model.clientId}`"
                :value="model.providerName"
                required
                placeholder="Production OpenAI"
                @input="updateField(index, 'providerName', eventValue($event))"
              >
            </div>

            <div class="ai-gateway-form-field">
              <label :for="`ai-model-provider-type-${model.clientId}`">{{ t('Provider') }}</label>
              <select
                :id="`ai-model-provider-type-${model.clientId}`"
                :value="model.providerType"
                required
                @change="updateField(index, 'providerType', eventValue($event))"
              >
                <option
                  v-for="(label, value) in providerLabels"
                  :key="value"
                  :value="value"
                >
                  {{ label }}
                </option>
              </select>
            </div>

            <div class="ai-gateway-form-field">
              <label :for="`ai-model-api-key-${model.clientId}`">
                {{ t('API key') }}
                <span v-if="model.providerType === 'openai_compat'">{{ locale === 'zh-CN' ? '（可选）' : '(optional)' }}</span>
              </label>
              <input
                :id="`ai-model-api-key-${model.clientId}`"
                :value="model.apiKey"
                autocomplete="new-password"
                :required="model.providerType !== 'openai_compat'"
                type="password"
                @input="updateField(index, 'apiKey', eventValue($event))"
              >
            </div>

            <div
              v-if="model.providerType === 'openai_compat'"
              class="ai-gateway-form-field"
            >
              <label :for="`ai-model-endpoint-url-${model.clientId}`">{{ t('Service URL') }}</label>
              <input
                :id="`ai-model-endpoint-url-${model.clientId}`"
                :value="model.endpointUrl"
                placeholder="http://localhost:11434"
                required
                type="url"
                @input="updateField(index, 'endpointUrl', eventValue($event))"
              >
            </div>
          </template>

          <div class="ai-gateway-form-field">
            <label :for="`ai-model-name-${model.clientId}`">{{ t('Model name') }}</label>
            <input
              :id="`ai-model-name-${model.clientId}`"
              :value="model.modelName"
              placeholder="gpt-4o"
              required
              @input="updateField(index, 'modelName', eventValue($event))"
            >
          </div>
        </div>
      </article>
    </div>

    <KButton
      appearance="secondary"
      type="button"
      @click="addModel"
    >
      {{ t('Add another model') }}
    </KButton>
  </section>
</template>

<script setup lang="ts">
import type { AiProvider } from '../types'
import type { EndpointModelDraft, ProviderType } from '../endpointTypes'
import { newModelDraft, providerLabels } from '../endpointUtils'
import { useAiGatewayI18n } from '../useAiGatewayI18n'

const props = defineProps<{
  models: EndpointModelDraft[]
  providers: AiProvider[]
}>()

const emit = defineEmits<{
  'update:models': [value: EndpointModelDraft[]]
}>()
const { locale, t } = useAiGatewayI18n()
const modelLabel = (index: number) => {
  if (props.models.length === 1) {
    return t('Model')
  }

  return locale.value === 'zh-CN' ? `模型 ${index + 1}` : `Model ${index + 1}`
}

const eventValue = (event: Event) => (event.target as HTMLInputElement | HTMLSelectElement).value

const updateField = (
  index: number,
  field: keyof EndpointModelDraft,
  value: string,
) => {
  const models = props.models.map(model => ({ ...model }))
  const model = models[index]

  if (!model) {
    return
  }

  if (field === 'providerMode') {
    model.providerMode = value as EndpointModelDraft['providerMode']
  } else if (field === 'providerType') {
    model.providerType = value as ProviderType
  } else if (field !== 'weight' && field !== 'clientId') {
    model[field] = value
  }

  emit('update:models', models)
}

const addModel = () => {
  const model = newModelDraft()

  model.providerId = props.providers[0]?.id ?? ''
  model.providerMode = props.providers.length ? 'existing' : 'new'
  emit('update:models', [...props.models, model])
}

const removeModel = (index: number) => {
  const models = props.models.filter((_model, modelIndex) => modelIndex !== index)

  const remainingModel = models[0]

  if (models.length === 1 && remainingModel) {
    models[0] = { ...remainingModel, weight: 100 }
  }

  emit('update:models', models)
}
</script>
