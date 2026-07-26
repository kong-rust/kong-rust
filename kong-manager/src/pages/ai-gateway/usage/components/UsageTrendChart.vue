<template>
  <section
    class="ai-usage-chart"
    :aria-label="t('aiUsage.trend.title')"
  >
    <div class="ai-usage-section-header">
      <div>
        <h2>{{ t('aiUsage.trend.title') }}</h2>
        <p>{{ t('aiUsage.trend.coverageHint') }}</p>
      </div>
    </div>

    <div
      v-if="items.length"
      class="ai-usage-chart-legend"
    >
      <span aria-hidden="true" />
      <strong>{{ metricLabel }}</strong>
    </div>

    <div
      v-if="items.length"
      class="ai-usage-chart-canvas"
    >
      <svg
        :aria-label="chartAriaLabel"
        role="img"
        :viewBox="`0 0 ${width} ${height}`"
      >
        <line
          class="ai-usage-chart-axis"
          :x1="padding"
          :x2="width - padding"
          :y1="height - padding"
          :y2="height - padding"
        />
        <line
          class="ai-usage-chart-axis"
          :x1="padding"
          :x2="padding"
          :y1="padding"
          :y2="height - padding"
        />
        <path
          class="ai-usage-chart-line"
          :d="path"
          fill="none"
        />
        <g
          v-for="(point, index) in points"
          :key="point.key"
        >
          <g
            class="ai-usage-chart-point"
            :aria-label="point.ariaLabel"
            role="img"
            tabindex="0"
            @blur="activeIndex = null"
            @focus="activeIndex = index"
            @mouseenter="activeIndex = index"
            @mouseleave="activeIndex = null"
          >
            <circle
              :cx="point.x"
              :cy="point.y"
              r="5"
            />
            <title>{{ point.ariaLabel }}</title>
          </g>
        </g>
      </svg>

      <div
        v-if="activePoint"
        class="ai-usage-chart-tooltip"
        role="status"
      >
        <strong>{{ activePoint.label }}</strong>
        <span>{{ activePoint.displayValue }}</span>
        <small>{{ activePoint.coverage }}</small>
      </div>
    </div>

    <details
      v-if="items.length"
      class="ai-usage-chart-table"
    >
      <summary>{{ t('aiUsage.trend.table') }}</summary>
      <div class="ai-usage-table-scroll">
        <table>
          <thead>
            <tr>
              <th scope="col">
                {{ t('aiUsage.fields.time') }}
              </th>
              <th scope="col">
                {{ metricLabel }}
              </th>
              <th scope="col">
                {{ t('aiUsage.fields.coverage') }}
              </th>
            </tr>
          </thead>
          <tbody>
            <tr
              v-for="point in points"
              :key="point.key"
            >
              <th scope="row">
                {{ point.label }}
              </th>
              <td>{{ point.displayValue }}</td>
              <td>{{ point.coverage }}</td>
            </tr>
          </tbody>
        </table>
      </div>
    </details>
  </section>
</template>

<script setup lang="ts">
import { computed, ref } from 'vue'
import { useI18n } from '@/composables/useI18n'
import type {
  AiUsageBreakdownItem,
  AiUsageMetric,
  AiUsageTokenMetric,
} from '../aiUsageTypes'
import {
  formatCoverage,
  formatIntegerString,
  formatUsd,
} from '../aiUsageFormatters'

const props = defineProps<{
  items: AiUsageBreakdownItem[]
  metric: AiUsageMetric
  tokenMetric: AiUsageTokenMetric
}>()

const { t } = useI18n()
const width = 760
const height = 260
const padding = 36
const activeIndex = ref<number | null>(null)

const tokenAggregate = (item: AiUsageBreakdownItem) => {
  if (props.tokenMetric === 'prompt') {
    return item.metrics.prompt_tokens
  }
  if (props.tokenMetric === 'completion') {
    return item.metrics.completion_tokens
  }

  return item.metrics.total_tokens
}

const rawValue = (item: AiUsageBreakdownItem) => (
  props.metric === 'cost'
    ? item.metrics.cost_usd_calculable_sum
    : tokenAggregate(item).known_sum
)

const displayValue = (item: AiUsageBreakdownItem) => (
  props.metric === 'cost'
    ? formatUsd(item.metrics.cost_usd_calculable_sum)
    : formatIntegerString(tokenAggregate(item).known_sum)
)

const values = computed(() => props.items.map(item => {
  const numeric = Number(rawValue(item))

  return Number.isFinite(numeric) ? numeric : 0
}))

const maxValue = computed(() => Math.max(...values.value, 0))

const points = computed(() => props.items.map((item, index) => {
  const denominator = Math.max(props.items.length - 1, 1)
  const x = padding + (index / denominator) * (width - padding * 2)
  const ratio = maxValue.value > 0 ? (values.value[index] ?? 0) / maxValue.value : 0
  const y = height - padding - ratio * (height - padding * 2)
  const aggregate = tokenAggregate(item)
  const coverage = formatCoverage(aggregate.coverage)
  const label = item.label || item.bucket_start || t('aiUsage.labels.unknown')
  const value = displayValue(item)

  return {
    key: item.key || `${index}`,
    x,
    y,
    label,
    displayValue: value,
    coverage,
    ariaLabel: `${label}: ${value}; ${t('aiUsage.fields.coverage')} ${coverage}`,
  }
}))

const path = computed(() => points.value
  .map((point, index) => `${index === 0 ? 'M' : 'L'} ${point.x} ${point.y}`)
  .join(' '))

const activePoint = computed(() => (
  activeIndex.value === null ? null : points.value[activeIndex.value]
))

const metricLabel = computed(() => {
  if (props.metric === 'cost') {
    return t('aiUsage.fields.calculableCost')
  }

  return t(`aiUsage.fields.${props.tokenMetric}Tokens`)
})

const chartAriaLabel = computed(() => (
  `${t('aiUsage.trend.title')}: ${metricLabel.value}`
))
</script>
