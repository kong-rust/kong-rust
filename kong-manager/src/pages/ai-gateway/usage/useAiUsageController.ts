import {
  computed,
  onScopeDispose,
  ref,
} from 'vue'
import {
  useRoute,
  useRouter,
  type LocationQuery,
  type LocationQueryRaw,
} from 'vue-router'
import {
  aiUsageApiFilterKeys,
  type AiUsageApiFilterKey,
  type AiUsageBreakdown,
  type AiUsageFact,
  type AiUsageFilters,
  type AiUsageMeta,
  type AiUsagePage,
  type AiUsageRangePreset,
  type AiUsageSummary,
  type AiUsageViewState,
} from './aiUsageTypes'
import { AiUsageRequestError, aiUsageService } from './services/aiUsageService'

const rangeDurations: Record<Exclude<AiUsageRangePreset, 'custom'>, number> = {
  '24h': 24 * 60 * 60 * 1000,
  '7d': 7 * 24 * 60 * 60 * 1000,
  '30d': 30 * 24 * 60 * 60 * 1000,
}

const getBrowserTimezone = () => {
  try {
    return Intl.DateTimeFormat().resolvedOptions().timeZone || 'UTC'
  } catch {
    return 'UTC'
  }
}

const defaultFilters = (): AiUsageFilters => ({
  range: '24h',
  start: '',
  end: '',
  timezone: getBrowserTimezone(),
  metric: 'cost',
  tokenMetric: 'total',
  request_id: '',
  route_id: '',
  service_id: '',
  provider_id: '',
  provider_type: '',
  requested_model: '',
  model_group: '',
  actual_model: '',
  virtual_key_id: '',
  consumer_id: '',
  status_code: '',
  outcome: '',
  stream: '',
  cache_status: '',
  usage_source: '',
  pricing_status: '',
  cost_status: '',
})

const firstQueryValue = (value: LocationQuery[string] | undefined) => {
  if (Array.isArray(value)) {
    return value[0] ?? ''
  }

  return value ?? ''
}

const isRange = (value: string): value is AiUsageRangePreset => (
  value === '24h' || value === '7d' || value === '30d' || value === 'custom'
)

const filtersFromQuery = (query: LocationQuery): AiUsageFilters => {
  const filters = defaultFilters()
  const range = firstQueryValue(query.range)
  const metric = firstQueryValue(query.metric)
  const tokenMetric = firstQueryValue(query.token_metric)

  filters.range = isRange(range) ? range : '24h'
  filters.start = firstQueryValue(query.start)
  filters.end = firstQueryValue(query.end)
  filters.timezone = firstQueryValue(query.timezone) || filters.timezone
  filters.metric = metric === 'tokens' ? 'tokens' : 'cost'
  filters.tokenMetric = (
    tokenMetric === 'prompt' || tokenMetric === 'completion' || tokenMetric === 'total'
  ) ? tokenMetric : 'total'

  for (const key of aiUsageApiFilterKeys) {
    filters[key] = firstQueryValue(query[key])
  }

  return filters
}

const filtersToQuery = (filters: AiUsageFilters): LocationQueryRaw => {
  const query: LocationQueryRaw = {
    range: filters.range,
    timezone: filters.timezone,
    metric: filters.metric,
    token_metric: filters.tokenMetric,
  }

  if (filters.range === 'custom') {
    query.start = filters.start
    query.end = filters.end
  }

  for (const key of aiUsageApiFilterKeys) {
    if (filters[key]) {
      query[key] = filters[key]
    }
  }

  return query
}

const canonicalQuerySignature = (query: LocationQuery | LocationQueryRaw) => JSON.stringify(
  Object.entries(query)
    .map(([key, value]) => [
      key,
      Array.isArray(value) ? value.map(entry => entry ?? '') : value ?? '',
    ])
    .sort(([left], [right]) => String(left).localeCompare(String(right))),
)

const timeBounds = (filters: AiUsageFilters) => {
  if (filters.range === 'custom') {
    return {
      start: filters.start,
      end: filters.end,
    }
  }

  const end = new Date()
  const start = new Date(end.getTime() - rangeDurations[filters.range])

  return {
    start: start.toISOString(),
    end: end.toISOString(),
  }
}

const dimensionApiParams = (filters: AiUsageFilters) => {
  const params: Record<string, string> = {}

  for (const key of aiUsageApiFilterKeys) {
    if (filters[key]) {
      params[key] = filters[key]
    }
  }

  return params
}

