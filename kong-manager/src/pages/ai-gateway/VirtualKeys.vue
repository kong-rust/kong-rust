<template>
  <PageHeader :title="t('AI Virtual Keys')">
    <KButton
      data-tour="create-virtual-key"
      :disabled="mutationPending || !!latestKey"
      @click="startCreate"
    >
      {{ t('Create Virtual Key') }}
    </KButton>
  </PageHeader>
  <AiGatewayNav />

  <KAlert
    appearance="info"
    class="ai-gateway-alert"
  >
    <div class="ai-virtual-key-alert-copy">
      <span>
        {{ t('Virtual keys authenticate AI traffic and enforce allowed models. Quota and lifecycle budget status below reflects each key’s effective policy and deployment capability.') }}
      </span>
      <span>
        {{ t('Attach ai-rate-limit with virtual_key mode to protected endpoints. First activation on other nodes may take the authentication cache TTL plus replica lag.') }}
        <RouterLink :to="{ name: 'plugin-list' }">
          {{ t('Manage Plugins') }}
        </RouterLink>
      </span>
    </div>
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
      {{ t('This secret is shown once. Copy it, then dismiss it before creating or rotating another key.') }}
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
        {{ t('Copy Key') }}
      </KButton>
      <KButton
        appearance="tertiary"
        type="button"
        @click="clearLatestKey"
      >
        {{ t('Dismiss') }}
      </KButton>
    </div>
  </section>

  <KCard
    v-if="formVisible"
    class="ai-gateway-form-card"
    :title="editingId ? t('Edit Virtual Key') : t('Create Virtual Key')"
  >
    <form
      class="ai-gateway-form"
      @submit.prevent="submitVirtualKey"
    >
      <div class="ai-gateway-form-grid">
        <div class="ai-gateway-form-field">
          <label for="ai-key-name">{{ t('Name') }}</label>
          <input
            id="ai-key-name"
            v-model.trim="form.name"
            required
          >
        </div>

        <div class="ai-gateway-form-field">
          <label for="ai-key-consumer">{{ t('Consumer ID') }}</label>
          <input
            id="ai-key-consumer"
            v-model.trim="form.consumerId"
          >
        </div>

        <div class="ai-gateway-form-field">
          <label for="ai-key-models">{{ t('Allowed Models') }}</label>
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
          {{ t('Enabled') }}
        </label>
      </div>

      <div class="ai-gateway-form-grid">
        <div class="ai-gateway-form-field">
          <label for="ai-key-tpm">{{ t('TPM Limit') }}</label>
          <input
            id="ai-key-tpm"
            v-model="form.tpmLimit"
            max="2147483647"
            min="1"
            step="1"
            type="number"
          >
        </div>

        <div class="ai-gateway-form-field">
          <label for="ai-key-rpm">{{ t('RPM Limit') }}</label>
          <input
            id="ai-key-rpm"
            v-model="form.rpmLimit"
            max="2147483647"
            min="1"
            step="1"
            type="number"
          >
        </div>

        <div class="ai-gateway-form-field">
          <label for="ai-key-budget">{{ t('Budget Limit (USD / Lifetime cumulative)') }}</label>
          <input
            id="ai-key-budget"
            v-model="form.budgetLimit"
            autocomplete="off"
            inputmode="decimal"
            type="text"
          >
          <p class="ai-endpoint-hint">
            {{ t('Exact decimal with up to 16 integer digits and 12 decimal places. Clear the field to remove the limit; historical usage is retained.') }}
          </p>
        </div>

        <div class="ai-gateway-form-field">
          <label for="ai-key-expires">{{ t('Expires At') }}</label>
          <input
            id="ai-key-expires"
            v-model="form.expiresAt"
            type="datetime-local"
          >
        </div>
      </div>

      <div class="ai-gateway-form-field">
        <label for="ai-key-tags">{{ t('Tags') }}</label>
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
          {{ mutationPending ? t('Saving...') : t('Save Virtual Key') }}
        </KButton>
        <KButton
          appearance="secondary"
          type="button"
          @click="cancelForm"
        >
          {{ t('Cancel') }}
        </KButton>
      </div>
    </form>
  </KCard>

  <KCard
    v-if="ledgerKey"
    class="ai-gateway-form-card ai-budget-ledger"
    :title="l(`Budget ledger · ${ledgerKey.name}`, `预算账本 · ${ledgerKey.name}`)"
  >
    <div class="ai-gateway-form-actions">
      <KButton
        appearance="secondary"
        size="small"
        :disabled="ledgerPending"
        @click="loadBudgetLedger(ledgerKey)"
      >
        {{ l('Refresh ledger', '刷新账本') }}
      </KButton>
      <KButton
        appearance="secondary"
        size="small"
        :disabled="ledgerPending || !reconciliationReason.trim()"
        @click="verifyBudgetLedger(true)"
      >
        {{ l('Verify aggregate', '校验汇总') }}
      </KButton>
      <KButton
        appearance="danger"
        size="small"
        :disabled="ledgerPending || !reconciliationReason.trim()"
        @click="verifyBudgetLedger(false)"
      >
        {{ l('Rebuild aggregate', '重建汇总') }}
      </KButton>
      <KButton
        appearance="tertiary"
        size="small"
        @click="closeBudgetLedger"
      >
        {{ t('Close') }}
      </KButton>
    </div>

    <div class="ai-gateway-form-field">
      <label for="ai-budget-reason">{{ l('Audit reason (required)', '审计原因（必填）') }}</label>
      <input
        id="ai-budget-reason"
        v-model.trim="reconciliationReason"
        :placeholder="l('Ticket or incident reason', '工单号或事件原因')"
      >
    </div>

    <KAlert
      v-if="ledgerError"
      appearance="danger"
      class="ai-gateway-alert"
    >
      {{ ledgerError }}
    </KAlert>

    <p v-if="ledgerAccount">
      {{ l(
        `Used ${ledgerAccount.budget_used_decimal} USD · ${ledgerAccount.pending_intent_count} pending · ${ledgerAccount.unresolved_intent_count} unresolved`,
        `已用 ${ledgerAccount.budget_used_decimal} USD · ${ledgerAccount.pending_intent_count} 个待结算 · ${ledgerAccount.unresolved_intent_count} 个未决`,
      ) }}
    </p>

    <div
      v-if="ledgerEntries.length"
      class="ai-budget-ledger-list"
    >
      <article
        v-for="entry in ledgerEntries"
        :key="entry.id"
        class="ai-budget-ledger-entry"
      >
        <div>
          <strong>{{ entry.status }}</strong>
          <span class="ai-gateway-mono">{{ entry.request_id || entry.id }}</span>
          <small>{{ entry.created_at }}</small>
          <small v-if="entry.cost_reasons?.length">{{ entry.cost_reasons.join(', ') }}</small>
        </div>
        <KButton
          v-if="entry.status === 'pending' || entry.status === 'unresolved'"
          appearance="secondary"
          size="small"
          :disabled="ledgerPending"
          @click="selectReconciliation(entry)"
        >
          {{ l('Reconcile', '处理未决') }}
        </KButton>
      </article>
    </div>
    <p v-else-if="!ledgerPending">
      {{ l('No pending or unresolved budget intents.', '没有待结算或未决的预算请求。') }}
    </p>

    <form
      v-if="reconciliationEntry"
      class="ai-gateway-form ai-budget-reconciliation"
      @submit.prevent="submitReconciliation"
    >
      <strong>
        {{ l(`Reconcile intent ${reconciliationEntry.id}`, `处理 intent ${reconciliationEntry.id}`) }}
      </strong>
      <div class="ai-gateway-form-grid">
        <div class="ai-gateway-form-field">
          <label for="ai-budget-action">{{ l('Resolution', '处理方式') }}</label>
          <select
            id="ai-budget-action"
            v-model="reconciliationAction"
          >
            <option value="settle">
              {{ l('Settle exact cost', '按精确成本结算') }}
            </option>
            <option value="waive">
              {{ l('Waive as zero', '豁免为零') }}
            </option>
          </select>
        </div>
        <div
          v-if="reconciliationAction === 'settle'"
          class="ai-gateway-form-field"
        >
          <label for="ai-budget-cost">{{ l('Cost (USD)', '成本（USD）') }}</label>
          <input
            id="ai-budget-cost"
            v-model.trim="reconciliationCost"
            inputmode="decimal"
            required
          >
        </div>
      </div>
      <div class="ai-gateway-form-actions">
        <KButton
          type="submit"
          :disabled="ledgerPending || !reconciliationReason.trim()"
        >
          {{ ledgerPending ? t('Saving...') : l('Apply reconciliation', '提交对账') }}
        </KButton>
        <KButton
          appearance="secondary"
          type="button"
          @click="reconciliationEntry = null"
        >
          {{ t('Cancel') }}
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
      :empty-state-title="t('No AI virtual keys')"
      :empty-state-message="t('Create a virtual key, then attach its policy to an AI endpoint.')"
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
        <div class="ai-virtual-key-policy">
          <strong>{{ quotaLimitsLabel(row) }}</strong>
          <div class="ai-virtual-key-policy-badges">
            <KBadge :appearance="quotaStatusAppearance(row.quota_enforcement)">
              {{ quotaStatusLabel(row.quota_enforcement) }}
            </KBadge>
          </div>
          <small>{{ quotaCapabilityLabel(row) }}</small>
          <small>{{ quotaStatusDescription(row) }}</small>
          <small
            v-if="quotaCoverageDescription(row)"
            class="ai-virtual-key-policy-attention"
          >
            {{ quotaCoverageDescription(row) }}
          </small>
        </div>
      </template>

      <template #budget="{ row }">
        <div class="ai-virtual-key-policy">
          <strong>{{ budgetAmountLabel(row) }}</strong>
          <div class="ai-virtual-key-policy-badges">
            <KBadge :appearance="budgetStatusAppearance(row.budget_status)">
              {{ budgetStatusLabel(row.budget_status) }}
            </KBadge>
          </div>
          <div
            v-if="budgetProgressWidth(row)"
            class="ai-virtual-key-budget-progress"
            role="progressbar"
            :aria-label="l('Lifecycle budget usage', '生命周期预算使用比例')"
            aria-valuemax="100"
            aria-valuemin="0"
            :aria-valuenow="budgetProgressValue(row)"
            :aria-valuetext="budgetPercentageLabel(row)"
          >
            <span :style="{ width: budgetProgressWidth(row) }" />
          </div>
          <small v-if="budgetPercentageLabel(row)">
            {{ budgetPercentageLabel(row) }}
          </small>
          <small>{{ budgetCapabilityLabel(row) }}</small>
          <small>{{ budgetStatusDescription(row) }}</small>
          <small
            v-if="budgetAccountingDescription(row)"
            class="ai-virtual-key-policy-attention"
          >
            {{ budgetAccountingDescription(row) }}
          </small>
        </div>
      </template>

      <template #expires_at="{ rowValue }">
        <span>{{ formatOptionalDate(rowValue) }}</span>
      </template>

      <template #enabled="{ rowValue }">
        <KBadge :appearance="rowValue ? 'success' : 'neutral'">
          {{ rowValue ? t('Enabled') : t('Disabled') }}
        </KBadge>
      </template>

      <template #actions="{ row }">
        <div class="ai-gateway-row-actions">
          <KButton
            appearance="secondary"
            size="small"
            @click="viewUsage(row)"
          >
            {{ t('View Usage') }}
          </KButton>
          <KButton
            appearance="secondary"
            size="small"
            :disabled="ledgerPending"
            @click="loadBudgetLedger(row)"
          >
            {{ l('Budget Ledger', '预算账本') }}
          </KButton>
          <KButton
            appearance="secondary"
            size="small"
            :disabled="mutationPending"
            @click="startEdit(row)"
          >
            {{ t('Edit') }}
          </KButton>
          <KButton
            appearance="secondary"
            size="small"
            :disabled="mutationPending || !!latestKey"
            @click="rotateVirtualKey(row)"
          >
            {{ t('Rotate') }}
          </KButton>
          <KButton
            appearance="danger"
            size="small"
            :disabled="mutationPending"
            @click="deleteVirtualKey(row)"
          >
            {{ t('Delete') }}
          </KButton>
        </div>
      </template>
    </KTable>
  </KCard>
