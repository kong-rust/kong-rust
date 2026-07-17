<template>
  <PageHeader title="AI Gateway" />
  <AiGatewayNav />

  <section class="ai-gateway-grid">
    <RouterLink
      class="ai-gateway-card-link"
      :to="{ name: 'ai-provider-list' }"
    >
      <KCard title="Providers">
        <div class="ai-gateway-card-body">
          <strong class="ai-gateway-card-metric">
            {{ loading ? '-' : metrics.providers }}
          </strong>
          <p class="ai-gateway-card-copy">
            Manage upstream AI providers, endpoints, credentials, and defaults.
          </p>
        </div>
      </KCard>
    </RouterLink>

    <RouterLink
      class="ai-gateway-card-link"
      :to="{ name: 'ai-model-list' }"
    >
      <KCard title="Models">
        <div class="ai-gateway-card-body">
          <strong class="ai-gateway-card-metric">
            {{ loading ? '-' : metrics.models }}
          </strong>
          <p class="ai-gateway-card-copy">
            Configure model groups, provider bindings, routing priority, and cost metadata.
          </p>
        </div>
      </KCard>
    </RouterLink>

    <RouterLink
      class="ai-gateway-card-link"
      :to="{ name: 'ai-model-list' }"
    >
      <KCard title="Model Groups">
        <div class="ai-gateway-card-body">
          <strong class="ai-gateway-card-metric">
            {{ loading ? '-' : metrics.modelGroups }}
          </strong>
          <p class="ai-gateway-card-copy">
            Models with the same group name are available for load balancing.
          </p>
        </div>
      </KCard>
    </RouterLink>

    <RouterLink
      class="ai-gateway-card-link"
      :to="{ name: 'ai-virtual-key-list' }"
    >
      <KCard title="Virtual Keys">
        <div class="ai-gateway-card-body">
          <strong class="ai-gateway-card-metric">
            {{ loading ? '-' : metrics.virtualKeys }}
          </strong>
          <p class="ai-gateway-card-copy">
            Store virtual-key management metadata. Proxy authentication, rate limits, and budget
            enforcement are not wired yet.
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

defineOptions({
  name: 'AiGatewayOverview',
})

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
    errorMessage.value = getErrorMessage(err, 'Unable to load AI Gateway metrics')
  } finally {
    loading.value = false
  }
}

onMounted(loadMetrics)
</script>
