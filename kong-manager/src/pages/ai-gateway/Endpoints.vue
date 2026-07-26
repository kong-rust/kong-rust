<template>
  <PageHeader :title="t('AI Endpoints')">
    <KButton
      appearance="secondary"
      type="button"
      @click="openTour"
    >
      {{ t('Guide') }}
    </KButton>
    <KButton
      data-tour="create-endpoint"
      :disabled="saving"
      @click="startCreate"
    >
      {{ t('Create Endpoint') }}
    </KButton>
  </PageHeader>
  <AiGatewayNav />

  <section class="ai-endpoint-hero">
    <div>
      <span class="ai-endpoint-eyebrow">{{ t('AI traffic starts here') }}</span>
      <h2>{{ t('Publish one URL. Route it to the right models.') }}</h2>
      <p>
        {{ t('Create an OpenAI-compatible chat endpoint without configuring services, routes, or plugins by hand.') }}
      </p>
    </div>
    <div class="ai-endpoint-flow">
      <span>{{ t('Your app') }}</span>
      <strong>→</strong>
      <span>{{ t('Kong AI Endpoint') }}</span>
      <strong>→</strong>
      <span>{{ t('AI providers') }}</span>
    </div>
  </section>

  <KAlert
    v-if="errorMessage"
    appearance="danger"
    class="ai-gateway-alert"
  >
    {{ errorMessage }}
  </KAlert>

  <KCard
    v-if="formVisible"
    class="ai-gateway-form-card ai-endpoint-editor"
    :title="editingEndpoint ? `${t('Configure')} ${editingEndpoint.displayName}` : t('Create AI Endpoint')"
  >
    <form
      class="ai-gateway-form"
      @submit.prevent="saveEndpoint"
    >
      <EndpointIdentityForm
        v-model:display-name="draft.displayName"
        v-model:enabled="draft.enabled"
        v-model:require-auth="draft.requireAuth"
        v-model:slug="draft.slug"
      />

      <ModelPoolBuilder
        v-model:models="draft.models"
        :providers="providers"
      />

      <TrafficPolicyEditor v-model:models="draft.models" />

      <section class="ai-endpoint-section ai-endpoint-publish-summary">
        <div class="ai-endpoint-section-heading">
          <span class="ai-endpoint-step">4</span>
          <div>
            <h3>{{ t('Publish') }}</h3>
            <p>{{ t('Review the public URL and model traffic before saving.') }}</p>
          </div>
        </div>

        <dl>
          <div>
            <dt>{{ t('Public URL') }}</dt>
            <dd class="ai-gateway-mono">
              {{ proxyBaseUrl }}{{ draftPath }}
            </dd>
          </div>
          <div>
            <dt>{{ t('Models') }}</dt>
            <dd>
              {{ modelCountLabel }}
            </dd>
          </div>
          <div>
            <dt>{{ t('Protocol') }}</dt>
            <dd>{{ t('OpenAI Chat Completions') }}</dd>
          </div>
        </dl>
      </section>

      <div class="ai-gateway-form-actions ai-endpoint-editor-actions">
        <KButton
          :disabled="saving"
          type="submit"
        >
          {{ saving ? t('Saving...') : (editingEndpoint ? t('Save changes') : t('Publish Endpoint')) }}
        </KButton>
        <KButton
          appearance="secondary"
          :disabled="saving"
          type="button"
          @click="cancelForm"
        >
          {{ t('Cancel') }}
        </KButton>
      </div>
    </form>
  </KCard>

  <section
    v-if="loading"
    class="ai-endpoint-empty"
  >
    {{ t('Loading AI endpoints...') }}
  </section>

  <section
    v-else-if="endpoints.length === 0"
    class="ai-endpoint-empty"
  >
    <div class="ai-endpoint-empty-icon">
      AI
    </div>
    <h2>{{ t('No AI endpoints yet') }}</h2>
    <p>{{ t('Publish your first endpoint to get a URL your applications can call.') }}</p>
    <KButton @click="startCreate">
      {{ t('Create your first endpoint') }}
    </KButton>
  </section>

  <section
    v-else
    class="ai-endpoint-list"
    :aria-label="t('Published AI endpoints')"
  >
    <article
      v-for="(endpoint, endpointIndex) in endpoints"
      :key="endpoint.id"
      class="ai-endpoint-card"
      :data-tour="endpointIndex === 0 ? 'endpoint-card' : undefined"
    >
      <div class="ai-endpoint-card-header">
        <div>
          <div class="ai-endpoint-title-row">
            <h2>{{ endpoint.displayName }}</h2>
            <KBadge :appearance="endpointStatus(endpoint).appearance">
              {{ endpointStatus(endpoint).label }}
            </KBadge>
          </div>
          <div class="ai-endpoint-url">
            <span>POST</span>
            <code>{{ proxyBaseUrl }}{{ endpoint.path }}</code>
          </div>
        </div>

        <div class="ai-gateway-row-actions">
          <KButton
            appearance="secondary"
            size="small"
            @click="copyEndpoint(endpoint)"
          >
            {{ t('Copy URL') }}
          </KButton>
          <KButton
            appearance="secondary"
            :disabled="!endpoint.complete"
            size="small"
            @click="openPlayground(endpoint)"
          >
            {{ t('Test') }}
          </KButton>
          <KButton
            appearance="secondary"
            :disabled="!endpoint.complete"
            size="small"
            @click="startEdit(endpoint)"
          >
            {{ t('Configure') }}
          </KButton>
          <KButton
            appearance="danger"
            size="small"
            @click="removeEndpoint(endpoint)"
          >
            {{ t('Delete') }}
          </KButton>
        </div>
      </div>

      <div class="ai-endpoint-model-summary">
        <div
          v-for="model in endpoint.models"
          :key="model.id"
          class="ai-endpoint-model-chip"
        >
          <span>{{ providerName(endpoint, model.provider_id) }}</span>
          <strong>{{ model.model_name }}</strong>
          <em>{{ t('Weight') }} {{ model.weight }} · {{ trafficShare(endpoint, model.weight) }}%</em>
        </div>
        <p v-if="endpoint.models.length === 0">
          {{ t('No models are attached. Repair this endpoint in Advanced Models.') }}
        </p>
      </div>

      <KAlert
        v-if="!endpoint.complete"
        appearance="warning"
      >
        {{ t('This endpoint is incomplete because one or more managed resources are missing. Its remaining resources are preserved for repair or safe deletion.') }}
      </KAlert>
    </article>
  </section>

  <KCard
    v-if="playgroundEndpoint"
    class="ai-gateway-form-card"
    :title="`Test ${playgroundEndpoint.displayName}`"
  >
    <EndpointPlayground
      :endpoint-path="playgroundEndpoint.path"
      :endpoint-url="`${proxyBaseUrl}${playgroundEndpoint.path}`"
      :model-group="playgroundEndpoint.modelGroup"
    />
    <div class="ai-gateway-form-actions">
      <KButton
        appearance="tertiary"
        type="button"
        @click="playgroundEndpoint = null"
      >
        {{ t('Close') }}
      </KButton>
    </div>
  </KCard>