</template>

<script setup lang="ts">
import type { TableDataFetcherParams } from '@kong/kongponents'
import { computed, reactive, ref } from 'vue'
import { useRouter } from 'vue-router'
import AiGatewayNav from './AiGatewayNav.vue'
import { apiService } from '@/services/apiService'
import { useToaster } from '@/composables/useToaster'
import type {
  AiBudgetLedgerEntry,
  AiBudgetLedgerResponse,
  AiBudgetStatus,
  AiQuotaEnforcement,
  AiVirtualKey,
  KongPageResponse,
} from './types'
import { useAiGatewayI18n } from './useAiGatewayI18n'
import {
  formatOptionalDate,
  formatTags,
  fromLocalDateTimeInput,
  getErrorMessage,
  omitUndefined,
  parseOptionalDecimal,
  parseTags,
  toLocalDateTimeInput,
} from './utils'

interface VirtualKeyFormState {
  name: string
  consumerId: string
  allowedModels: string
  tpmLimit: string | number
  rpmLimit: string | number
  budgetLimit: string
  expiresAt: string
  enabled: boolean
  tags: string
}

defineOptions({
  name: 'AiGatewayVirtualKeys',
})

const toaster = useToaster()
const router = useRouter()
const { l, locale, t } = useAiGatewayI18n()
const tableKey = ref(0)
const formVisible = ref(false)
const mutationPending = ref(false)
const editingId = ref('')
const errorMessage = ref('')
const tableErrorMessage = ref('')
const latestKey = ref('')
const latestKeyTitle = ref('')
const ledgerKey = ref<AiVirtualKey | null>(null)
const ledgerEntries = ref<AiBudgetLedgerEntry[]>([])
const ledgerAccount = ref<AiBudgetLedgerResponse['account']>(null)
const ledgerPending = ref(false)
const ledgerError = ref('')
const reconciliationEntry = ref<AiBudgetLedgerEntry | null>(null)
const reconciliationAction = ref<'settle' | 'waive'>('settle')
const reconciliationCost = ref('')
const reconciliationReason = ref('')
const reconciliationOperationId = ref('')
const rebuildOperationId = ref('')
const maxQuotaLimit = 2_147_483_647

