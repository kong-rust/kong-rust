<template>
  <section
    class="ai-usage-page"
    @keydown.esc="selectedLog = null"
  >
    <KAlert
      v-if="meta.ephemeral"
      appearance="warning"
      class="ai-gateway-alert"
    >
      {{
        t('aiUsage.mode.dblessLogs', {
          node: meta.node_id || t('aiUsage.labels.unknown'),
          capacity: meta.capacity ?? t('aiUsage.labels.unknown'),
        })
      }}
    </KAlert>

    <div
      v-if="state === 'loading' || logsLoading"
      class="ai-usage-state"
      role="status"
    >
      <span class="ai-usage-spinner" />
      <h2>{{ t('aiUsage.logs.loading') }}</h2>
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
      v-else-if="!logs.length"
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

    <template v-else>
      <KCard class="ai-usage-panel ai-usage-logs-card">
        <div class="ai-usage-section-header">
          <div>
            <h2>{{ t('aiUsage.logs.title') }}</h2>
            <p>{{ t('aiUsage.logs.body') }}</p>
          </div>
          <span class="ai-usage-snapshot">
            {{ t('aiUsage.logs.snapshot') }}:
            <code>{{ snapshot.slice(0, 12) }}…</code>
          </span>
        </div>

        <div class="ai-usage-table-scroll">
          <table class="ai-usage-logs-table">
            <thead>
              <tr>
                <th scope="col">
                  {{ t('aiUsage.fields.time') }}
                </th>
                <th scope="col">
                  {{ t('aiUsage.fields.request') }}
                </th>
                <th scope="col">
                  {{ t('aiUsage.fields.result') }}
                </th>
                <th scope="col">
                  {{ t('aiUsage.fields.gateway') }}
                </th>
                <th scope="col">
                  {{ t('aiUsage.fields.providerModel') }}
                </th>
                <th scope="col">
                  {{ t('aiUsage.fields.identity') }}
                </th>
                <th scope="col">
                  {{ t('aiUsage.fields.tokens') }}
                </th>
                <th scope="col">
                  {{ t('aiUsage.fields.pricingCost') }}
                </th>
                <th scope="col">
                  {{ t('aiUsage.fields.latency') }}
                </th>
                <th scope="col">
                  <span class="sr-only">{{ t('aiUsage.actions.details') }}</span>
                </th>
              </tr>
            </thead>
            <tbody>
              <tr
                v-for="log in logs"
                :key="log.id"
              >
                <td>
                  <time :datetime="log.started_at">
                    {{ formatTimestamp(log.started_at, filters.timezone) }}
                  </time>
                </td>
                <td>
                  <code>{{ log.request_id }}</code>
                </td>
                <td>
                  <UsageStatusBadge :value="log.result.outcome" />
                  <small>{{ log.result.status_code ?? '—' }}</small>
                </td>
                <td>
                  <strong>{{ snapshotLabel(log.gateway.route) }}</strong>
                  <small>{{ snapshotLabel(log.gateway.service) }}</small>
                </td>
                <td>
                  <strong>{{ snapshotLabel(log.ai.provider) }}</strong>
                  <small>
                    {{ log.ai.model?.actual || '—' }}
                    · {{ log.ai.model?.group || '—' }}
                    · {{ log.ai.model?.requested || '—' }}
                  </small>
                </td>
                <td>
                  <strong>{{ snapshotLabel(log.identity.virtual_key) }}</strong>
                  <small>{{ log.identity.virtual_key?.prefix || '—' }}</small>
                </td>
                <td>
                  <span>
                    P {{ formatIntegerString(log.usage.prompt_tokens) }}
                    / C {{ formatIntegerString(log.usage.completion_tokens) }}
                    / T {{ formatIntegerString(log.usage.total_tokens) }}
                  </span>
                  <small>
                    {{ localizedValue(log.usage.prompt_source) }}
                    / {{ localizedValue(log.usage.completion_source) }}
                    / {{ localizedValue(log.usage.total_source) }}
                  </small>
                  <div
                    v-if="log.context_compression"
                    class="ai-usage-inline-badges"
                  >
                    <UsageStatusBadge :value="log.context_compression.status" />
                  </div>
                  <small
                    v-if="log.context_compression?.tokens_saved !== null
                      && log.context_compression?.tokens_saved !== undefined"
                  >
                    {{
                      t('aiUsage.logs.contextCompressionSaved', {
                        tokens: formatIntegerString(log.context_compression.tokens_saved),
                      })
                    }}
                  </small>
                </td>
                <td>
                  <strong>{{ formatUsd(log.cost.usd) }}</strong>
                  <small>
                    Input {{ priceSummary(log.pricing.input) }}
                  </small>
                  <small>
                    Output {{ priceSummary(log.pricing.output) }}
                  </small>
                  <div class="ai-usage-inline-badges">
                    <UsageStatusBadge :value="log.pricing.status" />
                    <UsageStatusBadge :value="log.cost.status" />
                  </div>
                  <small>
                    {{
                      compactReasons([
                        ...log.pricing.unsupported_reasons,
                        ...log.cost.unavailable_reasons,
                      ])
                    }}
                  </small>
                </td>
                <td>
                  <span>E2E {{ formatLatency(log.result.e2e_ms) }}</span>
                  <small>TTFT {{ formatLatency(log.result.ttft_ms) }}</small>
                  <small>
                    {{ streamCacheSummary(log.result.stream, log.result.cache_status) }}
                  </small>
                </td>
                <td>
                  <KButton
                    appearance="tertiary"
                    size="small"
                    @click="selectedLog = log"
                  >
                    {{ t('aiUsage.actions.details') }}
                  </KButton>
                </td>
              </tr>
            </tbody>
          </table>
        </div>

        <div class="ai-usage-pagination">
          <KButton
            appearance="secondary"
            :disabled="!canPreviousLogs || logsLoading"
            @click="previousLogs"
          >
            {{ t('aiUsage.actions.previous') }}
          </KButton>
          <span>
            {{ t('aiUsage.logs.page', { page: pageNumber }) }}
          </span>
          <KButton
            appearance="secondary"
            :disabled="!canNextLogs || logsLoading"
            @click="nextLogs"
          >
            {{ t('aiUsage.actions.next') }}
          </KButton>
        </div>
      </KCard>

      <aside
        v-if="selectedLog"
        aria-labelledby="ai-usage-detail-title"
        aria-modal="true"
        class="ai-usage-detail"
        ref="detailPanel"
        role="dialog"
        tabindex="-1"
      >
        <header>
          <div>
            <span>{{ t('aiUsage.logs.detailEyebrow') }}</span>
            <h2 id="ai-usage-detail-title">
              {{ selectedLog.request_id }}
            </h2>
          </div>
          <KButton
            appearance="tertiary"
            @click="selectedLog = null"
          >
            {{ t('aiUsage.actions.close') }}
          </KButton>
        </header>

        <div class="ai-usage-detail-body">
          <DetailGroup :title="t('aiUsage.detail.request')">
            <DetailRow
              :label="t('aiUsage.fields.factId')"
              :value="selectedLog.id"
            />
            <DetailRow
              :label="t('aiUsage.filters.requestId')"
              :value="selectedLog.request_id"
            />
            <DetailRow
              :label="t('aiUsage.fields.startedAt')"
              :value="formatTimestamp(selectedLog.started_at, filters.timezone)"
            />
            <DetailRow
              :label="t('aiUsage.fields.finishedAt')"
              :value="formatTimestamp(selectedLog.finished_at, filters.timezone)"
            />
          </DetailGroup>

          <DetailGroup :title="t('aiUsage.detail.gateway')">
            <DetailRow
              :label="t('aiUsage.fields.route')"
              :value="entityDetail(selectedLog.gateway.route)"
            />
            <DetailRow
              :label="t('aiUsage.fields.service')"
              :value="entityDetail(selectedLog.gateway.service)"
            />
          </DetailGroup>

          <DetailGroup :title="t('aiUsage.detail.ai')">
            <DetailRow
              :label="t('aiUsage.fields.provider')"
              :value="providerDetail(selectedLog.ai.provider)"
            />
            <DetailRow
              :label="t('aiUsage.fields.modelId')"
              :value="selectedLog.ai.model?.id || '—'"
            />
            <DetailRow
              :label="t('aiUsage.fields.requestedModel')"
              :value="selectedLog.ai.model?.requested || '—'"
            />
            <DetailRow
              :label="t('aiUsage.fields.modelGroup')"
              :value="selectedLog.ai.model?.group || '—'"
            />
            <DetailRow
              :label="t('aiUsage.fields.actualModel')"
              :value="selectedLog.ai.model?.actual || '—'"
            />
            <DetailRow
              :label="t('aiUsage.fields.attemptCount')"
              :value="String(selectedLog.ai.attempt_count)"
            />
          </DetailGroup>

          <DetailGroup :title="t('aiUsage.detail.identity')">
            <DetailRow
              :label="t('aiUsage.fields.virtualKey')"
              :value="virtualKeyDetail(selectedLog.identity.virtual_key)"
            />
            <DetailRow
              :label="t('aiUsage.fields.consumerId')"
              :value="selectedLog.identity.consumer_id || '—'"
            />
          </DetailGroup>

          <DetailGroup :title="t('aiUsage.detail.usage')">
            <DetailRow
              :label="t('aiUsage.fields.promptTokens')"
              :value="tokenDetail(selectedLog.usage.prompt_tokens, selectedLog.usage.prompt_source)"
            />
            <DetailRow
              :label="t('aiUsage.fields.completionTokens')"
              :value="tokenDetail(selectedLog.usage.completion_tokens, selectedLog.usage.completion_source)"
            />
            <DetailRow
              :label="t('aiUsage.fields.totalTokens')"
              :value="tokenDetail(selectedLog.usage.total_tokens, selectedLog.usage.total_source)"
            />
            <DetailRow
              :label="t('aiUsage.fields.reasoningTokens')"
              :value="formatIntegerString(selectedLog.usage.reasoning_tokens)"
            />
            <DetailRow
              :label="t('aiUsage.fields.cacheReadTokens')"
              :value="formatIntegerString(selectedLog.usage.cache_read_input_tokens)"
            />
            <DetailRow
              :label="t('aiUsage.fields.cacheWriteTokens')"
              :value="formatIntegerString(selectedLog.usage.cache_write_input_tokens)"
            />
            <DetailRow
              :label="t('aiUsage.fields.usageSource')"
              :value="valueWithCode(selectedLog.usage.source)"
            />
            <DetailRow
              :label="t('aiUsage.fields.reasons')"
              :value="compactReasons(selectedLog.usage.unavailable_reasons)"
            />
          </DetailGroup>

          <DetailGroup
            v-if="selectedLog.context_compression"
            :title="t('aiUsage.detail.contextCompression')"
          >
            <DetailRow
              :label="t('aiUsage.fields.contextCompressionStatus')"
              :value="valueWithCode(selectedLog.context_compression.status)"
            />
            <DetailRow
              :label="t('aiUsage.fields.contextCompressionReason')"
              :value="valueWithCode(selectedLog.context_compression.reason)"
            />
            <DetailRow
              :label="t('aiUsage.fields.contextCompressionBackend')"
              :value="selectedLog.context_compression.backend || '—'"
            />
            <DetailRow
              :label="t('aiUsage.fields.contextCompressionCcr')"
              :value="valueWithCode(String(selectedLog.context_compression.ccr))"
            />
            <DetailRow
              :label="t('aiUsage.fields.contextCompressionTokensBefore')"
              :value="formatIntegerString(selectedLog.context_compression.tokens_before)"
            />
            <DetailRow
              :label="t('aiUsage.fields.contextCompressionTokensAfter')"
              :value="formatIntegerString(selectedLog.context_compression.tokens_after)"
            />
            <DetailRow
              :label="t('aiUsage.fields.contextCompressionSavedTokens')"
              :value="formatIntegerString(selectedLog.context_compression.tokens_saved)"
            />
            <DetailRow
              :label="t('aiUsage.fields.contextCompressionHopLatency')"
              :value="formatLatency(selectedLog.context_compression.hop_latency_ms)"
            />
          </DetailGroup>

          <DetailGroup :title="t('aiUsage.detail.pricing')">
            <DetailRow
              :label="t('aiUsage.fields.inputPrice')"
              :value="priceDetail(selectedLog.pricing.input)"
            />
            <DetailRow
              :label="t('aiUsage.fields.outputPrice')"
              :value="priceDetail(selectedLog.pricing.output)"
            />
            <DetailRow
              :label="t('aiUsage.fields.pricingStatus')"
              :value="valueWithCode(selectedLog.pricing.status)"
            />
            <DetailRow
              :label="t('aiUsage.fields.costStatus')"
              :value="valueWithCode(selectedLog.cost.status)"
            />
            <DetailRow
              :label="t('aiUsage.fields.calculableCost')"
              :value="formatUsd(selectedLog.cost.usd)"
            />
            <DetailRow
              :label="t('aiUsage.fields.reasons')"
              :value="compactReasons([
                ...selectedLog.pricing.unsupported_reasons,
                ...selectedLog.cost.unavailable_reasons,
              ])"
            />
          </DetailGroup>

          <DetailGroup :title="t('aiUsage.detail.result')">
            <DetailRow
              :label="t('aiUsage.fields.statusCode')"
              :value="String(selectedLog.result.status_code ?? '—')"
            />
            <DetailRow
              :label="t('aiUsage.fields.upstreamStatus')"
              :value="String(selectedLog.result.upstream_status_code ?? '—')"
            />
            <DetailRow
              :label="t('aiUsage.fields.outcome')"
              :value="valueWithCode(selectedLog.result.outcome)"
            />
            <DetailRow
              :label="t('aiUsage.fields.latency')"
              :value="`E2E ${formatLatency(selectedLog.result.e2e_ms)} · TTFT ${formatLatency(selectedLog.result.ttft_ms)}`"
            />
            <DetailRow
              :label="t('aiUsage.fields.streamCache')"
              :value="streamCacheSummary(selectedLog.result.stream, selectedLog.result.cache_status)"
            />
            <DetailRow
              :label="t('aiUsage.fields.upstreamAttempted')"
              :value="valueWithCode(String(selectedLog.result.upstream_attempted))"
            />
          </DetailGroup>
        </div>
      </aside>
    </template>
  </section>
