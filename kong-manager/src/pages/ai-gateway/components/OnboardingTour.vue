<template>
  <div
    v-if="active"
    class="ai-tour"
    role="dialog"
    aria-modal="true"
  >
    <!-- Spotlight cutout over the anchored element — 锚定元素上的高亮镂空 -->
    <div
      v-if="spotlight"
      class="ai-tour-spotlight"
      :style="spotlightStyle"
    />
    <!-- Plain dimmer when a step has no anchor — 无锚定元素时的普通遮罩 -->
    <div
      v-else
      class="ai-tour-backdrop"
    />

    <div
      ref="popover"
      class="ai-tour-popover"
      :class="[`ai-tour-popover-${placement}`, { 'ai-tour-popover-centered': !spotlight }]"
      :style="popoverStyle"
    >
      <header class="ai-tour-header">
        <h2>{{ step.title }}</h2>
        <button
          class="ai-tour-close"
          type="button"
          :aria-label="t('Close')"
          @click="finish"
        >
          ×
        </button>
      </header>

      <div class="ai-tour-body">
        <p class="ai-tour-lead">
          {{ step.lead }}
        </p>

        <ol
          v-if="step.actions?.length"
          class="ai-tour-actions"
        >
          <li
            v-for="action in step.actions"
            :key="action"
          >
            {{ action }}
          </li>
        </ol>

        <div
          v-if="step.code"
          class="ai-tour-code"
        >
          <header>
            <strong>{{ step.codeTitle }}</strong>
            <button
              class="ai-tour-copy"
              type="button"
              @click="copy(step.code)"
            >
              {{ t('Copy') }}
            </button>
          </header>
          <pre>{{ step.code }}</pre>
        </div>

        <p
          v-if="step.note"
          class="ai-tour-note"
        >
          {{ step.note }}
        </p>

        <p
          v-if="targetMissing"
          class="ai-tour-note ai-tour-note-warning"
        >
          {{ step.missingHint ?? t('That element is not on screen right now — the description above still applies.') }}
        </p>
      </div>

      <footer class="ai-tour-footer">
        <span class="ai-tour-progress">
          <button
            v-for="(_, index) in steps"
            :key="index"
            class="ai-tour-dot"
            :class="{ 'ai-tour-dot-active': index === stepIndex, 'ai-tour-dot-done': index < stepIndex }"
            type="button"
            :aria-label="`${t('Step')} ${index + 1}`"
            @click="goToStep(index)"
          />
          <span class="ai-tour-count">{{ stepIndex + 1 }} / {{ steps.length }}</span>
        </span>

        <span class="ai-tour-buttons">
          <KButton
            appearance="tertiary"
            size="small"
            type="button"
            @click="finish"
          >
            {{ isLast ? t('Close') : t('Skip') }}
          </KButton>
          <KButton
            v-if="stepIndex > 0"
            appearance="secondary"
            size="small"
            type="button"
            @click="back"
          >
            {{ t('Back') }}
          </KButton>
          <KButton
            size="small"
            type="button"
            @click="next"
          >
            {{ isLast ? t('Start using it') : t('Next') }}
          </KButton>
        </span>
      </footer>
    </div>
  </div>
</template>

<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { useRouter } from 'vue-router'
import { useToaster } from '@/composables/useToaster'
import { useAiGatewayI18n } from '../useAiGatewayI18n'
import { useOnboardingTour } from '../useOnboardingTour'

// Gap between the highlighted element and the popover — 高亮元素与气泡之间的间距
const POPOVER_GAP = 14
// Padding of the spotlight ring around the element — 高亮环相对元素的内边距
const SPOTLIGHT_PADDING = 6
// How long to wait for a target to render after navigating — 跳转后等待目标元素渲染的时长
const TARGET_TIMEOUT_MS = 2500

interface TourStep {
  title: string
  lead: string
  /** Route to navigate to before showing this step — 展示该步骤前跳转到的路由 */
  route?: string
  /** CSS selector of the element to highlight — 要高亮的元素选择器 */
  target?: string
  actions?: string[]
  codeTitle?: string
  code?: string
  note?: string
  /** Shown when the target cannot be found — 找不到目标元素时展示 */
  missingHint?: string
}

const router = useRouter()
const toaster = useToaster()
const { l, t } = useAiGatewayI18n()
const { active, close, goTo, openIfFirstVisit, stepIndex } = useOnboardingTour()

const spotlight = ref<{ top: number, left: number, width: number, height: number } | null>(null)
const placement = ref<'top' | 'bottom'>('bottom')
const popover = ref<HTMLElement | null>(null)
const popoverStyle = ref<Record<string, string>>({})
const targetMissing = ref(false)

const proxyBaseUrl = computed(() => {
  const port = window.location.protocol === 'https:' ? 8443 : 8000

  return `${window.location.protocol}//${window.location.hostname}:${port}`
})