const compactDecimal = (value: string) => {
  const match = value.trim().match(/^(\d+)(?:\.(\d+))?$/)
  if (!match) {
    return value
  }

  const integer = (match[1] ?? '0').replace(/^0+(?=\d)/, '') || '0'
  const fraction = (match[2] ?? '').replace(/0+$/, '')

  return fraction ? `${integer}.${fraction}` : integer
}

const parseQuotaLimit = (value: string | number, dimension: 'TPM' | 'RPM') => {
  const normalized = String(value).trim()
  if (!normalized) {
    return undefined
  }
  if (!/^\d+$/.test(normalized)) {
    throw new Error(l(
      `${dimension} limit must be a positive integer`,
      `${dimension} 上限必须是正整数`,
    ))
  }

  const parsed = Number(normalized)
  if (parsed < 1 || parsed > maxQuotaLimit) {
    throw new Error(l(
      `${dimension} limit must be between 1 and ${maxQuotaLimit}`,
      `${dimension} 上限必须在 1 到 ${maxQuotaLimit} 之间`,
    ))
  }

  return parsed
}

const parseBudgetLimit = (value: string) => {
  try {
    return parseOptionalDecimal(value, 'Budget limit')
  } catch (err) {
    if (locale.value !== 'zh-CN') {
      throw err
    }

    const message = err instanceof Error ? err.message : ''
    if (message.includes('non-negative decimal')) {
      throw new Error('预算上限必须是非负十进制数，且不能使用指数格式')
    }

    throw new Error('预算上限最多支持 16 位整数和 12 位小数')
  }
}

