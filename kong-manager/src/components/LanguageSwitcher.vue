<template>
  <label class="manager-language-switcher">
    <span>{{ t('global.language') }}</span>
    <select
      :aria-label="t('global.language')"
      :value="locale"
      @change="changeLocale"
    >
      <option value="en">
        English
      </option>
      <option value="zh-CN">
        简体中文
      </option>
    </select>
  </label>
</template>

<script setup lang="ts">
import type { ManagerLocale } from '@/composables/useLocale'
import { useI18n } from '@/composables/useI18n'
import { useLocale } from '@/composables/useLocale'

const { t } = useI18n()
const { locale, setLocale } = useLocale()

const changeLocale = (event: Event) => {
  setLocale((event.target as HTMLSelectElement).value as ManagerLocale)
}
</script>

<style scoped lang="scss">
.manager-language-switcher {
  align-items: center;
  color: $kui-color-text-inverse;
  display: inline-flex;
  font-size: $kui-font-size-20;
  gap: $kui-space-30;
  margin-right: $kui-space-50;

  select {
    background-color: $kui-color-background-neutral-weakest;
    border: $kui-border-width-10 solid $kui-color-border;
    border-radius: $kui-border-radius-30;
    color: $kui-color-text-neutral-strongest;
    cursor: pointer;
    min-height: 32px;
    padding: 0 $kui-space-40;
  }
}
</style>