</template>

<script setup lang="ts">
import { computed, onMounted, reactive, ref } from 'vue'
import AiGatewayNav from './AiGatewayNav.vue'
import EndpointIdentityForm from './components/EndpointIdentityForm.vue'
import EndpointPlayground from './components/EndpointPlayground.vue'
import ModelPoolBuilder from './components/ModelPoolBuilder.vue'
import TrafficPolicyEditor from './components/TrafficPolicyEditor.vue'
import { useOnboardingTour } from './useOnboardingTour'
import type { AiEndpoint, EndpointDraft } from './endpointTypes'
import type { AiProvider } from './types'
import {
  endpointPath,
  endpointToDraft,
  newModelDraft,
  normalizeSlug,
} from './endpointUtils'
import {
  buildEndpoints,
  createEndpoint,
  deleteEndpoint,
  loadEndpointResources,
  updateEndpoint,
} from './useEndpointPublisher'
import { getErrorMessage } from './utils'
import { useToaster } from '@/composables/useToaster'
import { useAiGatewayI18n } from './useAiGatewayI18n'

defineOptions({
  name: 'AiGatewayEndpoints',
})

const toaster = useToaster()
const { l, locale, t } = useAiGatewayI18n()
const endpoints = ref<AiEndpoint[]>([])
const providers = ref<AiProvider[]>([])
const editingEndpoint = ref<AiEndpoint | null>(null)
const playgroundEndpoint = ref<AiEndpoint | null>(null)
const formVisible = ref(false)
const loading = ref(true)
const saving = ref(false)
// Onboarding tour lives at app level so it can navigate — 引导挂载在 App 层以便跨页面跳转
const { open: openTour } = useOnboardingTour()
const errorMessage = ref('')

const initialDraft = (): EndpointDraft => {
  const model = newModelDraft()

  model.providerId = providers.value[0]?.id ?? ''
  model.providerMode = providers.value.length ? 'existing' : 'new'

  return {
    displayName: '',
    slug: '',
    enabled: true,
    requireAuth: false,
    models: [model],
  }
}

const draft = reactive<EndpointDraft>(initialDraft())

const proxyBaseUrl = computed(() => {
  const port = window.location.protocol === 'https:' ? 8443 : 8000

  return `${window.location.protocol}//${window.location.hostname}:${port}`
})

