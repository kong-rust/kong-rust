<template>
  <section class="ai-usage-page">
    <KAlert
      v-if="meta.ephemeral"
      appearance="warning"
      class="ai-gateway-alert"
    >
      <strong>{{ t('aiUsage.mode.dblessTitle') }}</strong>
      {{
        t('aiUsage.mode.dblessBody', {
          node: meta.node_id || t('aiUsage.labels.unknown'),
          capacity: meta.capacity ?? t('aiUsage.labels.unknown'),
          earliest: formatTimestamp(meta.earliest_available_at, filters.timezone),
        })
      }}
    </KAlert>

    <div
      v-if="state === 'loading'"
      class="ai-usage-state"
      role="status"
    >
      <span class="ai-usage-spinner" />
      <h2>{{ t('aiUsage.states.loadingTitle') }}</h2>
      <p>{{ t('aiUsage.states.loadingBody') }}</p>
    </div>

    <div
      v-else-if="state === 'unsupported'"
      class="ai-usage-state"
    >
      <h2>{{ t('aiUsage.states.unsupportedTitle') }}</h2>
      <p>{{ t('aiUsage.states.unsupportedBody') }}</p>
    </div>

    <div
      v-else-if="state === 'snapshot-expired'"
      class="ai-usage-state"
    >
      <h2>{{ t('aiUsage.states.expiredTitle') }}</h2>
      <p>{{ t('aiUsage.states.expiredBody') }}</p>
      <KButton @click="retry">
        {{ t('aiUsage.actions.refresh') }}
      </KButton>
    </div>

    <div
      v-else-if="state === 'error'"
      class="ai-usage-state"
    >
      <h2>{{ t('aiUsage.states.errorTitle') }}</h2>
      <p>{{ errorMessage }}</p>
      <KButton @click="retry">
        {{ t('aiUsage.actions.retry') }}
      </KButton>
    </div>

    <div
      v-else-if="state === 'empty-window' || state === 'empty-filter'"
      class="ai-usage-state"
    >
      <h2>
        {{
          state === 'empty-filter'
            ? t('aiUsage.states.emptyFilterTitle')
            : t('aiUsage.states.emptyWindowTitle')
        }}
      </h2>
      <p>
        {{
          state === 'empty-filter'
            ? t('aiUsage.states.emptyFilterBody')
            : t('aiUsage.states.emptyWindowBody')
        }}
      </p>
    </div>

    <template v-else-if="totals">
      <div class="ai-usage-kpi-grid">
        <KCard class="ai-usage-kpi">
          <span>{{ t('aiUsage.fields.calculableCost') }}</span>
          <strong>{{ formatUsd(totals.cost_usd_calculable_sum) }}</strong>
          <small>
            {{
              t('aiUsage.kpi.coverage', {
                coverage: formatCoverage(totals.cost_calculable_coverage),
              })
            }}
          </small>
        </KCard>

        <KCard class="ai-usage-kpi">
          <span>{{ t('aiUsage.fields.requests') }}</span>
          <strong>{{ formatIntegerString(totals.requests) }}</strong>
          <small>
            {{
              t('aiUsage.kpi.successFailure', {
                success: totals.successful_requests,
                failed: totals.failed_requests,
              })
            }}
          </small>
        </KCard>

        <KCard
          v-for="token in tokenCards"
          :key="token.key"
          class="ai-usage-kpi"
        >
          <span>{{ token.label }}</span>
          <strong>{{ formatTokenAggregate(token.aggregate) }}</strong>
          <small>
            {{
              t('aiUsage.kpi.knownCoverage', {
                unknown: token.aggregate.unknown_requests,
                coverage: formatCoverage(token.aggregate.coverage),
              })
            }}
          </small>
        </KCard>

        <KCard class="ai-usage-kpi">
          <span>{{ t('aiUsage.fields.estimatedRatio') }}</span>
          <strong>{{ formatCoverage(totals.estimated_usage_ratio) }}</strong>
          <small>{{ t('aiUsage.kpi.estimatedHint') }}</small>
        </KCard>

        <KCard class="ai-usage-kpi">
          <span>{{ t('aiUsage.fields.pricingCoverage') }}</span>
          <strong>{{ formatCoverage(totals.pricing_coverage) }}</strong>
          <small>
            {{
              t('aiUsage.kpi.unpriced', {
                count: totals.pricing_status.unmatched + totals.pricing_status.unsupported,
              })
            }}
          </small>
        </KCard>

        <template v-if="compressionVisible && compression">
          <KCard class="ai-usage-kpi">
            <span>{{ t('aiUsage.fields.contextCompressionSavedTokens') }}</span>
            <strong>
              {{
                compression.metrics_known_requests
                  ? formatIntegerString(compression.tokens_saved_sum)
                  : '—'
              }}
            </strong>
            <small>
              {{
                t('aiUsage.kpi.contextCompressionKnown', {
                  count: compression.metrics_known_requests,
                })
              }}
            </small>
          </KCard>

          <KCard class="ai-usage-kpi">
            <span>{{ t('aiUsage.fields.contextCompressionRatio') }}</span>
            <strong>{{ formatCoverage(compression.weighted_compression_ratio) }}</strong>
            <small>
              {{
                t('aiUsage.kpi.contextCompressionTokenFlow', {
                  before: formatIntegerString(compression.tokens_before_sum),
                  after: formatIntegerString(compression.tokens_after_sum),
                })
              }}
            </small>
          </KCard>

          <KCard class="ai-usage-kpi">
            <span>{{ t('aiUsage.fields.contextCompressionBypassRatio') }}</span>
            <strong>{{ formatCoverage(compressionBypassRatio) }}</strong>
            <small>
              {{
                t('aiUsage.kpi.contextCompressionBypassed', {
                  bypassed: compression.bypassed_requests,
                  total: compressionEvaluatedRequests,
                })
              }}
            </small>
          </KCard>
        </template>
      </div>

      <KCard class="ai-usage-panel">
        <div class="ai-usage-section-header ai-usage-trend-controls">
          <div>
            <h2>{{ t('aiUsage.trend.metric') }}</h2>
            <p>{{ t('aiUsage.trend.metricHint') }}</p>
          </div>
          <div class="ai-usage-toggle-group">
            <button
              :class="{ active: filters.metric === 'cost' }"
              type="button"
              @click="setMetric('cost')"
            >
              {{ t('aiUsage.metrics.cost') }}
            </button>
            <button
              :class="{ active: filters.metric === 'tokens' }"
              type="button"
              @click="setMetric('tokens')"
            >
              {{ t('aiUsage.metrics.tokens') }}
            </button>
            <select
              v-if="filters.metric === 'tokens'"
              :aria-label="t('aiUsage.trend.tokenMetric')"
              :value="filters.tokenMetric"
              @change="onTokenMetricChange"
            >
              <option value="prompt">
                {{ t('aiUsage.fields.promptTokens') }}
              </option>
              <option value="completion">
                {{ t('aiUsage.fields.completionTokens') }}
              </option>
              <option value="total">
                {{ t('aiUsage.fields.totalTokens') }}
              </option>
            </select>
          </div>
        </div>

        <UsageTrendChart
          :items="trend?.items ?? []"
          :metric="filters.metric"
          :token-metric="filters.tokenMetric"
        />
      </KCard>

      <div class="ai-usage-ranking-grid">
        <KCard class="ai-usage-panel">
          <UsageRankingTable
            :breakdown="modelRanking"
            :title="t('aiUsage.ranking.models')"
            @drill="drillDown"
          />
        </KCard>
        <KCard class="ai-usage-panel">
          <UsageRankingTable
            :breakdown="virtualKeyRanking"
            :title="t('aiUsage.ranking.virtualKeys')"
            @drill="drillDown"
          />
        </KCard>
      </div>

      <KCard class="ai-usage-panel">
        <div class="ai-usage-section-header">
          <div>
            <h2>{{ t('aiUsage.status.title') }}</h2>
            <p>{{ t('aiUsage.status.body') }}</p>
          </div>
        </div>
        <div class="ai-usage-status-grid">
          <div>
            <h3>{{ t('aiUsage.status.outcomes') }}</h3>
            <dl>
              <template
                v-for="(count, key) in totals.outcomes"
                :key="key"
              >
                <dt><UsageStatusBadge :value="String(key)" /></dt>
                <dd>{{ count }}</dd>
              </template>
            </dl>
          </div>
          <div>
            <h3>{{ t('aiUsage.status.pricing') }}</h3>
            <dl>
              <template
                v-for="(count, key) in totals.pricing_status"
                :key="key"
              >
                <dt><UsageStatusBadge :value="String(key)" /></dt>
                <dd>{{ count }}</dd>
              </template>
            </dl>
          </div>
          <div>
            <h3>{{ t('aiUsage.status.cost') }}</h3>
            <dl>
              <template
                v-for="(count, key) in totals.cost_status"
                :key="key"
              >
                <dt><UsageStatusBadge :value="String(key)" /></dt>
                <dd>{{ count }}</dd>
              </template>
            </dl>
          </div>
        </div>
      </KCard>
    </template>
  </section>
