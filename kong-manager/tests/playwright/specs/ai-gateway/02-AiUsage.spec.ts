import {
  expect,
  type Page,
  type Route,
} from '@playwright/test'
import baseTest from '@pw/base-test'

const test = baseTest()
const snapshot = 'snapshot-one-consistent-watermark'

const postgresMeta = {
  mode: 'postgres',
  ephemeral: false,
  node_id: null,
  capacity: null,
  earliest_available_at: null,
  restart_clears: false,
}

const aggregateMetrics = (requests = 3) => ({
  requests,
  successful_requests: requests,
  failed_requests: 0,
  outcomes: {
    success: requests,
    gateway_rejected: 0,
    gateway_error: 0,
    upstream_error: 0,
    client_disconnected: 0,
    stream_interrupted: 0,
  },
  prompt_tokens: {
    known_sum: requests ? '1000' : '0',
    known_requests: requests,
    unknown_requests: 0,
    coverage: requests ? '1.000000' : null,
  },
  completion_tokens: {
    known_sum: requests ? '200' : '0',
    known_requests: requests,
    unknown_requests: 0,
    coverage: requests ? '1.000000' : null,
  },
  total_tokens: {
    known_sum: requests ? '1200' : '0',
    known_requests: requests,
    unknown_requests: 0,
    coverage: requests ? '1.000000' : null,
  },
  cost_usd_calculable_sum: requests ? '0.120000000000' : '0.000000000000',
  pricing_status: {
    matched: requests,
    unmatched: 0,
    unsupported: 0,
    not_applicable: 0,
  },
  cost_status: {
    calculated: requests,
    estimated: 0,
    not_incurred: 0,
    unavailable: 0,
  },
  estimated_usage_ratio: requests ? '0.000000' : null,
  pricing_coverage: requests ? '1.000000' : null,
  cost_calculable_coverage: requests ? '1.000000' : null,
  avg_e2e_ms: requests ? '123.500' : null,
  p95_e2e_ms: requests ? '180.000' : null,
  avg_ttft_ms: requests ? '24.000' : null,
  cache_hits: 0,
})

const timeItem = (key: string, label: string, cost: string, tokens: string) => {
  const metrics = aggregateMetrics(1)
  metrics.cost_usd_calculable_sum = cost
  metrics.total_tokens.known_sum = tokens

  return {
    key,
    label,
    is_other: false,
    bucket_start: key,
    bucket_end: '2026-07-26T09:00:00Z',
    dimension: null,
    metrics,
  }
}

const categoryItem = (
  key: string,
  label: string,
  dimension: {
    id: string | null
    name: string | null
    type: string | null
    prefix: string | null
  },
) => ({
  key,
  label,
  is_other: false,
  bucket_start: null,
  bucket_end: null,
  dimension,
  metrics: aggregateMetrics(2),
})

const summaryResponse = (
  breakdown: string | null,
  meta: typeof postgresMeta,
  requests: number,
) => {
  let breakdownBody = null

  if (breakdown === 'hour' || breakdown === 'day') {
    breakdownBody = {
      type: breakdown,
      timezone: 'UTC',
      order_by: null,
      limit: null,
      items: [
        timeItem('2026-07-26T08:00:00Z', '2026-07-26 08:00 +00:00', '0.040000000000', '400'),
        timeItem('2026-07-26T09:00:00Z', '2026-07-26 09:00 +00:00', '0.080000000000', '800'),
      ],
      other: null,
    }
  } else if (breakdown === 'actual_model') {
    breakdownBody = {
      type: breakdown,
      timezone: null,
      order_by: 'cost_usd',
      limit: 10,
      items: [categoryItem(
        'snapshot:model',
        'gpt-5.6-sol',
        {
          id: null,
          name: 'gpt-5.6-sol',
          type: 'openai',
          prefix: null,
        },
      )],
      other: null,
    }
  } else if (breakdown === 'virtual_key') {
    breakdownBody = {
      type: breakdown,
      timezone: null,
      order_by: 'cost_usd',
      limit: 10,
      items: [categoryItem(
        'id:44444444-4444-4444-8444-444444444444',
        'team-a',
        {
          id: '44444444-4444-4444-8444-444444444444',
          name: 'team-a',
          type: null,
          prefix: 'kr_team',
        },
      )],
      other: null,
    }
  }

  return {
    snapshot,
    meta,
    totals: aggregateMetrics(requests),
    breakdown: breakdownBody,
  }
}