const steps = computed<TourStep[]>(() => [
  {
    title: t('Welcome to the AI Gateway'),
    lead: t('Put one OpenAI-compatible endpoint in front of any LLM provider: route by model, spread traffic across a pool, fail over, and hand out keys per team. This walkthrough takes about a minute and moves through the pages with you.'),
    route: 'ai-endpoint-list',
    note: t('You can reopen this walkthrough any time from the Guide button above the endpoint list.'),
  },
  {
    title: t('Step 1 — Publish an AI endpoint'),
    lead: t('This button opens a wizard that creates everything at once: the upstream connection, the model pool, the public route, and the ai-proxy plugin.'),
    route: 'ai-endpoint-list',
    target: '[data-tour="create-endpoint"]',
    actions: [
      t('Name the endpoint and pick its public path, for example customer-support.'),
      t('Add one or more models — reuse a provider connection or create one inline with your API key.'),
      t('Set traffic weights when the pool has several models, then publish.'),
    ],
    codeTitle: l('Your endpoint goes live at', '发布后的接口地址'),
    code: `POST ${proxyBaseUrl.value}/ai/customer-support/v1/chat/completions`,
  },
  {
    title: t('Step 2 — Every endpoint is testable here'),
    lead: t('Published endpoints appear as cards with their live URL. Test opens a console that sends a real request through the proxy; Configure reopens the wizard.'),
    route: 'ai-endpoint-list',
    target: '[data-tour="endpoint-card"]',
    missingHint: t('No endpoint exists yet — publish one and this card is where it appears.'),
    note: t('The test console reports status, the model that actually served the request, and latency. Copy curl reproduces the same call in a terminal.'),
  },
  {
    title: t('Step 3 — Issue a virtual key'),
    lead: t('Callers authenticate with a gateway-issued key instead of your real provider credentials, and you can restrict which models each key may use.'),
    route: 'ai-virtual-key-list',
    target: '[data-tour="create-virtual-key"]',
    actions: [
      t('Create a key here — it is shown once, so copy it then.'),
      t('Optionally fill Allowed Models: gpt-4* covers the whole gpt-4 family; empty means no restriction.'),
      t('Back in the publish wizard, tick "Require a virtual key to call this endpoint" to enforce it.'),
    ],
    note: t('Requests without a valid key get 401; a model outside the allow list gets 403. Rotating or disabling a key takes effect immediately.'),
  },
  {
    title: t('Step 4 — Reuse connections and tune the pool'),
    lead: t('These tabs hold the building blocks the wizard creates for you, for when you need finer control.'),
    route: 'ai-provider-list',
    target: '[data-tour="ai-gateway-nav"]',
    actions: [
      t('Provider Connections — upstream services and their credentials, shared across endpoints.'),
      t('Advanced Models — models sharing a name form a group; weights split traffic and priority drives fallback.'),
      t('Virtual Keys — the keys you issue per team or application.'),
    ],
  },
  {
    title: t('Point your client at it'),
    lead: t('The gateway speaks the OpenAI Chat Completions protocol, so existing clients work unchanged — only the base URL changes.'),
    route: 'ai-endpoint-list',
    codeTitle: l('OpenAI SDK (Python)', 'OpenAI SDK（Python）'),
    code: `from openai import OpenAI

client = OpenAI(
    base_url="${proxyBaseUrl.value}/ai/customer-support/v1",
    api_key="sk-kr-...",   # ${l('the virtual key', '虚拟密钥')}
)

response = client.chat.completions.create(
    model="gpt-4o",
    messages=[{"role": "user", "content": "Hello!"}],
)`,
    note: t('A key can be sent as Authorization: Bearer, as x-api-key, or in the X-AI-Key header, so both the OpenAI and Anthropic SDK defaults are accepted.'),
  },
])

// The list is never empty, so clamping always yields a step — 列表非空，取值后必定有步骤
const step = computed<TourStep>(() => {
  const list = steps.value

  return list[Math.min(Math.max(stepIndex.value, 0), list.length - 1)] as TourStep
})
const isLast = computed(() => stepIndex.value === steps.value.length - 1)

const spotlightStyle = computed(() => {
  const rect = spotlight.value

  if (!rect) {
    return {}
  }

  return {
    height: `${rect.height}px`,
    left: `${rect.left}px`,
    top: `${rect.top}px`,
    width: `${rect.width}px`,
  }
})

/** Poll until the target renders after a route change — 路由切换后轮询等待目标元素渲染 */
const waitForTarget = (selector: string) => new Promise<HTMLElement | null>((resolve) => {
  const startedAt = performance.now()

  const attempt = () => {
    const found = document.querySelector<HTMLElement>(selector)

    if (found) {
      resolve(found)

      return
    }

    if (performance.now() - startedAt > TARGET_TIMEOUT_MS) {
      resolve(null)

      return
    }

    requestAnimationFrame(attempt)
  }

  attempt()
})