const hasQuotaLimit = (virtualKey: AiVirtualKey) => {
  return (virtualKey.rpm_limit !== null && virtualKey.rpm_limit !== undefined)
    || (virtualKey.tpm_limit !== null && virtualKey.tpm_limit !== undefined)
}

const quotaLimitsLabel = (virtualKey: AiVirtualKey) => {
  if (!hasQuotaLimit(virtualKey)) {
    return t('Not configured')
  }

  return `${virtualKey.rpm_limit ?? '—'} RPM / ${virtualKey.tpm_limit ?? '—'} TPM`
}

const quotaStatusLabel = (status?: AiQuotaEnforcement) => {
  switch (status) {
    case 'configured_local':
      return l('Enforced locally', '本地已执行')
    case 'configured_local_partial':
      return l('Partially enforced', '部分执行')
    case 'awaiting_plugin':
      return l('Awaiting plugin', '等待挂载插件')
    case 'unconfigured':
      return t('Not configured')
    case 'unsupported':
      return t('Unsupported')
    default:
      return t('Status unavailable')
  }
}

const quotaStatusAppearance = (status?: AiQuotaEnforcement) => {
  if (status === 'configured_local') {
    return 'success' as const
  }
  if (status === 'configured_local_partial' || status === 'awaiting_plugin') {
    return 'warning' as const
  }

  return 'neutral' as const
}

const quotaCapabilityLabel = (virtualKey: AiVirtualKey) => {
  switch (virtualKey.capability?.quota) {
    case 'local_memory':
      return l('Capability: local memory · node scoped', '能力：本地内存 · 节点范围')
    case 'local_memory_ephemeral':
      return l('Capability: ephemeral local memory · node scoped', '能力：临时本地内存 · 节点范围')
    case 'unsupported':
      return l('Capability: unsupported in this deployment mode', '能力：当前部署模式不支持')
    default:
      return virtualKey.quota_backend
        ? l(`Backend: ${virtualKey.quota_backend}`, `后端：${virtualKey.quota_backend}`)
        : t('Capability unavailable')
  }
}

