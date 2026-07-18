import { unref } from 'vue'
import english from '@/locales/en.json'
import chinese from '@/locales/zh-CN.json'
import { useLocale } from '@/composables/useLocale'

type TranslationParams = Record<string, unknown>
type TranslationSource = Record<string, unknown>

const flattenMessages = (
  source: TranslationSource,
  prefix = '',
  messages: Record<string, string> = {},
) => {
  for (const [key, value] of Object.entries(source)) {
    const path = prefix ? `${prefix}.${key}` : key

    if (value && typeof value === 'object') {
      flattenMessages(value as TranslationSource, path, messages)
    } else if (typeof value === 'string') {
      messages[path] = value
    }
  }

  return messages
}

const englishMessages = flattenMessages(english)
const chineseMessages = flattenMessages(chinese)

const interpolate = (message: string, params: TranslationParams) => (
  message.replace(/\{([^{}]+)\}/g, (match, name: string) => (
    Object.prototype.hasOwnProperty.call(params, name)
      ? String(unref(params[name]))
      : match
  ))
)

export const useI18n = () => {
  const { locale } = useLocale()

  const t = (key: string, params: TranslationParams = {}) => {
    const source = locale.value === 'zh-CN' ? chineseMessages : englishMessages
    const message = source[key] ?? englishMessages[key] ?? key

    return interpolate(message, params)
  }

  return {
    t,
  }
}
