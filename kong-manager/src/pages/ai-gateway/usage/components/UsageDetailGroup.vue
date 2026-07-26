<template>
  <section class="ai-usage-detail-group">
    <h3>{{ title }}</h3>
    <dl>
      <slot />
    </dl>
  </section>
</template>

<script setup lang="ts">
defineProps<{
  title: string
}>()
</script>