const facts = [{
  id: 'aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa',
  request_id: '0123456789abcdef0123456789abcdef',
  started_at: '2026-07-26T08:00:00.123Z',
  finished_at: '2026-07-26T08:00:01.234Z',
  gateway: {
    route: {
      id: '11111111-1111-4111-8111-111111111111',
      name: 'chat-route',
    },
    service: {
      id: '22222222-2222-4222-8222-222222222222',
      name: 'chat-service',
    },
  },
  ai: {
    provider: {
      id: '33333333-3333-4333-8333-333333333333',
      name: 'prod-openai',
      type: 'openai',
    },
    model: {
      id: '55555555-5555-4555-8555-555555555555',
      requested: 'chat',
      group: 'chat',
      actual: 'gpt-5.6-sol',
    },
    attempt_count: 1,
  },
  identity: {
    virtual_key: {
      id: '44444444-4444-4444-8444-444444444444',
      name: 'team-a',
      prefix: 'kr_team',
    },
    consumer_id: '66666666-6666-4666-8666-666666666666',
  },
  usage: {
    prompt_tokens: 100,
    completion_tokens: 20,
    total_tokens: 120,
    prompt_source: 'provider',
    completion_source: 'provider',
    total_source: 'provider',
    reasoning_tokens: 5,
    cache_read_input_tokens: null,
    cache_write_input_tokens: null,
    source: 'provider',
    unavailable_reasons: [],
  },
  pricing: {
    status: 'matched',
    currency: 'USD',
    input: {
      usd_per_million: '5.000000000000',
      source: 'builtin',
      version: '2026-07-26.1',
      snapshot_date: '2026-07-26',
      effective_from: '2026-07-26T00:00:00Z',
      effective_to: null,
    },
    output: {
      usd_per_million: '30.000000000000',
      source: 'override',
      version: 'model:555:output',
      snapshot_date: '2026-07-26',
      effective_from: '2026-07-26T00:00:00Z',
      effective_to: null,
    },
    unsupported_reasons: [],
  },
  cost: {
    usd: '0.001100000000',
    status: 'calculated',
    unavailable_reasons: [],
  },
  result: {
    status_code: 200,
    upstream_status_code: 200,
    outcome: 'success',
    e2e_ms: 1111,
    ttft_ms: 25,
    upstream_attempted: true,
    stream: false,
    cache_status: 'not_configured',
  },
}, {
  id: 'bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb',
  request_id: 'fedcba9876543210fedcba9876543210',
  started_at: '2026-07-26T07:00:00.123Z',
  finished_at: '2026-07-26T07:00:02.234Z',
  gateway: {
    route: null,
    service: null,
  },
  ai: {
    provider: null,
    model: null,
    attempt_count: 1,
  },
  identity: {
    virtual_key: null,
    consumer_id: null,
  },
  usage: {
    prompt_tokens: null,
    completion_tokens: null,
    total_tokens: null,
    prompt_source: null,
    completion_source: null,
    total_source: null,
    reasoning_tokens: null,
    cache_read_input_tokens: null,
    cache_write_input_tokens: null,
    source: 'unavailable',
    unavailable_reasons: ['missing_prompt_usage', 'missing_completion_usage'],
  },
  pricing: {
    status: 'unmatched',
    currency: 'USD',
    input: null,
    output: null,
    unsupported_reasons: [],
  },
  cost: {
    usd: null,
    status: 'unavailable',
    unavailable_reasons: ['unmatched_input_price', 'unmatched_output_price'],
  },
  result: {
    status_code: 502,
    upstream_status_code: 500,
    outcome: 'upstream_error',
    e2e_ms: 2111,
    ttft_ms: null,
    upstream_attempted: true,
    stream: true,
    cache_status: 'bypass',
  },
}]

interface MockOptions {
  meta?: typeof postgresMeta
  requests?: number
  failFirstSummary?: boolean
  summaryError?: {
    status: number
    errorCode: string
    message: string
  }
  listError?: {
    status: number
    errorCode: string
    message: string
  }
}

const fulfillJson = async (route: Route, status: number, body: unknown) => {
  await route.fulfill({
    status,
    contentType: 'application/json',
    body: JSON.stringify(body),
  })
}