const quotaStatusDescription = (virtualKey: AiVirtualKey) => {
  const windowSeconds = virtualKey.quota_window_seconds ?? 60
  switch (virtualKey.quota_enforcement) {
    case 'configured_local':
      return l(
        `This node uses a ${windowSeconds}-second window starting on first hit.`,
        `本节点按首次命中起算的 ${windowSeconds} 秒窗口执行。`,
      )
    case 'configured_local_partial':
      return l(
        'Only part of the authenticated endpoint coverage enforces this quota.',
        '只有部分已认证接口覆盖执行此配额。',
      )
    case 'awaiting_plugin':
      return l(
        'Limits are configured, but no effective virtual-key rate-limit policy is mounted yet.',
        '上限已配置，但尚未挂载生效的虚拟密钥限流策略。',
      )
    case 'unconfigured':
      return l('No RPM or TPM limit is configured.', '尚未配置 RPM 或 TPM 上限。')
    case 'unsupported':
      return l(
        'Real-time quota enforcement is unavailable in this deployment mode.',
        '当前部署模式不支持实时配额执行。',
      )
    default:
      return l(
        'The server did not return quota enforcement metadata.',
        '服务端未返回配额执行元数据。',
      )
  }
}

const quotaCoverageDescription = (virtualKey: AiVirtualKey) => {
  if (virtualKey.coverage_available === false && hasQuotaLimit(virtualKey)) {
    return l(
      'Policy coverage is pending; mount ai-rate-limit on the protected endpoint.',
      '策略覆盖待挂载；请在受保护接口上挂载 ai-rate-limit。',
    )
  }

  const details: string[] = []
  if (virtualKey.auth_endpoint_count !== null
    && virtualKey.auth_endpoint_count !== undefined
    && virtualKey.enforced_endpoint_count !== null
    && virtualKey.enforced_endpoint_count !== undefined) {
    details.push(l(
      `${virtualKey.enforced_endpoint_count} of ${virtualKey.auth_endpoint_count} authenticated endpoints enforced`,
      `${virtualKey.auth_endpoint_count} 个认证接口中 ${virtualKey.enforced_endpoint_count} 个已执行`,
    ))
  }
  if (virtualKey.policy_error_count) {
    details.push(l(
      `${virtualKey.policy_error_count} invalid policy chains`,
      `${virtualKey.policy_error_count} 条无效策略链`,
    ))
  }

  return details.join(' · ')
}

const budgetAmountLabel = (virtualKey: AiVirtualKey) => {
  const used = virtualKey.budget_used_decimal
  if (!used) {
    return l('Exact amount unavailable', '精确金额不可用')
  }
  if (virtualKey.budget_limit_decimal === null) {
    return `${compactDecimal(used)} USD`
  }
  if (!virtualKey.budget_limit_decimal) {
    return l('Exact limit unavailable', '精确上限不可用')
  }

  return `${compactDecimal(used)} / ${compactDecimal(virtualKey.budget_limit_decimal)} USD`
}

const budgetStatusLabel = (status?: AiBudgetStatus) => {
  switch (status) {
    case 'active':
      return t('Active')
    case 'warning':
      return t('Warning')
    case 'exhausted':
      return t('Exhausted')
    case 'unresolved':
      return t('Reconciliation required')
    case 'paused':
      return t('Paused')
    case 'awaiting_plugin':
      return l('Awaiting plugin', '等待挂载插件')
    case 'unconfigured':
      return t('Not configured')
    case 'unsupported':
      return t('Unsupported')
    case 'unavailable':
      return l('Accounting unavailable', '账务不可用')
    default:
      return t('Status unavailable')
  }
}

const budgetStatusAppearance = (status?: AiBudgetStatus) => {
  if (status === 'active') {
    return 'success' as const
  }
  if (['warning', 'exhausted', 'unresolved', 'paused', 'awaiting_plugin', 'unavailable'].includes(status ?? '')) {
    return 'warning' as const
  }

  return 'neutral' as const
}

const budgetCapabilityLabel = (virtualKey: AiVirtualKey) => {
  switch (virtualKey.capability?.budget) {
    case 'postgres_authoritative':
      return l('Capability: PostgreSQL authoritative ledger', '能力：PostgreSQL 权威账本')
    case 'accounting_unavailable':
      return l('Capability: PostgreSQL accounting unavailable', '能力：PostgreSQL 账务当前不可用')
    case 'unsupported':
      return l('Capability: unsupported in this deployment mode', '能力：当前部署模式不支持')
    default:
      return virtualKey.budget_backend
        ? l(`Backend: ${virtualKey.budget_backend}`, `后端：${virtualKey.budget_backend}`)
        : t('Capability unavailable')
  }
}

