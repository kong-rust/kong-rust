<template>
  <section class="not-found">
    <h2>404</h2>
    <p>{{ t('not-found.description') }}</p>
    <router-link :to="{ name: 'overview' }">
      {{ t('not-found.return-home') }}
    </router-link>
  </section>
</template>

<script setup lang="ts">
import { useI18n } from '@/composables/useI18n'

const { t } = useI18n()
</script>

<style lang="scss" scoped>
.not-found {
  height: 400px;
  display: flex;
  flex-direction: column;
  justify-content: center;
  align-items: center;
  text-align: center;

  h2 {
    font-size: 72px;
    color: $kui-color-text-disabled;
    line-height: 1;
  }

  p {
    max-width: 400px;
  }
}
</style>
