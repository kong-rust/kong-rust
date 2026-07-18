import { readonly, ref } from 'vue'

export type ManagerLocale = 'en' | 'zh-CN'

const localeStorageKey = 'kong-rust-manager-locale'
const legacyLocaleStorageKey = 'kong-manager-ai-gateway-locale'

const isManagerLocale = (value: string | null): value is ManagerLocale => (
  value === 'en' || value === 'zh-CN'
)

const savedLocale = typeof window === 'undefined'
  ? null
  : window.localStorage.getItem(localeStorageKey)
    ?? window.localStorage.getItem(legacyLocaleStorageKey)
const browserLocale: ManagerLocale = typeof navigator !== 'undefined'
  && navigator.language.toLowerCase().startsWith('zh')
  ? 'zh-CN'
  : 'en'
const locale = ref<ManagerLocale>(isManagerLocale(savedLocale) ? savedLocale : browserLocale)

const applyDocumentLocale = (value: ManagerLocale) => {
  if (typeof document !== 'undefined') {
    document.documentElement.lang = value
  }
}

applyDocumentLocale(locale.value)

export const useLocale = () => {
  const setLocale = (value: ManagerLocale) => {
    locale.value = value
    window.localStorage.setItem(localeStorageKey, value)
    window.localStorage.removeItem(legacyLocaleStorageKey)
    applyDocumentLocale(value)
  }

  return {
    locale: readonly(locale),
    setLocale,
  }
}
