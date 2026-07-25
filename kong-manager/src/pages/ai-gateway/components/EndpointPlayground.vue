<template>
  <section class="ai-endpoint-playground">
    <div class="ai-endpoint-section-heading">
      <div>
        <h3>{{ t('Test endpoint') }}</h3>
        <p>{{ t('Send a real request through the published Kong proxy route.') }}</p>
      </div>
    </div>

    <div class="ai-gateway-form-field">
      <label for="ai-endpoint-test-message">{{ t('Message') }}</label>
      <textarea
        id="ai-endpoint-test-message"
        v-model="message"
        :placeholder="l('Hello! Tell me what you can do.', '你好！请介绍一下你能做什么。')"
      />
    </div>

    <div class="ai-gateway-form-field">
      <label for="ai-endpoint-test-key">{{ t('API Key') }}</label>
      <input
        id="ai-endpoint-test-key"
        v-model="apiKey"
        autocomplete="off"
        :placeholder="l('sk-kr-...', 'sk-kr-...')"
        type="password"
      >
      <p class="ai-endpoint-hint">
        {{ t('Optional. Required when the endpoint enforces virtual keys.') }}
      </p>
    </div>

    <label class="ai-gateway-checkbox">
      <input
        v-model="stream"
        type="checkbox"
      >
      {{ t('Stream the response') }}
    </label>

    <div class="ai-gateway-form-actions">
      <KButton
        :disabled="testing || !message.trim()"
        type="button"
        @click="sendRequest"
      >
        {{ testing ? t('Sending...') : t('Send test request') }}
      </KButton>
      <KButton
        appearance="secondary"
        type="button"
        @click="copyCurl"
      >
        {{ t('Copy curl') }}
      </KButton>
    </div>

    <KAlert
      v-if="errorMessage"
      appearance="danger"
    >
      {{ errorMessage }}
    </KAlert>

    <div
      v-if="result"
      class="ai-endpoint-test-result"
    >
      <div class="ai-endpoint-test-metrics">
        <span>{{ t('Status') }} <strong>{{ result.status }}</strong></span>
        <span>{{ t('Model') }} <strong>{{ result.model || '-' }}</strong></span>
        <span>{{ t('Time') }} <strong>{{ result.duration }} ms</strong></span>
      </div>
      <pre>{{ result.body }}</pre>
    </div>
  </section>
</template>

<script setup lang="ts">
import { computed, ref } from 'vue'
import { useToaster } from '@/composables/useToaster'
import { apiService } from '@/services/apiService'
import { useAiGatewayI18n } from '../useAiGatewayI18n'

const props = defineProps<{
  endpointPath: string
  endpointUrl: string
  modelGroup: string
}>()

const toaster = useToaster()
const { l, t } = useAiGatewayI18n()
const message = ref('Hello! Tell me what you can do.')
// Held only for the duration of the test call — never persisted or logged
// 仅在本次测试调用期间保留 — 不持久化、不记录日志
const apiKey = ref('')
const stream = ref(false)
const testing = ref(false)
const errorMessage = ref('')
const result = ref<{ status: number, model: string, duration: number, body: string } | null>(null)
interface EndpointTestResponse {
  status: number
  model?: string | null
  body: string
}

const requestBody = computed(() => ({
  model: props.modelGroup,
  messages: [{ role: 'user', content: message.value }],
  stream: stream.value,
}))

const curl = computed(() => (
  `curl ${stream.value ? '-N ' : ''}-X POST '${props.endpointUrl}' \\\n`
  + "  -H 'Content-Type: application/json' \\\n"
  + (apiKey.value ? `  -H 'Authorization: Bearer ${apiKey.value}' \\\n` : '')
  + `  -d '${JSON.stringify(requestBody.value)}'`
))

const sendRequest = async () => {
  testing.value = true
  errorMessage.value = ''
  result.value = null
  const startedAt = performance.now()

  try {
    const response = await apiService.post('ai-endpoint-test', {
      path: props.endpointPath,
      request: requestBody.value,
      ...(apiKey.value ? { api_key: apiKey.value } : {}),
    })
    const data = response.data as EndpointTestResponse

    result.value = {
      status: data.status,
      model: data.model ?? '',
      duration: Math.round(performance.now() - startedAt),
      body: data.body,
    }

    if (data.status < 200 || data.status >= 300) {
      errorMessage.value = l(
        `The endpoint returned HTTP ${data.status}`,
        `接口返回 HTTP ${data.status}`,
      )
    }
  } catch (error) {
    const responseMessage = (
      typeof error === 'object'
      && error !== null
      && 'response' in error
      && typeof error.response === 'object'
      && error.response !== null
      && 'data' in error.response
      && typeof error.response.data === 'object'
      && error.response.data !== null
      && 'message' in error.response.data
      && typeof error.response.data.message === 'string'
    )
      ? error.response.data.message
      : null

    errorMessage.value = responseMessage
      ?? (error instanceof Error ? error.message : l('Unable to call the endpoint', '无法调用接口'))
  } finally {
    testing.value = false
  }
}

const copyCurl = async () => {
  await navigator.clipboard.writeText(curl.value)
  toaster.open({
    appearance: 'success',
    message: l('Copied curl command', '已复制 curl 命令'),
  })
}
</script>
