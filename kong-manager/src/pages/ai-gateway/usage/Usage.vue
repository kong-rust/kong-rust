<template>
  <PageHeader :title="t('aiUsage.title')" />
  <AiGatewayNav />

  <nav
    class="ai-usage-subnav"
    :aria-label="t('aiUsage.navigation.label')"
  >
    <RouterLink
      :to="{ name: 'ai-usage-overview', query: route.query }"
    >
      {{ t('aiUsage.navigation.overview') }}
    </RouterLink>
    <RouterLink
      :to="{ name: 'ai-usage-logs', query: route.query }"
    >
      {{ t('aiUsage.navigation.logs') }}
    </RouterLink>
  </nav>

  <UsageFilterBar
    :filters="appliedFilters"
    :loading="loading"
    @apply="controller.applyFilters"
  />

  <RouterView v-slot="{ Component }">
    <component
      :is="Component"
      :controller="controller"
    />
  </RouterView>
</template>

<script setup lang="ts">
import { computed } from 'vue'
import { useRoute } from 'vue-router'
import { useI18n } from '@/composables/useI18n'
import AiGatewayNav from '../AiGatewayNav.vue'
import UsageFilterBar from './components/UsageFilterBar.vue'
import { useAiUsageController } from './useAiUsageController'

defineOptions({
  name: 'AiGatewayUsage',
})

const route = useRoute()
const { t } = useI18n()
const controller = useAiUsageController()
const appliedFilters = computed(() => controller.filters.value)
const loading = computed(() => controller.isLoading.value)
</script>
