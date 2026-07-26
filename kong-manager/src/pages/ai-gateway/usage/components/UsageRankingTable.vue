<template>
  <section class="ai-usage-ranking">
    <div class="ai-usage-section-header">
      <h2>{{ title }}</h2>
    </div>

    <div class="ai-usage-table-scroll">
      <table>
        <thead>
          <tr>
            <th scope="col">
              {{ t('aiUsage.fields.name') }}
            </th>
            <th scope="col">
              {{ t('aiUsage.fields.requests') }}
            </th>
            <th scope="col">
              {{ t('aiUsage.fields.totalTokens') }}
            </th>
            <th scope="col">
              {{ t('aiUsage.fields.calculableCost') }}
            </th>
            <th scope="col">
              {{ t('aiUsage.fields.coverage') }}
            </th>
          </tr>
        </thead>
        <tbody>
          <tr
            v-for="item in rows"
            :key="item.key ?? (item.is_other ? 'other' : 'unassociated')"
          >
            <th scope="row">
              <button
                v-if="!item.is_other && drillFilter(item)"
                class="ai-usage-link-button"
                type="button"
                @click="emit('drill', drillFilter(item)!)"
              >
                {{ itemLabel(item) }}
              </button>
              <span v-else>{{ itemLabel(item) }}</span>
            </th>
            <td>{{ item.metrics.requests }}</td>
            <td>{{ formatIntegerString(item.metrics.total_tokens.known_sum) }}</td>
            <td>{{ formatUsd(item.metrics.cost_usd_calculable_sum) }}</td>
            <td>{{ formatCoverage(item.metrics.cost_calculable_coverage) }}</td>
          </tr>
          <tr v-if="rows.length === 0">
            <td colspan="5">
              {{ t('aiUsage.empty.noRanking') }}
            </td>
          </tr>
        </tbody>
      </table>
    </div>
  </section>
</template>

<script setup lang="ts">
import { computed } from 'vue'
import { useI18n } from '@/composables/useI18n'
import type {
  AiUsageApiFilterKey,
  AiUsageBreakdown,
  AiUsageBreakdownItem,
} from '../aiUsageTypes'
import {
  formatCoverage,
  formatIntegerString,
  formatUsd,
} from '../aiUsageFormatters'

const props = defineProps<{
  title: string
  breakdown: AiUsageBreakdown | null
}>()

const emit = defineEmits<{
  drill: [value: Partial<Record<AiUsageApiFilterKey, string>>]
}>()

const { t } = useI18n()

const rows = computed(() => {
  if (!props.breakdown) {
    return []
  }

  return [
    ...props.breakdown.items,
    ...(props.breakdown.other ? [props.breakdown.other] : []),
  ]
})

const itemLabel = (item: AiUsageBreakdownItem) => {
  if (item.is_other) {
    return t('aiUsage.labels.other')
  }

  return item.label
    || item.dimension?.name
    || item.dimension?.prefix
    || t('aiUsage.labels.unassociated')
}

const drillFilter = (
  item: AiUsageBreakdownItem,
): Partial<Record<AiUsageApiFilterKey, string>> | null => {
  if (!props.breakdown || item.is_other) {
    return null
  }

  if (props.breakdown.type === 'actual_model') {
    const actualModel = item.dimension?.name || item.label
    if (!actualModel) {
      return null
    }

    return {
      actual_model: actualModel,
      ...(item.dimension?.type ? { provider_type: item.dimension.type } : {}),
    }
  }

  if (props.breakdown.type === 'virtual_key' && item.dimension?.id) {
    return { virtual_key_id: item.dimension.id }
  }

  return null
}
</script>