const draftPath = computed(() => endpointPath(normalizeSlug(draft.slug) || 'your-endpoint'))
const modelCountLabel = computed(() => (
  locale.value === 'zh-CN'
    ? `${draft.models.length} 个模型`
    : `${draft.models.length} model${draft.models.length === 1 ? '' : 's'}`
))

const trafficShare = (endpoint: AiEndpoint, weight: number) => {
  const totalWeight = endpoint.models.reduce((sum, model) => sum + model.weight, 0)

  return totalWeight > 0 ? Math.round(weight / totalWeight * 100) : 0
}

const replaceDraft = (value: EndpointDraft) => {
  draft.id = value.id
  draft.displayName = value.displayName
  draft.slug = value.slug
  draft.enabled = value.enabled
  draft.requireAuth = value.requireAuth
  draft.models = value.models
}

const refresh = async () => {
  loading.value = true
  errorMessage.value = ''

  try {
    // Fetch resources once and derive endpoints from them — 只拉取一次资源，再据此派生接口列表
    const resources = await loadEndpointResources()
    endpoints.value = buildEndpoints(resources)
    providers.value = resources.providers
  } catch (error) {
    errorMessage.value = getErrorMessage(error, 'Unable to load AI endpoints')
  } finally {
    loading.value = false
  }
}

const startCreate = () => {
  editingEndpoint.value = null
  playgroundEndpoint.value = null
  errorMessage.value = ''
  replaceDraft(initialDraft())
  formVisible.value = true
  window.scrollTo({ top: 0, behavior: 'smooth' })
}

const startEdit = (endpoint: AiEndpoint) => {
  editingEndpoint.value = endpoint
  playgroundEndpoint.value = null
  errorMessage.value = ''
  replaceDraft(endpointToDraft(endpoint))
  formVisible.value = true
  window.scrollTo({ top: 0, behavior: 'smooth' })
}

const cancelForm = () => {
  formVisible.value = false
  editingEndpoint.value = null
  errorMessage.value = ''
  replaceDraft(initialDraft())
}

const saveEndpoint = async () => {
  saving.value = true
  errorMessage.value = ''

  try {
    if (editingEndpoint.value) {
      await updateEndpoint(editingEndpoint.value, draft)
      toaster.open({
        appearance: 'success',
        message: l(`Updated ${draft.displayName}`, `已更新 ${draft.displayName}`),
      })
    } else {
      await createEndpoint(draft)
      toaster.open({
        appearance: 'success',
        message: l(`Published ${draft.displayName}`, `已发布 ${draft.displayName}`),
      })
    }

    cancelForm()
    await refresh()
  } catch (error) {
    errorMessage.value = getErrorMessage(
      error,
      l('Unable to save AI endpoint', '无法保存 AI 接口'),
    )
  } finally {
    saving.value = false
  }
}

const removeEndpoint = async (endpoint: AiEndpoint) => {
  if (!window.confirm(l(
    `Delete AI endpoint "${endpoint.displayName}"? Provider connections will be kept.`,
    `删除 AI 接口“${endpoint.displayName}”？服务商连接将被保留。`,
  ))) {
    return
  }

  errorMessage.value = ''

  try {
    await deleteEndpoint(endpoint)
    toaster.open({
      appearance: 'success',
      message: l(`Deleted ${endpoint.displayName}`, `已删除 ${endpoint.displayName}`),
    })
    if (playgroundEndpoint.value?.id === endpoint.id) {
      playgroundEndpoint.value = null
    }
    await refresh()
  } catch (error) {
    errorMessage.value = getErrorMessage(
      error,
      l('Unable to delete AI endpoint', '无法删除 AI 接口'),
    )
  }
}

const copyEndpoint = async (endpoint: AiEndpoint) => {
  await navigator.clipboard.writeText(`${proxyBaseUrl.value}${endpoint.path}`)
  toaster.open({
    appearance: 'success',
    message: l('Copied endpoint URL', '已复制接口地址'),
  })
}

const openPlayground = (endpoint: AiEndpoint) => {
  playgroundEndpoint.value = endpoint
  window.setTimeout(() => window.scrollTo({ top: document.body.scrollHeight, behavior: 'smooth' }))
}

const providerName = (endpoint: AiEndpoint, providerId: string) => {
  return endpoint.providers.find(provider => provider.id === providerId)?.name
    ?? t('Missing provider')
}

const endpointStatus = (endpoint: AiEndpoint) => {
  if (!endpoint.complete) {
    return { label: t('Needs attention'), appearance: 'warning' as const }
  }

  if (!endpoint.enabled) {
    return { label: t('Disabled'), appearance: 'neutral' as const }
  }

  return { label: t('Running'), appearance: 'success' as const }
}

onMounted(refresh)
</script>