const budgetStatusDescription = (virtualKey: AiVirtualKey) => {
  switch (virtualKey.budget_status) {
    case 'active':
      return l('Lifecycle budget enforcement is active.', '生命周期预算执行中。')
    case 'warning':
      return l('At least 80% of the lifecycle budget has been used.', '生命周期预算已使用至少 80%。')
    case 'exhausted':
      return l('The lifecycle budget is exhausted.', '生命周期预算已耗尽。')
    case 'unresolved':
      return l('Accounting must be reconciled before new budgeted requests.', '账务必须完成对账后才能继续预算请求。')
    case 'paused':
      return l('The limit is cleared; historical usage remains visible.', '上限已清除，历史用量仍保留显示。')
    case 'awaiting_plugin':
      return l(
        'The budget is configured, but effective endpoint policy coverage is pending.',
        '预算已配置，但有效接口策略覆盖仍待挂载。',
      )
    case 'unconfigured':
      return l('No lifecycle budget is configured.', '尚未配置生命周期预算。')
    case 'unsupported':
      return l(
        'Lifecycle budget enforcement is unavailable in this deployment mode.',
        '当前部署模式不支持生命周期预算执行。',
      )
    case 'unavailable':
      return l(
        'The authoritative budget accounting runtime is temporarily unavailable.',
        '权威预算账务运行时暂时不可用。',
      )
    default:
      return l(
        'The server did not return budget status metadata.',
        '服务端未返回预算状态元数据。',
      )
  }
}

const budgetAccountingDescription = (virtualKey: AiVirtualKey) => {
  const details: string[] = []
  if (virtualKey.pending_intent_count) {
    details.push(l(
      `${virtualKey.pending_intent_count} pending requests`,
      `${virtualKey.pending_intent_count} 个待结算请求`,
    ))
  }
  if (virtualKey.unresolved_intent_count) {
    details.push(l(
      `${virtualKey.unresolved_intent_count} unresolved requests`,
      `${virtualKey.unresolved_intent_count} 个未决请求`,
    ))
  }
  if (virtualKey.coverage_available === false
    && virtualKey.budget_limit_decimal !== null
    && virtualKey.budget_limit_decimal !== undefined) {
    details.push(l('Policy coverage awaiting plugin mounting', '策略覆盖等待挂载插件'))
  }
  if (virtualKey.budget_status === 'awaiting_plugin'
    && ['warning', 'exhausted'].includes(virtualKey.budget_financial_status ?? '')) {
    details.push(l(
      `Financial usage: ${budgetStatusLabel(virtualKey.budget_financial_status as AiBudgetStatus)}`,
      `资金使用状态：${budgetStatusLabel(virtualKey.budget_financial_status as AiBudgetStatus)}`,
    ))
  }

  return details.join(' · ')
}

const normalizedPercentage = (virtualKey: AiVirtualKey) => {
  const percentage = virtualKey.budget_percentage_decimal?.trim()

  return percentage && /^\d+(?:\.\d+)?$/.test(percentage) ? compactDecimal(percentage) : ''
}

const budgetPercentageLabel = (virtualKey: AiVirtualKey) => {
  const percentage = normalizedPercentage(virtualKey)

  return percentage ? l(
    `${percentage}% of lifetime budget`,
    `生命周期预算的 ${percentage}%`,
  ) : ''
}

const budgetProgressWidth = (virtualKey: AiVirtualKey) => {
  const percentage = normalizedPercentage(virtualKey)
  if (!percentage) {
    return ''
  }

  const [rawInteger = '0'] = percentage.split('.')
  const integer = rawInteger.replace(/^0+(?=\d)/, '') || '0'
  const atLeastOneHundred = integer.length > 3
    || (integer.length === 3 && integer >= '100')

  return atLeastOneHundred ? '100%' : `${percentage}%`
}

const budgetProgressValue = (virtualKey: AiVirtualKey) => {
  const percentage = normalizedPercentage(virtualKey)
  if (!percentage) {
    return undefined
  }

  return budgetProgressWidth(virtualKey) === '100%' ? '100' : percentage
}

const headers = computed(() => [
  { label: t('Name'), key: 'name' },
  { label: locale.value === 'zh-CN' ? '前缀' : 'Prefix', key: 'key_prefix' },
  { label: t('Allowed Models'), key: 'allowed_models' },
  { label: t('Quota'), key: 'limits' },
  { label: t('Lifecycle Budget'), key: 'budget' },
  { label: locale.value === 'zh-CN' ? '过期时间' : 'Expires', key: 'expires_at' },
  { label: t('Status'), key: 'enabled' },
  { hideLabel: true, key: 'actions' },
])

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

