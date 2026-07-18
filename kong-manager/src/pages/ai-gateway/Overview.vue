<template>
  <PageHeader :title="l('AI Gateway', 'AI 网关')" />
  <AiGatewayNav />

  <section class="ai-gateway-grid">
    <RouterLink
      class="ai-gateway-card-link"
      :to="{ name: 'ai-provider-list' }"
    >
      <KCard :title="l('Providers', '服务商')">
        <div class="ai-gateway-card-body">
          <strong class="ai-gateway-card-metric">
            {{ loading ? '-' : metrics.providers }}
          </strong>
          <p class="ai-gateway-card-copy">
            {{ l('Manage upstream AI providers, endpoints, credentials, and defaults.', '管理上游 AI 服务商、地址、凭据和默认设置。') }}
          </p>
        </div>
      </KCard>
    </RouterLink>

    <RouterLink
      class="ai-gateway-card-link"
      :to="{ name: 'ai-model-list' }"
    >
      <KCard :title="l('Models', '模型')">
        <div class="ai-gateway-card-body">
          <strong class="ai-gateway-card-metric">
            {{ loading ? '-' : metrics.models }}
          </strong>
          <p class="ai-gateway-card-copy">
            {{ l('Configure model groups, provider bindings, routing priority, and cost metadata.', '配置模型组、服务商绑定、路由优先级和成本元数据。') }}
          </p>
        </div>
      </KCard>
    </RouterLink>

    <RouterLink
      class="ai-gateway-card-link"
      :to="{ name: 'ai-model-list' }"
    >
      <KCard :title="l('Model Groups', '模型组')">
        <div class="ai-gateway-card-body">
          <strong class="ai-gateway-card-metric">
            {{ loading ? '-' : metrics.modelGroups }}
          </strong>
          <p class="ai-gateway-card-copy">
            {{ l('Models with the same group name are available for load balancing.', '同名模型组中的模型可用于负载均衡。') }}
          </p>
        </div>
      </KCard>
    </RouterLink>

    <RouterLink
      class="ai-gateway-card-link"
      :to="{ name: 'ai-virtual-key-list' }"
    >
      <KCard :title="l('Virtual Keys', '虚拟密钥')">
        <div class="ai-gateway-card-body">
          <strong class="ai-gateway-card-metric">
            {{ loading ? '-' : metrics.virtualKeys }}
          </strong>
          <p class="ai-gateway-card-copy">
            {{ l(
              'Store virtual-key management metadata. Proxy authentication, rate limits, and budget enforcement are not wired yet.',
              '保存虚拟密钥管理元数据；代理认证、限流和预算控制尚未接入。',
            ) }}
          </p>
        </div>
      </KCard>
    </RouterLink>
  </section>

  <KAlert
    v-if="errorMessage"
    appearance="danger"
    class="ai-gateway-overview-alert"
  >
    {{ errorMessage }}
  </KAlert>
</template>

<script setup lang="ts">
import { onMounted, reactive, ref } from 'vue'
import AiGatewayNav from './AiGatewayNav.vue'
import { apiService } from '@/services/apiService'
import type {
  AiModel,
  AiModelGroup,
  AiProvider,
  AiVirtualKey,
  KongPageResponse,
} from './types'
import { getErrorMessage } from './utils'
import { useAiGatewayI18n } from './useAiGatewayI18n'

defineOptions({
  name: 'AiGatewayOverview',
})

const { l } = useAiGatewayI18n()
const loading = ref(false)
const errorMessage = ref('')
const metrics = reactive({
  providers: 0,
  models: 0,
  modelGroups: 0,
  virtualKeys: 0,
})

const countEndpoint = async <T>(endpoint: string) => {
  const seenOffsets = new Set<string>()
  let count = 0
  let offset: string | number | undefined
  let hasMore = true

  while (hasMore) {
    const { data } = await apiService.get<KongPageResponse<T>>(endpoint, {
      params: {
        size: 1000,
        ...(offset === undefined ? {} : { offset }),
      },
    })

    count += data.data.length

    if (data.offset === null || data.offset === undefined) {
      hasMore = false
      continue
    }

    const offsetKey = String(data.offset)
    if (seenOffsets.has(offsetKey)) {
      throw new Error(`Pagination for ${endpoint} returned a repeated offset`)
    }

    seenOffsets.add(offsetKey)
    offset = data.offset
  }

  return count
}

const loadMetrics = async () => {
  loading.value = true
  errorMessage.value = ''

  try {
    const [providers, models, modelGroups, virtualKeys] = await Promise.all([
      countEndpoint<AiProvider>('ai-providers'),
      countEndpoint<AiModel>('ai-models'),
      countEndpoint<AiModelGroup>('ai-model-groups'),
      countEndpoint<AiVirtualKey>('ai-virtual-keys'),
    ])

    metrics.providers = providers
    metrics.models = models
    metrics.modelGroups = modelGroups
    metrics.virtualKeys = virtualKeys
  } catch (err) {
    errorMessage.value = getErrorMessage(
      err,
      l('Unable to load AI Gateway metrics', '无法加载 AI 网关指标'),
    )
  } finally {
    loading.value = false
  }
}

onMounted(loadMetrics)
</script>
