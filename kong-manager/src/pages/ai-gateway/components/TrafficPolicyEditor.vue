<template>
  <section class="ai-endpoint-section">
    <div class="ai-endpoint-section-heading">
      <span class="ai-endpoint-step">3</span>
      <div>
        <h3>{{ t('Traffic') }}</h3>
        <p v-if="models.length === 1">
          {{ t('All requests go to this model. Add another model to split traffic.') }}
        </p>
        <p v-else>
          {{ t('Set a relative weight for each model. Each weight can be up to 10000.') }}
        </p>
      </div>
    </div>

    <div
      v-if="models.length === 1"
      class="ai-endpoint-single-model"
    >
      <strong>{{ models[0]?.modelName || t('Selected model') }}</strong>
      <span>{{ t('100% of traffic') }}</span>
    </div>

    <div
      v-else
      class="ai-endpoint-traffic-list"
    >
      <label
        v-for="(model, index) in models"
        :key="model.clientId"
        class="ai-endpoint-traffic-row"
      >
        <span>{{ model.modelName || `Model ${index + 1}` }}</span>
        <input
          :aria-label="`Traffic for ${model.modelName || `Model ${index + 1}`}`"
          :max="maxModelWeight"
          min="0"
          required
          step="1"
          :value="model.weight"
          type="number"
          @input="updateWeight(index, $event)"
        >
        <span>{{ t('Weight') }}</span>
      </label>

      <div :class="['ai-endpoint-traffic-total', { invalid: invalidWeights }]">
        {{ t('Total weight') }}: <strong>{{ total }}</strong>
      </div>
    </div>
  </section>
</template>

<script setup lang="ts">
import { computed, watch } from 'vue'
import type { EndpointModelDraft } from '../endpointTypes'
import { maxModelWeight } from '../endpointUtils'
import { useAiGatewayI18n } from '../useAiGatewayI18n'

const props = defineProps<{
  models: EndpointModelDraft[]
}>()

const emit = defineEmits<{
  'update:models': [value: EndpointModelDraft[]]
}>()
const { t } = useAiGatewayI18n()

const total = computed(() => props.models.reduce((sum, model) => sum + Number(model.weight), 0))
const invalidWeights = computed(() => (
  total.value <= 0
  || props.models.some(model => (
    !Number.isInteger(Number(model.weight))
    || Number(model.weight) < 0
    || Number(model.weight) > maxModelWeight
  ))
))

watch(
  () => props.models.length,
  length => {
    const model = props.models[0]

    if (length === 1 && model && model.weight !== 100) {
      emit('update:models', [{ ...model, weight: 100 }])
    }
  },
)

const updateWeight = (index: number, event: Event) => {
  const value = Number((event.target as HTMLInputElement).value)
  const models = props.models.map(model => ({ ...model }))
  const model = models[index]

  if (!model) {
    return
  }

  model.weight = Number.isFinite(value) ? value : 0
  emit('update:models', models)
}
</script>