</template>

<script setup lang="ts">
import {
  computed,
  nextTick,
  ref,
  watch,
} from 'vue'
import { useI18n } from '@/composables/useI18n'
import type {
  AiUsageEntitySnapshot,
  AiUsagePriceDirection,
  AiUsageProviderSnapshot,
  AiUsageSource,
  AiUsageVirtualKeySnapshot,
} from './aiUsageTypes'
import {
  compactReasons,
  formatIntegerString,
  formatLatency,
  formatTimestamp,
  formatUsd,
  snapshotLabel,
  statusLabel,
} from './aiUsageFormatters'
import type { AiUsageController } from './useAiUsageController'
import DetailGroup from './components/UsageDetailGroup.vue'
import DetailRow from './components/UsageDetailRow.vue'
import UsageStatusBadge from './components/UsageStatusBadge.vue'

defineOptions({
  name: 'AiUsageLogs',
})

const props = defineProps<{
  controller: AiUsageController
}>()

const { t } = useI18n()
const {
  canNextLogs,
  canPreviousLogs,
  errorMessage,
  filters,
  logsLoading,
  logsPage,
  logsPageNumber,
  meta,
  nextLogs,
  previousLogs,
  retry,
  selectedLog,
  snapshot,
  state,
} = props.controller

const logs = computed(() => logsPage.value?.data ?? [])
const pageNumber = logsPageNumber
const detailPanel = ref<HTMLElement | null>(null)