const installUsageMock = async (page: Page, options: MockOptions = {}) => {
  const calls: URL[] = []
  let firstSummaryFailed = false

  await page.route('**/ai-usage**', async route => {
    const url = new URL(route.request().url())
    calls.push(url)

    if (url.pathname.endsWith('/ai-usage/summary')) {
      const breakdown = url.searchParams.get('breakdown')

      if (options.summaryError) {
        await fulfillJson(route, options.summaryError.status, {
          message: options.summaryError.message,
          error_code: options.summaryError.errorCode,
        })
        return
      }

      if (options.failFirstSummary && !breakdown && !firstSummaryFailed) {
        firstSummaryFailed = true
        await fulfillJson(route, 503, {
          message: 'Analytics query timed out',
          error_code: 'analytics_query_timeout',
        })
        return
      }

      await fulfillJson(
        route,
        200,
        summaryResponse(
          breakdown,
          options.meta ?? postgresMeta,
          options.requests ?? 3,
        ),
      )
      return
    }

    if (options.listError) {
      await fulfillJson(route, options.listError.status, {
        message: options.listError.message,
        error_code: options.listError.errorCode,
      })
      return
    }

    const secondPage = url.searchParams.get('offset') === 'cursor-one'
    await fulfillJson(route, 200, {
      data: [secondPage ? facts[1] : facts[0]],
      offset: secondPage ? null : 'cursor-one',
      next: secondPage ? null : '/ai-usage?offset=cursor-one',
      snapshot,
      meta: options.meta ?? postgresMeta,
    })
  })

  return calls
}

test.beforeEach(async ({ page }) => {
  await page.unroute('**/ai-usage**')
  await page.addInitScript(() => {
    window.localStorage.setItem('kong-rust:ai-gateway-tour', '2')
  })
})

test('keeps first-time visitors on a directly opened usage page', async ({ page }) => {
  await page.addInitScript(() => {
    window.localStorage.removeItem('kong-rust:ai-gateway-tour')
  })
  await installUsageMock(page)

  await page.goto('/ai-gateway/usage?range=24h&timezone=UTC')
  await expect(page.getByRole('heading', { name: 'AI Usage' })).toBeVisible()
  await expect(page).toHaveURL(/\/ai-gateway\/usage/)
  await expect(page.getByRole('dialog', { name: 'Welcome to the AI Gateway' })).toHaveCount(0)
})

test('keeps all filters in the URL and reuses one snapshot for trends and rankings', async ({
  page,
}) => {
  const calls = await installUsageMock(page)
  const filterValues = {
    request_id: '0123456789abcdef0123456789abcdef',
    route_id: '11111111-1111-4111-8111-111111111111',
    service_id: '22222222-2222-4222-8222-222222222222',
    provider_id: '33333333-3333-4333-8333-333333333333',
    provider_type: 'openai',
    requested_model: 'chat',
    model_group: 'chat',
    actual_model: 'gpt-5.6-sol',
    virtual_key_id: '44444444-4444-4444-8444-444444444444',
    consumer_id: '66666666-6666-4666-8666-666666666666',
    status_code: '200',
    outcome: 'success',
    stream: 'true',
    cache_status: 'hit',
    usage_source: 'provider',
    pricing_status: 'matched',
    cost_status: 'calculated',
  }
  const query = new URLSearchParams({
    range: '7d',
    timezone: 'UTC',
    ...filterValues,
  })

  await page.goto(`/ai-gateway/usage?${query}`)
  await expect(page.getByRole('heading', { name: 'AI Usage' })).toBeVisible()
  const costKpi = page.locator('.ai-usage-kpi').filter({ hasText: 'Calculable cost subtotal' })
  const totalTokenKpi = page.locator('.ai-usage-kpi').filter({ hasText: 'Known total tokens' })
  await expect(costKpi).toContainText('$0.12')
  await expect(totalTokenKpi).toContainText('1,200')

  await expect.poll(() => calls.length).toBeGreaterThanOrEqual(4)
  const initialCalls = calls.slice(0, 4)
  const totalsCall = initialCalls.find(url => (
    url.pathname.endsWith('/ai-usage/summary')
    && !url.searchParams.has('breakdown')
  ))

  expect(totalsCall).toBeTruthy()
  for (const [key, value] of Object.entries(filterValues)) {
    expect(totalsCall?.searchParams.get(key)).toBe(value)
  }
  expect(totalsCall?.searchParams.get('start')).toBeTruthy()
  expect(totalsCall?.searchParams.get('end')).toBeTruthy()

  const secondaryCalls = initialCalls.filter(url => url.searchParams.has('breakdown'))
  expect(secondaryCalls).toHaveLength(3)
  expect(secondaryCalls.map(url => url.searchParams.get('breakdown')).sort())
    .toEqual(['actual_model', 'hour', 'virtual_key'])
  for (const url of secondaryCalls) {
    expect(url.searchParams.get('snapshot')).toBe(snapshot)
  }

  const dataPoint = page.locator('.ai-usage-chart-point').first()
  await dataPoint.focus()
  await expect(page.locator('.ai-usage-chart-tooltip')).toBeVisible()

  await page.getByRole('button', { name: 'Tokens', exact: true }).click()
  await expect(page).toHaveURL(/metric=tokens/)
  await page.getByLabel('Token metric').selectOption('prompt')
  await expect(page).toHaveURL(/token_metric=prompt/)

  await page.getByRole('button', { name: 'Clear filters' }).click()
  await expect(page).not.toHaveURL(/actual_model=/)
  const modelRanking = page.locator('.ai-usage-ranking').filter({ hasText: 'Top actual models' })
  await modelRanking.getByRole('button', { name: 'gpt-5.6-sol' }).click()
  await expect(page).toHaveURL(/actual_model=gpt-5.6-sol/)
  expect(new URL(page.url()).searchParams.get('provider_type')).toBe('openai')

  await page.getByLabel('Request ID', { exact: true })
    .fill('abcdefabcdefabcdefabcdefabcdefab')
  await page.getByRole('button', { name: 'Apply filters' }).click()
  await expect(page).toHaveURL(/request_id=abcdefabcdefabcdefabcdefabcdefab/)
})