const commonApiParams = (filters: AiUsageFilters) => ({
  ...timeBounds(filters),
  ...dimensionApiParams(filters),
})

const hasDimensionFilters = (filters: AiUsageFilters) => (
  aiUsageApiFilterKeys.some(key => Boolean(filters[key]))
)

const stateForSummary = (
  summary: AiUsageSummary,
  filters: AiUsageFilters,
): AiUsageViewState => {
  if (summary.totals.requests > 0) {
    return 'ready'
  }

  return hasDimensionFilters(filters) ? 'empty-filter' : 'empty-window'
}

const defaultMeta = (): AiUsageMeta => ({
  mode: 'postgres',
  ephemeral: false,
  node_id: null,
  capacity: null,
  earliest_available_at: null,
  restart_clears: false,
})

export const useAiUsageController = () => {
  const route = useRoute()
  const router = useRouter()
  const filters = computed(() => filtersFromQuery(route.query))
  const state = ref<AiUsageViewState>('loading')
  const errorMessage = ref('')
  const summary = ref<AiUsageSummary | null>(null)
  const trend = ref<AiUsageBreakdown | null>(null)
  const modelRanking = ref<AiUsageBreakdown | null>(null)
  const virtualKeyRanking = ref<AiUsageBreakdown | null>(null)
  const meta = ref<AiUsageMeta>(defaultMeta())
  const snapshot = ref('')
  const logsPage = ref<AiUsagePage | null>(null)
  const logsLoading = ref(false)
  const selectedLog = ref<AiUsageFact | null>(null)
  const cursorStack = ref<Array<string | undefined>>([undefined])
  const cursorIndex = ref(0)
  let generation = 0
  let abortController: AbortController | null = null
  let lastQuerySignature = ''

  const isLogsRoute = computed(() => route.name === 'ai-usage-logs')
  const canPreviousLogs = computed(() => cursorIndex.value > 0)
  const canNextLogs = computed(() => Boolean(logsPage.value?.offset))
  const logsPageNumber = computed(() => cursorIndex.value + 1)
  const isLoading = computed(() => state.value === 'loading')

  const handleError = (error: unknown) => {
    if (error instanceof DOMException && error.name === 'AbortError') {
      return
    }

    const requestError = error instanceof AiUsageRequestError
      ? error
      : new AiUsageRequestError(
        error instanceof Error ? error.message : 'Unable to load AI usage',
        null,
        null,
      )

    errorMessage.value = requestError.message

    if (
      requestError.status === 501
      || requestError.errorCode === 'analytics_unsupported_in_hybrid'
    ) {
      state.value = 'unsupported'
    } else if (
      requestError.status === 409
      || requestError.errorCode === 'analytics_snapshot_expired'
    ) {
      state.value = 'snapshot-expired'
    } else {
      state.value = 'error'
    }
  }

  const resetLogs = () => {
    logsPage.value = null
    selectedLog.value = null
    cursorStack.value = [undefined]
    cursorIndex.value = 0
  }

  const loadLogsAt = async (
    offset: string | undefined,
    activeGeneration = generation,
    signal = abortController?.signal,
  ) => {
    if (!snapshot.value || !signal) {
      return
    }

    logsLoading.value = true

    try {
      const page = await aiUsageService.list({
        // snapshot 已绑定精确时间窗；避免相对时间范围在分页时重新取当前时间。
        ...dimensionApiParams(filters.value),
        snapshot: snapshot.value,
        size: 100,
        offset,
      }, signal)

      if (activeGeneration !== generation) {
        return
      }

      logsPage.value = page
      meta.value = page.meta
    } catch (error) {
      if (activeGeneration === generation) {
        handleError(error)
      }
    } finally {
      if (activeGeneration === generation) {
        logsLoading.value = false
      }
    }
  }

  const loadOverview = async () => {
    generation += 1
    const activeGeneration = generation

    abortController?.abort()
    abortController = new AbortController()
    state.value = 'loading'
    errorMessage.value = ''
    summary.value = null
    trend.value = null
    modelRanking.value = null
    virtualKeyRanking.value = null
    snapshot.value = ''
    resetLogs()

    try {
      const common = commonApiParams(filters.value)
      const totals = await aiUsageService.summary(common, abortController.signal)

      if (activeGeneration !== generation) {
        return
      }

      summary.value = totals
      snapshot.value = totals.snapshot
      meta.value = totals.meta

      if (totals.totals.requests === 0) {
        state.value = stateForSummary(totals, filters.value)
        if (isLogsRoute.value) {
          await loadLogsAt(undefined, activeGeneration, abortController.signal)
        }
        return
      }

      const trendBreakdown = filters.value.range === '30d'
        || (
          filters.value.range === 'custom'
          && new Date(filters.value.end).getTime() - new Date(filters.value.start).getTime()
            > rangeDurations['7d']
        )
        ? 'day'
        : 'hour'

      const [loadedTrend, loadedModels, loadedKeys] = await Promise.all([
        aiUsageService.summary({
          ...common,
          snapshot: totals.snapshot,
          breakdown: trendBreakdown,
          timezone: filters.value.timezone,
        }, abortController.signal),
        aiUsageService.summary({
          ...common,
          snapshot: totals.snapshot,
          breakdown: 'actual_model',
          order_by: filters.value.metric === 'tokens' ? 'total_tokens' : 'cost_usd',
          limit: 10,
        }, abortController.signal),
        aiUsageService.summary({
          ...common,
          snapshot: totals.snapshot,
          breakdown: 'virtual_key',
          order_by: filters.value.metric === 'tokens' ? 'total_tokens' : 'cost_usd',
          limit: 10,
        }, abortController.signal),
      ])

      if (activeGeneration !== generation) {
        return
      }

      trend.value = loadedTrend.breakdown
      modelRanking.value = loadedModels.breakdown
      virtualKeyRanking.value = loadedKeys.breakdown
      state.value = 'ready'

      if (isLogsRoute.value) {
        await loadLogsAt(undefined, activeGeneration, abortController.signal)
      }
    } catch (error) {
      if (activeGeneration === generation) {
        handleError(error)
      }
    }
  }

  const nextLogs = async () => {
    const nextOffset = logsPage.value?.offset ?? undefined
    if (!nextOffset) {
      return
    }

    cursorStack.value = cursorStack.value.slice(0, cursorIndex.value + 1)
    cursorStack.value.push(nextOffset)
    cursorIndex.value += 1
    await loadLogsAt(nextOffset)
  }

  const previousLogs = async () => {
    if (cursorIndex.value === 0) {
      return
    }

    cursorIndex.value -= 1
    await loadLogsAt(cursorStack.value[cursorIndex.value])
  }

  const applyFilters = async (nextFilters: AiUsageFilters) => {
    await router.push({
      name: route.name ?? 'ai-usage-overview',
      query: filtersToQuery(nextFilters),
    })
  }

  const drillDown = async (values: Partial<Record<AiUsageApiFilterKey, string>>) => {
    const nextFilters = {
      ...filters.value,
      ...values,
    }

    await router.push({
      name: 'ai-usage-overview',
      query: filtersToQuery(nextFilters),
    })
  }

  const retry = () => loadOverview()

  const syncWithRoute = () => {
    const normalizedQuery = filtersToQuery(filters.value)
    const querySignature = canonicalQuerySignature(route.query)

    if (querySignature !== canonicalQuerySignature(normalizedQuery)) {
      void router.replace({
        name: route.name ?? 'ai-usage-overview',
        query: normalizedQuery,
      })
      return
    }

    const queryChanged = querySignature !== lastQuerySignature
    lastQuerySignature = querySignature

    if (queryChanged || !snapshot.value) {
      void loadOverview()
    } else if (isLogsRoute.value && !logsPage.value) {
      void loadLogsAt(cursorStack.value[cursorIndex.value])
    } else if (route.name === 'ai-usage-overview' && summary.value) {
      // 日志查询错误不应污染已经成功加载的统计页。
      state.value = stateForSummary(summary.value, filters.value)
      errorMessage.value = ''
    }
  }

  const removeRouteHook = router.afterEach(syncWithRoute)
  syncWithRoute()

  onScopeDispose(() => {
    generation += 1
    abortController?.abort()
    removeRouteHook()
  })

  return {
    applyFilters,
    canNextLogs,
    canPreviousLogs,
    drillDown,
    errorMessage,
    filters,
    isLoading,
    isLogsRoute,
    loadOverview,
    logsLoading,
    logsPage,
    logsPageNumber,
    meta,
    modelRanking,
    nextLogs,
    previousLogs,
    retry,
    selectedLog,
    snapshot,
    state,
    summary,
    trend,
    virtualKeyRanking,
  }
}

export type AiUsageController = ReturnType<typeof useAiUsageController>