</template>

<script setup lang="ts">
import { computed } from 'vue'
import { useI18n } from '@/composables/useI18n'
import type {
  AiUsageMetric,
  AiUsageTokenMetric,
} from './aiUsageTypes'
import {
  formatCoverage,
  formatIntegerString,
  formatTimestamp,
  formatTokenAggregate,
  formatUsd,
} from './aiUsageFormatters'
import type { AiUsageController } from './useAiUsageController'
import UsageRankingTable from './components/UsageRankingTable.vue'
import UsageStatusBadge from './components/UsageStatusBadge.vue'
import UsageTrendChart from './components/UsageTrendChart.vue'

defineOptions({
  name: 'AiUsageOverview',
})

const props = defineProps<{
  controller: AiUsageController
}>()

const { t } = useI18n()
const {
  applyFilters,
  drillDown,
  errorMessage,
  filters,
  meta,
  modelRanking,
  retry,
  state,
  summary,
  trend,
  virtualKeyRanking,
} = props.controller

const totals = computed(() => summary.value?.totals ?? null)
const compression = computed(() => totals.value?.context_compression ?? null)
const compressionEvaluatedRequests = computed(() => {
  const value = compression.value
  if (!value) {
    return 0
  }

  return value.applied_requests
    + value.bypassed_requests
    + value.degraded_requests
    + value.rejected_requests
})
const compressionVisible = computed(() => {
  const value = compression.value

  return Boolean(value && (
    compressionEvaluatedRequests.value > 0
    || value.pending_requests > 0
    || value.metrics_known_requests > 0
  ))
})
const compressionBypassRatio = computed(() => {
  const value = compression.value
  const total = compressionEvaluatedRequests.value
  if (!value || total === 0) {
    return null
  }

  return (value.bypassed_requests / total).toFixed(6)
})

const tokenCards = computed(() => {
  if (!totals.value) {
    return []
  }

  return [
    {
      key: 'prompt',
      label: t('aiUsage.fields.promptTokens'),
      aggregate: totals.value.prompt_tokens,
    },
    {
      key: 'completion',
      label: t('aiUsage.fields.completionTokens'),
      aggregate: totals.value.completion_tokens,
    },
    {
      key: 'total',
      label: t('aiUsage.fields.totalTokens'),
      aggregate: totals.value.total_tokens,
    },
  ]
})

const setMetric = (metric: AiUsageMetric) => {
  void applyFilters({
    ...filters.value,
    metric,
  })
}

const setTokenMetric = (value: string) => {
  if (!['prompt', 'completion', 'total'].includes(value)) {
    return
  }

  void applyFilters({
    ...filters.value,
    tokenMetric: value as AiUsageTokenMetric,
  })
}

const onTokenMetricChange = (event: Event) => {
  setTokenMetric((event.target as HTMLSelectElement).value)
}
</script>