const viewUsage = (virtualKey: AiVirtualKey) => {
  void router.push({
    name: 'ai-usage-overview',
    query: {
      range: '24h',
      timezone: Intl.DateTimeFormat().resolvedOptions().timeZone || 'UTC',
      virtual_key_id: virtualKey.id,
    },
  })
}

const loadBudgetLedger = async (virtualKey: AiVirtualKey) => {
  if (ledgerPending.value) {
    return
  }

  ledgerKey.value = virtualKey
  ledgerPending.value = true
  ledgerError.value = ''
  reconciliationEntry.value = null

  try {
    const { data } = await apiService.get<AiBudgetLedgerResponse>(
      `ai-virtual-keys/${virtualKey.id}/budget-ledger`,
      { params: { status: 'pending,unresolved', size: 200 } },
    )
    ledgerEntries.value = data.data
    ledgerAccount.value = data.account ?? null
  } catch (err) {
    ledgerError.value = getErrorMessage(
      err,
      l('Unable to load the budget ledger', '无法加载预算账本'),
    )
  } finally {
    ledgerPending.value = false
  }
}

const closeBudgetLedger = () => {
  ledgerKey.value = null
  ledgerEntries.value = []
  ledgerAccount.value = null
  ledgerError.value = ''
  reconciliationEntry.value = null
  reconciliationReason.value = ''
  reconciliationOperationId.value = ''
  rebuildOperationId.value = ''
}

const selectReconciliation = (entry: AiBudgetLedgerEntry) => {
  reconciliationEntry.value = entry
  reconciliationAction.value = 'settle'
  reconciliationCost.value = entry.observed_cost_usd_decimal ?? ''
  reconciliationOperationId.value = crypto.randomUUID()
  ledgerError.value = ''
}

const submitReconciliation = async () => {
  if (!ledgerKey.value || !reconciliationEntry.value || ledgerPending.value) {
    return
  }
  const currentKey = ledgerKey.value
  const currentEntry = reconciliationEntry.value
  if (!reconciliationReason.value.trim()) {
    ledgerError.value = l('An audit reason is required.', '必须填写审计原因。')
    return
  }

  ledgerPending.value = true
  ledgerError.value = ''
  try {
    const cost = reconciliationAction.value === 'settle'
      ? parseBudgetLimit(reconciliationCost.value)
      : undefined
    if (reconciliationAction.value === 'settle' && cost === undefined) {
      throw new Error(l('An exact settlement cost is required.', '必须填写精确结算成本。'))
    }
    if (!reconciliationOperationId.value) {
      reconciliationOperationId.value = crypto.randomUUID()
    }
    await apiService.post(`ai-virtual-keys/${currentKey.id}/budget-reconciliations`, {
      intent_id: currentEntry.id,
      operation_id: reconciliationOperationId.value,
      ...(cost !== undefined ? { cost_usd_decimal: cost } : {}),
      waive: reconciliationAction.value === 'waive',
      reason: reconciliationReason.value.trim(),
    })
    toaster.open({
      appearance: 'success',
      message: l('Budget reconciliation applied.', '预算对账已完成。'),
    })
    reconciliationEntry.value = null
    reconciliationOperationId.value = ''
    tableKey.value += 1
    ledgerPending.value = false
    await loadBudgetLedger(currentKey)
  } catch (err) {
    ledgerError.value = getErrorMessage(
      err,
      l('Unable to reconcile the budget intent', '无法处理预算未决请求'),
    )
  } finally {
    ledgerPending.value = false
  }
}

