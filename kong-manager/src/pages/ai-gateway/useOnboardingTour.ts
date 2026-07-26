import { computed, ref } from 'vue'

/**
 * Shared onboarding-tour state — the tour is mounted at the app level so it can
 * navigate between pages, while any page can open it.
 * 使用引导的共享状态 — 引导挂载在 App 层以便跨页面跳转，同时任何页面都能打开它。
 */

// Bump the version to re-show the tour after a substantial content change
// 引导内容有大改动时提升版本号，可让用户重新看到
const STORAGE_KEY = 'kong-rust:ai-gateway-tour'
const TOUR_VERSION = '2'

const active = ref(false)
const stepIndex = ref(0)

const hasSeenTour = () => {
  try {
    return window.localStorage.getItem(STORAGE_KEY) === TOUR_VERSION
  } catch {
    // Private browsing or storage disabled — treat as unseen
    // 隐私模式或存储被禁用 — 视为未看过
    return false
  }
}

const rememberSeen = () => {
  try {
    window.localStorage.setItem(STORAGE_KEY, TOUR_VERSION)
  } catch {
    // Storage unavailable — the tour simply shows again next time
    // 存储不可用 — 引导下次会再次出现，不影响功能
  }
}

export const useOnboardingTour = () => {
  const open = () => {
    stepIndex.value = 0
    active.value = true
  }

  /** Open only on a first visit — 仅首次访问时打开 */
  const openIfFirstVisit = () => {
    if (!hasSeenTour()) {
      open()
    }
  }

  const close = () => {
    active.value = false
    rememberSeen()
  }

  const goTo = (index: number) => {
    stepIndex.value = index
  }

  return {
    active: computed(() => active.value),
    close,
    goTo,
    open,
    openIfFirstVisit,
    stepIndex: computed(() => stepIndex.value),
  }
}