watch(selectedLog, async (selected) => {
  if (!selected) {
    return
  }

  await nextTick()
  detailPanel.value?.focus()
})

const entityDetail = (entity: AiUsageEntitySnapshot | null) => {
  if (!entity) {
    return '—'
  }

  return [entity.name, entity.id].filter(Boolean).join(' · ') || '—'
}

const providerDetail = (provider: AiUsageProviderSnapshot | null) => {
  if (!provider) {
    return '—'
  }

  return [provider.name, provider.type, provider.id].filter(Boolean).join(' · ') || '—'
}

const virtualKeyDetail = (virtualKey: AiUsageVirtualKeySnapshot | null) => {
  if (!virtualKey) {
    return '—'
  }

  return [virtualKey.name, virtualKey.prefix, virtualKey.id].filter(Boolean).join(' · ') || '—'
}

const localizedValue = (value: string | null | undefined) => {
  if (!value) {
    return '—'
  }

  const key = `aiUsage.values.${value}`
  const translated = t(key)

  return translated === key ? statusLabel(value) : translated
}

const valueWithCode = (value: string) => {
  const localized = localizedValue(value)

  return localized === value ? value : `${localized} · ${value}`
}

const tokenDetail = (value: number | null, source: AiUsageSource | null) => (
  `${formatIntegerString(value)} · ${localizedValue(source)}`
)

const priceSummary = (direction: AiUsagePriceDirection | null) => {
  if (!direction) {
    return t('aiUsage.labels.unpriced')
  }

  return `${formatUsd(direction.usd_per_million)} · ${localizedValue(direction.source)}`
}

const streamCacheSummary = (stream: boolean | null, cacheStatus: string) => (
  `${stream === null ? '—' : localizedValue(String(stream))} · ${localizedValue(cacheStatus)}`
)

const priceDetail = (direction: AiUsagePriceDirection | null) => {
  if (!direction) {
    return t('aiUsage.labels.unpriced')
  }

  const effective = direction.effective_to
    ? `${direction.effective_from} – ${direction.effective_to}`
    : `${direction.effective_from} – ∞`

  return [
    `${formatUsd(direction.usd_per_million)} / 1M`,
    valueWithCode(direction.source),
    direction.version,
    direction.snapshot_date,
    effective,
  ].join(' · ')
}
</script>