test('shows complete request facts and keeps cursor pagination stable', async ({ page }) => {
  const calls = await installUsageMock(page)

  await page.goto('/ai-gateway/usage/logs?range=24h&timezone=UTC')
  await expect(page.getByRole('heading', { name: 'Request facts' })).toBeVisible()
  await expect(page.getByText(facts[0].request_id, { exact: true })).toBeVisible()
  await expect(page.getByText('Input $5 · Built-in price')).toBeVisible()
  await expect(page.getByText('Output $30 · Custom override')).toBeVisible()

  await page.getByRole('button', { name: 'Details' }).click()
  const detail = page.getByRole('dialog', { name: facts[0].request_id })
  await expect(detail).toBeVisible()
  await expect(detail).toContainText('55555555-5555-4555-8555-555555555555')
  await expect(detail).toContainText('kr_team')
  await expect(detail).toContainText('2026-07-26.1')
  await expect(detail).toContainText('model:555:output')
  await expect(detail).toContainText('Provider reported · provider')
  await detail.getByRole('button', { name: 'Close' }).click()

  await page.getByRole('button', { name: 'Next' }).click()
  await expect(page.getByText('Page 2')).toBeVisible()
  await expect(page.getByText(facts[1].request_id, { exact: true })).toBeVisible()
  await expect(page.getByText('—', { exact: true }).first()).toBeVisible()

  await page.getByRole('button', { name: 'Previous' }).click()
  await expect(page.getByText('Page 1')).toBeVisible()
  await expect(page.getByText(facts[0].request_id, { exact: true })).toBeVisible()

  const listCalls = calls.filter(url => url.pathname.endsWith('/ai-usage'))
  expect(listCalls.map(url => url.searchParams.get('offset')))
    .toEqual([null, 'cursor-one', null])
  for (const url of listCalls) {
    expect(url.searchParams.get('snapshot')).toBe(snapshot)
    expect(url.searchParams.has('start')).toBe(false)
    expect(url.searchParams.has('end')).toBe(false)
  }
  expect(new URL(page.url()).searchParams.has('snapshot')).toBe(false)
})

test('restores the existing overview after a request-log query fails', async ({ page }) => {
  const calls = await installUsageMock(page, {
    listError: {
      status: 503,
      errorCode: 'analytics_query_unavailable',
      message: 'Analytics store is temporarily unavailable',
    },
  })

  await page.goto('/ai-gateway/usage?range=24h&timezone=UTC')
  const costKpi = page.locator('.ai-usage-kpi').filter({
    hasText: 'Calculable cost subtotal',
  })
  await expect(costKpi).toBeVisible()

  const usageNavigation = page.getByRole('navigation', { name: 'Usage views' })
  await usageNavigation.getByRole('link', { name: 'Request logs' }).click()
  await expect(page.getByRole('heading', { name: 'Usage could not be loaded' })).toBeVisible()

  const listCall = calls.find(url => url.pathname.endsWith('/ai-usage'))
  expect(listCall?.searchParams.get('snapshot')).toBe(snapshot)
  expect(listCall?.searchParams.has('start')).toBe(false)
  expect(listCall?.searchParams.has('end')).toBe(false)

  await usageNavigation.getByRole('link', { name: 'Overview', exact: true }).click()
  await expect(costKpi).toBeVisible()
  await expect(page.getByRole('heading', { name: 'Usage could not be loaded' })).toHaveCount(0)
})

