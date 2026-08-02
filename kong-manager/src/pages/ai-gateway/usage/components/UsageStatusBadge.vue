<template>
  <KBadge :appearance="appearance">
    {{ label }}
  </KBadge>
</template>

<script setup lang="ts">
import { computed } from 'vue'
import { useI18n } from '@/composables/useI18n'
import { statusLabel } from '../aiUsageFormatters'

const props = defineProps<{
  value: string
}>()

const { t } = useI18n()

const appearance = computed(() => {
  if (['success', 'matched', 'calculated', 'hit', 'applied'].includes(props.value)) {
    return 'success' as const
  }

  if ([
    'gateway_rejected',
    'estimated',
    'not_incurred',
    'not_applicable',
    'bypassed',
  ].includes(props.value)) {
    return 'warning' as const
  }

  if ([
    'gateway_error',
    'upstream_error',
    'client_disconnected',
    'stream_interrupted',
    'unmatched',
    'unsupported',
    'unavailable',
    'degraded',
    'rejected',
  ].includes(props.value)) {
    return 'danger' as const
  }

  return 'neutral' as const
})

const label = computed(() => {
  const key = `aiUsage.values.${props.value}`
  const translated = t(key)

  return translated === key ? statusLabel(props.value) : translated
})
</script>
