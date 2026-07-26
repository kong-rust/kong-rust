<template>
  <div>
    <dt>{{ label }}</dt>
    <dd>{{ value }}</dd>
  </div>
</template>

<script setup lang="ts">
defineProps<{
  label: string
  value: string
}>()
</script>
