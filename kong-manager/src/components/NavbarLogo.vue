<template>
  <div class="brand-logo">
    <router-link
      :to="{ name: 'overview' }"
      :title="t('navigation.overview')"
    >
      <span
        class="brand-mark"
        aria-hidden="true"
      >KR</span>
      <span>Kong Rust Manager</span>
    </router-link>
  </div>
</template>

<script setup lang="ts">
import { useI18n } from '@/composables/useI18n'

const { t } = useI18n()
</script>

<style scoped lang="scss">
.brand-logo a {
  align-items: center;
  color: $kui-color-text-inverse;
  display: inline-flex;
  font-size: $kui-font-size-40;
  font-weight: $kui-font-weight-semibold;
  gap: $kui-space-40;
  line-height: 1.2;
  text-decoration: none;
}

.brand-mark {
  align-items: center;
  background: $kui-color-background-primary-strong;
  border-radius: $kui-border-radius-30;
  color: $kui-color-text-inverse;
  display: inline-flex;
  font-size: $kui-font-size-20;
  height: 32px;
  justify-content: center;
  letter-spacing: -0.04em;
  width: 32px;
}
</style>
