<template>
  <nav
    class="ai-gateway-nav"
    :aria-label="l('AI Gateway sections', 'AI 网关分区')"
  >
    <RouterLink :to="{ name: 'ai-endpoint-list' }">
      {{ t('AI Endpoints') }}
    </RouterLink>
    <RouterLink :to="{ name: 'ai-provider-list' }">
      {{ t('Provider Connections') }}
    </RouterLink>
    <RouterLink :to="{ name: 'ai-model-list' }">
      {{ t('Advanced Models') }}
    </RouterLink>
    <RouterLink :to="{ name: 'ai-virtual-key-list' }">
      {{ t('Virtual Keys') }}
    </RouterLink>
  </nav>
</template>

<script setup lang="ts">
import { useAiGatewayI18n } from './useAiGatewayI18n'

defineOptions({
  name: 'AiGatewayNav',
})

const { l, t } = useAiGatewayI18n()
</script>
