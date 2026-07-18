import { computed } from 'vue'
import { useRoute, useRouter, type RouteLocationRaw, type RouteLocationNamedRaw } from 'vue-router'
import { useI18n } from '@/composables/useI18n'

interface Tab {
  titleKey: string
  route: RouteLocationRaw & { name: string }
}

const titleKeyToHash = (titleKey: string) => {
  const segments = titleKey.split('.')

  return `#${segments[segments.length - 1]}`
}

// <KTabs> expects hash-based objects, while Manager tabs navigate between routes.
// Translation-key suffixes keep the existing hashes stable when titles change.
export const useTabs = (tabs: Tab[]) => {
  const route = useRoute()
  const router = useRouter()
  const { t } = useI18n()

  const initialTab = computed(() => tabs.find((tab) => tab.route.name === route.name))
  const initialHash = computed(() => (
    initialTab.value ? titleKeyToHash(initialTab.value.titleKey) : ''
  ))

  const kongponentTabs = computed(() => tabs.map((tab) => ({
    title: t(tab.titleKey),
    hash: titleKeyToHash(tab.titleKey),
  })))

  const onTabChange = (hash: string) => {
    const activeTab = tabs.find((tab) => titleKeyToHash(tab.titleKey) === hash)
    if (!activeTab) {
      return
    }

    router.push({
      query: route.query,
      ...(activeTab.route as RouteLocationNamedRaw),
    })
  }

  return {
    kongponentTabs,
    onTabChange,
    initialHash,
  }
}
