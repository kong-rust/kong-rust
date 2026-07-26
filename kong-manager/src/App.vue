<template>
  <AppLayout
    :sidebar-top-items="sidebarItems"
  >
    <template #navbar-right>
      <div class="navbar-actions">
        <LanguageSwitcher />
        <a
          class="github-link"
          href="https://github.com/kong-rust/kong-rust"
          rel="noopener noreferrer"
          target="_blank"
        >
          <GithubIcon :size="18" />
          GitHub
        </a>
      </div>
    </template>
    <template #sidebar-header>
      <NavbarLogo />
    </template>
    <router-view />
    <MakeAWish />
    <!-- Mounted at app level so the tour can walk the user across pages
         挂载在 App 层，使引导能带用户跨页面跳转 -->
    <OnboardingTour />
  </AppLayout>
</template>

<script setup lang="ts">
import { computed } from 'vue'
import { useRoute } from 'vue-router'
import { storeToRefs } from 'pinia'
import { AppLayout, type SidebarPrimaryItem } from '@kong-ui-public/app-layout'
import { GithubIcon } from '@kong/icons'
import { useInfoStore } from '@/stores/info'
import NavbarLogo from '@/components/NavbarLogo.vue'
import LanguageSwitcher from '@/components/LanguageSwitcher.vue'
import MakeAWish from '@/components/MakeAWish.vue'
import OnboardingTour from '@/pages/ai-gateway/components/OnboardingTour.vue'
import { useI18n } from '@/composables/useI18n'

const route = useRoute()
const infoStore = useInfoStore()
const { isHybridMode } = storeToRefs(infoStore)
const { t } = useI18n()

const sidebarItems = computed<SidebarPrimaryItem[]>(() => [
  {
    name: t('navigation.ai-gateway'),
    to: { name: 'ai-endpoint-list' },
    key: 'AI Gateway',
    active: route.meta?.entity === 'ai-gateway',
  },
  {
    name: t('navigation.overview'),
    to: { name: 'overview' },
    key: 'Overview',
    active: route.name === 'overview',
  },
  {
    name: t('navigation.services'),
    to: { name: 'service-list' },
    key: 'Gateway Services',
    active: route.meta?.entity === 'service',
  },
  {
    name: t('navigation.routes'),
    to: { name: 'route-list' },
    key: 'Routes',
    active: route.meta?.entity === 'route',
  },
  {
    name: t('navigation.consumers'),
    to: { name: 'consumer-list' },
    key: 'Consumers',
    active: route.meta?.entity === 'consumer',
  },
  {
    name: t('navigation.plugins'),
    to: { name: 'plugin-list' },
    key: 'Plugins',
    active: route.meta?.entity === 'plugin',
  },
  {
    name: t('navigation.upstreams'),
    to: { name: 'upstream-list' },
    key: 'Upstreams',
    active: route.meta?.entity === 'upstream',
  },
  {
    name: t('navigation.certificates'),
    to: { name: 'certificate-list' },
    key: 'Certificates',
    active: route.meta?.entity === 'certificate',
  },
  {
    name: t('navigation.ca-certificates'),
    to: { name: 'ca-certificate-list' },
    key: 'CA Certificates',
    active: route.meta?.entity === 'ca-certificate',
  },
  {
    name: t('navigation.snis'),
    to: { name: 'sni-list' },
    key: 'SNIs',
    active: route.meta?.entity === 'sni',
  },
  {
    name: t('navigation.vaults'),
    to: { name: 'vault-list' },
    key: 'Vaults',
    active: route.meta?.entity === 'vault',
  },
  {
    name: t('navigation.keys'),
    to: { name: 'key-list' },
    key: 'Keys',
    active: route.meta?.entity === 'key',
  },
  {
    name: t('navigation.key-sets'),
    to: { name: 'key-set-list' },
    key: 'Key Sets',
    active: route.meta?.entity === 'key-set',
  },
  ...(
    isHybridMode.value
      ? [
        // {
        //   name: 'Data Plane Nodes',
        //   to: { name: 'data-plane-nodes' },
        //   key: 'Data Plane Nodes',
        //   active: route.meta?.entity === 'data-plane-node',
        // },
      ]
      : []
  ),
])
</script>

<style scoped lang="scss">
.navbar-actions {
  align-items: center;
  display: flex;
}

.github-link {
  align-items: center;
  color: $kui-color-text-inverse;
  display: inline-flex;
  font-size: $kui-font-size-30;
  font-weight: $kui-font-weight-semibold;
  gap: $kui-space-30;
  text-decoration: none;

  &:hover {
    color: $kui-color-text-primary-weakest;
  }
}

:deep(.kong-ui-app-layout-content-inner) {
  position: relative;
  min-height: 100%;
  padding: 32px 40px 80px !important;
}

:deep(.json-content.k-code-block) {
  border-top-left-radius: $kui-border-radius-0 !important;
  border-top-right-radius: $kui-border-radius-0 !important;
}
</style>