const verifyBudgetLedger = async (dryRun: boolean) => {
  if (!ledgerKey.value || ledgerPending.value || !reconciliationReason.value.trim()) {
    return
  }
  if (!dryRun && !window.confirm(l(
    `Rebuild the authoritative budget aggregate for "${ledgerKey.value.name}"?`,
    `重建“${ledgerKey.value.name}”的权威预算汇总？`,
  ))) {
    return
  }
  const currentKey = ledgerKey.value

  ledgerPending.value = true
  ledgerError.value = ''
  if (!rebuildOperationId.value) {
    rebuildOperationId.value = crypto.randomUUID()
  }
  try {
    const { data } = await apiService.post(
      `ai-virtual-keys/${currentKey.id}/budget-ledger/rebuild`,
      {
        operation_id: rebuildOperationId.value,
        reason: reconciliationReason.value.trim(),
        dry_run: dryRun,
      },
    )
    const current = Boolean((data as { comparison?: { is_current?: boolean } }).comparison?.is_current)
    toaster.open({
      appearance: current ? 'success' : 'warning',
      message: dryRun
        ? l(
          current ? 'Budget aggregate verified.' : 'Budget aggregate drift detected.',
          current ? '预算汇总校验通过。' : '检测到预算汇总漂移。',
        )
        : l('Budget aggregate rebuilt.', '预算汇总已重建。'),
    })
    rebuildOperationId.value = ''
    tableKey.value += 1
    ledgerPending.value = false
    await loadBudgetLedger(currentKey)
  } catch (err) {
    ledgerError.value = getErrorMessage(
      err,
      l('Unable to verify the budget ledger', '无法校验预算账本'),
    )
  } finally {
    ledgerPending.value = false
  }
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
    tableErrorMessage.value = getErrorMessage(
      err,
      l('Unable to load AI virtual keys', '无法加载 AI 虚拟密钥'),
    )
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
  form.budgetLimit = virtualKey.budget_limit_decimal ?? ''
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
      tpm_limit: optionalFieldValue(parseQuotaLimit(form.tpmLimit, 'TPM')),
      rpm_limit: optionalFieldValue(parseQuotaLimit(form.rpmLimit, 'RPM')),
      budget_limit_decimal: optionalFieldValue(parseBudgetLimit(form.budgetLimit)),
      expires_at: optionalFieldValue(fromLocalDateTimeInput(form.expiresAt)),
      enabled: form.enabled,
      tags: optionalFieldValue(parseTags(form.tags)),
    })

    if (editingId.value) {
      await apiService.patch(`ai-virtual-keys/${editingId.value}`, body)
      toaster.open({
        appearance: 'success',
        message: l(`Updated virtual key ${form.name}`, `已更新虚拟密钥 ${form.name}`),
      })
    } else {
      const { data } = await apiService.post('ai-virtual-keys', body)
      const created = data as AiVirtualKey
      if (!created.key) {
        throw new Error('The server did not return the newly created virtual key')
      }

      latestKey.value = created.key
      latestKeyTitle.value = l(
        `Created virtual key ${created.name}`,
        `已创建虚拟密钥 ${created.name}`,
      )
      toaster.open({
        appearance: 'success',
        message: l(`Created virtual key ${form.name}`, `已创建虚拟密钥 ${form.name}`),
      })
    }

    cancelForm()
    tableKey.value += 1
  } catch (err) {
    errorMessage.value = getErrorMessage(
      err,
      l('Unable to save AI virtual key', '无法保存 AI 虚拟密钥'),
    )
  } finally {
    mutationPending.value = false
  }
}

const rotateVirtualKey = async (virtualKey: AiVirtualKey) => {
  if (mutationPending.value || latestKey.value) {
    return
  }

  if (!window.confirm(l(
    `Rotate AI virtual key "${virtualKey.name}"?`,
    `轮换 AI 虚拟密钥“${virtualKey.name}”？`,
  ))) {
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
    latestKeyTitle.value = l(
      `Rotated virtual key ${virtualKey.name}`,
      `已轮换虚拟密钥 ${virtualKey.name}`,
    )
    toaster.open({
      appearance: 'success',
      message: l(`Rotated virtual key ${virtualKey.name}`, `已轮换虚拟密钥 ${virtualKey.name}`),
    })
    tableKey.value += 1
  } catch (err) {
    errorMessage.value = getErrorMessage(
      err,
      l('Unable to rotate AI virtual key', '无法轮换 AI 虚拟密钥'),
    )
  } finally {
    mutationPending.value = false
  }
}

const deleteVirtualKey = async (virtualKey: AiVirtualKey) => {
  if (mutationPending.value) {
    return
  }

  if (!window.confirm(l(
    `Delete AI virtual key "${virtualKey.name}"?`,
    `删除 AI 虚拟密钥“${virtualKey.name}”？`,
  ))) {
    return
  }

  mutationPending.value = true
  errorMessage.value = ''

  try {
    await apiService.delete(`ai-virtual-keys/${virtualKey.id}`)
    toaster.open({
      appearance: 'success',
      message: l(`Deleted virtual key ${virtualKey.name}`, `已删除虚拟密钥 ${virtualKey.name}`),
    })
    tableKey.value += 1
  } catch (err) {
    errorMessage.value = getErrorMessage(
      err,
      l('Unable to delete AI virtual key', '无法删除 AI 虚拟密钥'),
    )
  } finally {
    mutationPending.value = false
  }
}

const copyLatestKey = async () => {
  await navigator.clipboard.writeText(latestKey.value)
  toaster.open({ appearance: 'success', message: l('Copied virtual key', '已复制虚拟密钥') })
}

const clearLatestKey = () => {
  latestKey.value = ''
  latestKeyTitle.value = ''
}
</script>
