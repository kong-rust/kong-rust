<template>
  <nav
    class="ai-gateway-nav"
    aria-label="AI Gateway sections"
  >
    <RouterLink :to="{ name: 'ai-gateway-overview' }">
      Overview
    </RouterLink>
    <RouterLink :to="{ name: 'ai-provider-list' }">
      Providers
    </RouterLink>
    <RouterLink :to="{ name: 'ai-model-list' }">
      Models
    </RouterLink>
    <RouterLink :to="{ name: 'ai-virtual-key-list' }">
      Virtual Keys
    </RouterLink>
  </nav>
</template>

<script setup lang="ts">
defineOptions({
  name: 'AiGatewayNav',
})
</script>