test('distinguishes retryable, Hybrid, DB-less, and expired-snapshot states', async ({
  page,
}) => {
  await installUsageMock(page, { failFirstSummary: true })
  await page.goto('/ai-gateway/usage?range=24h&timezone=UTC')
  await expect(page.getByRole('heading', { name: 'Usage could not be loaded' })).toBeVisible()
  await page.getByRole('button', { name: 'Try again' }).click()
  await expect(page.locator('.ai-usage-kpi').filter({
    hasText: 'Calculable cost subtotal',
  })).toBeVisible()

  await page.unroute('**/ai-usage**')
  await installUsageMock(page, {
    summaryError: {
      status: 501,
      errorCode: 'analytics_unsupported_in_hybrid',
      message: 'Analytics is unsupported in Hybrid mode',
    },
  })
  await page.reload()
  await expect(page.getByRole('heading', {
    name: 'Usage analytics is unavailable in Hybrid mode',
  })).toBeVisible()

  await page.unroute('**/ai-usage**')
  await installUsageMock(page, {
    meta: {
      mode: 'dbless',
      ephemeral: true,
      node_id: 'node-a',
      capacity: 10000,
      earliest_available_at: '2026-07-26T07:00:00Z',
      restart_clears: true,
    },
  })
  await page.reload()
  await expect(page.getByText('DB-less node-local analytics')).toBeVisible()
  await expect(page.getByText(/Node node-a; capacity 10000/)).toBeVisible()

  await page.unroute('**/ai-usage**')
  await installUsageMock(page, {
    listError: {
      status: 409,
      errorCode: 'analytics_snapshot_expired',
      message: 'Analytics snapshot has expired',
    },
  })
  await page.goto('/ai-gateway/usage/logs?range=24h&timezone=UTC')
  await expect(page.getByRole('heading', {
    name: 'The data window has rolled forward',
  })).toBeVisible()
  await expect(page.getByRole('button', { name: 'Refresh snapshot' })).toBeVisible()
})

test('distinguishes an empty time window from filters with no matches', async ({ page }) => {
  await installUsageMock(page, { requests: 0 })

  await page.goto('/ai-gateway/usage?range=24h&timezone=UTC')
  await expect(page.getByRole('heading', {
    name: 'No AI calls in this time window',
  })).toBeVisible()

  await page.getByLabel('Provider type').fill('openai')
  await page.getByRole('button', { name: 'Apply filters' }).click()
  await expect(page.getByRole('heading', {
    name: 'No calls match these filters',
  })).toBeVisible()
})

test('supports Simplified Chinese and keeps narrow layouts usable', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 })
  await installUsageMock(page)

  await page.goto('/ai-gateway/usage?range=24h&timezone=Asia%2FShanghai')
  await page.getByLabel('Language').selectOption('zh-CN')
  await expect(page.getByRole('heading', { name: '调用统计' })).toBeVisible()
  await expect(page.getByText('可计算成本小计').first()).toBeVisible()
  await expect(page.getByRole('link', { name: '调用日志' })).toBeVisible()

  const filterColumns = await page.locator('.ai-usage-filter-grid').first().evaluate(element => (
    window.getComputedStyle(element).gridTemplateColumns
  ))
  expect(filterColumns.trim().split(/\s+/)).toHaveLength(1)
  expect(await page.evaluate(() => document.documentElement.scrollWidth))
    .toBeLessThanOrEqual(390)

  await page.getByRole('link', { name: '调用日志' }).click()
  await expect(page.getByRole('heading', { name: '请求事实' })).toBeVisible()
  await page.getByRole('button', { name: '详情' }).click()
  const detail = page.getByRole('dialog')
  const detailBox = await detail.boundingBox()
  expect(detailBox?.width).toBeLessThanOrEqual(390)
  await expect(detail.getByText('定价与成本')).toBeVisible()
})