const clearAnchor = () => {
  spotlight.value = null
  popoverStyle.value = {}
}

const positionPopover = (rect: DOMRect) => {
  const card = popover.value

  if (!card) {
    return
  }

  const { height: cardHeight, width: cardWidth } = card.getBoundingClientRect()
  const spaceBelow = window.innerHeight - rect.bottom
  const below = spaceBelow > cardHeight + POPOVER_GAP + 16 || rect.top < cardHeight + POPOVER_GAP

  placement.value = below ? 'bottom' : 'top'

  const top = below
    ? rect.bottom + POPOVER_GAP
    : rect.top - cardHeight - POPOVER_GAP
  // Align to the element, then keep the card fully inside the viewport
  // 与元素对齐，同时确保气泡完整留在视口内
  const left = Math.min(
    Math.max(16, rect.left + rect.width / 2 - cardWidth / 2),
    Math.max(16, window.innerWidth - cardWidth - 16),
  )

  popoverStyle.value = {
    left: `${left}px`,
    top: `${Math.max(16, top)}px`,
  }
}

const measure = (element: HTMLElement) => {
  const rect = element.getBoundingClientRect()

  spotlight.value = {
    height: rect.height + SPOTLIGHT_PADDING * 2,
    left: rect.left - SPOTLIGHT_PADDING,
    top: rect.top - SPOTLIGHT_PADDING,
    width: rect.width + SPOTLIGHT_PADDING * 2,
  }

  positionPopover(rect)
}

let currentTarget: HTMLElement | null = null

/** Navigate if needed, then anchor onto this step's target — 按需跳转，然后锚定到该步骤的目标元素 */
const applyStep = async () => {
  const { route, target } = step.value

  targetMissing.value = false
  // Drop the previous highlight first, so a navigating step never leaves a ring
  // on the old element — 先清掉上一处高亮，避免跳转期间旧元素上残留高亮环
  currentTarget = null
  clearAnchor()

  if (route && router.currentRoute.value.name !== route) {
    await router.push({ name: route })
  }

  await nextTick()

  if (!target) {
    currentTarget = null
    clearAnchor()

    return
  }

  const element = await waitForTarget(target)

  if (!element) {
    currentTarget = null
    clearAnchor()
    targetMissing.value = true

    return
  }

  currentTarget = element
  element.scrollIntoView({ behavior: 'smooth', block: 'center' })
  // Let smooth scrolling settle before measuring — 等平滑滚动结束后再测量
  await new Promise((resolve) => setTimeout(resolve, 320))
  await nextTick()
  measure(element)
}

const reposition = () => {
  if (currentTarget) {
    measure(currentTarget)
  }
}

const goToStep = (index: number) => {
  goTo(index)
}

const next = () => {
  if (isLast.value) {
    finish()

    return
  }

  goTo(stepIndex.value + 1)
}

const back = () => {
  if (stepIndex.value > 0) {
    goTo(stepIndex.value - 1)
  }
}

const finish = () => {
  clearAnchor()
  currentTarget = null
  close()
}

const copy = async (code: string) => {
  await navigator.clipboard.writeText(code)
  toaster.open({
    appearance: 'success',
    message: l('Copied', '已复制'),
  })
}

const onKeydown = (event: KeyboardEvent) => {
  if (!active.value) {
    return
  }

  if (event.key === 'Escape') {
    finish()
  } else if (event.key === 'ArrowRight') {
    next()
  } else if (event.key === 'ArrowLeft') {
    back()
  }
}

watch([active, stepIndex], ([isActive]) => {
  if (isActive) {
    applyStep()
  } else {
    clearAnchor()
  }
})

onMounted(() => {
  window.addEventListener('resize', reposition)
  window.addEventListener('scroll', reposition, true)
  window.addEventListener('keydown', onKeydown)

  // Wait for the initial navigation to resolve before reading route meta —
  // the app mounts before the router settles on the landing route.
  // 等首次导航解析完成后再读取路由 meta —— App 的挂载早于路由确定落地页。
  router
    .isReady()
    .then(() => {
      // 仅在接口首页自动展示；直达模型、密钥或用量页时不得被引导劫持路由。
      if (router.currentRoute.value.name === 'ai-endpoint-list') {
        openIfFirstVisit()
      }
    })
    .catch(() => {
      // A failed initial navigation is the router's concern, not the tour's
      // 首次导航失败属于路由问题，引导不做处理
    })
})

onBeforeUnmount(() => {
  window.removeEventListener('resize', reposition)
  window.removeEventListener('scroll', reposition, true)
  window.removeEventListener('keydown', onKeydown)
})
</script>
