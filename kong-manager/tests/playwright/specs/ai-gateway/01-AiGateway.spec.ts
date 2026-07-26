import { expect, type APIRequestContext, type APIResponse } from '@playwright/test'
import baseTest from '@pw/base-test'
import axios from 'axios'
import { createServer, type Server, type ServerResponse } from 'node:http'
import type { AddressInfo } from 'node:net'

const test = baseTest()
const suffix = `${Date.now()}`
const providerName = `pw-ai-provider-${suffix}`
const updatedProviderName = `${providerName}-updated`
const endpointName = `Endpoint ${suffix}`
const updatedEndpointName = `${endpointName} Updated`
const endpointSlug = `endpoint-${suffix}`
const updatedEndpointSlug = `${endpointSlug}-updated`
const advancedModelGroup = `pw-ai-model-${suffix}`
const virtualKeyName = `pw-ai-key-${suffix}`
let mockUpstream: Server
let mockUpstreamEndpoint = ''
let lastUpstreamModel = ''
let streamingResponse: ServerResponse | null = null
let streamingUpstreamClosed = false
let streamingRequestStarted: Promise<void>
let resolveStreamingRequestStarted: () => void
let endpointOwnershipTag = ''

interface KongEntity {
  id: string
  name?: string
  enabled?: boolean
  provider_id?: string
  route?: { id: string }
  response_buffering?: boolean
  tags?: string[]
  config?: {
    model?: string
    model_group?: string
  }
}

const apiUrl = () => {
  const url = process.env.KM_TEST_API_URL

  if (!url) {
    throw new Error('KM_TEST_API_URL is required for AI Gateway end-to-end tests')
  }

  return url.replace(/\/+$/, '')
}

const listEntities = async (endpoint: string) => {
  const { data } = await axios.get<{ data: KongEntity[] }>(`${apiUrl()}/${endpoint}`, {
    params: { size: 1000 },
  })

  return data.data
}

const deleteMatching = async (
  endpoint: string,
  matches: (entity: KongEntity) => boolean,
) => {
  for (const entity of (await listEntities(endpoint)).filter(matches)) {
    await axios.delete(`${apiUrl()}/${endpoint}/${entity.id}`)
  }
}

const cleanup = async () => {
  const providers = await listEntities('ai-providers')
  const providerIds = providers
    .filter(provider => [providerName, updatedProviderName].includes(provider.name ?? ''))
    .map(provider => provider.id)

  await deleteMatching(
    'plugins',
    entity => [endpointSlug, updatedEndpointSlug]
      .some(slug => entity.tags?.includes('kr-ai-endpoint-v1')
        && entity.config?.model_group?.includes(slug)),
  )
  await deleteMatching(
    'routes',
    entity => [endpointSlug, updatedEndpointSlug].some(slug => entity.name === `ai-${slug}`),
  )
  await deleteMatching(
    'services',
    entity => [endpointSlug, updatedEndpointSlug].some(slug => entity.name === `ai-${slug}`),
  )
  await deleteMatching('ai-virtual-keys', entity => entity.name === virtualKeyName)
  await deleteMatching(
    'ai-models',
    entity => providerIds.includes(entity.provider_id ?? '') || entity.name === advancedModelGroup,
  )
  await deleteMatching(
    'ai-providers',
    entity => [providerName, updatedProviderName].includes(entity.name ?? ''),
  )
}

const waitForProxy = async (
  request: APIRequestContext,
  url: string,
  data: Record<string, unknown>,
  expectedModel: string,
) => {
  let response: APIResponse | undefined

  await expect.poll(async () => {
    response = await request.post(url, { data })

    return `${response.status()}:${response.headers()['x-kong-llm-model'] ?? ''}`
  }, {
    timeout: 5_000,
  }).toBe(`200:${expectedModel}`)

  if (!response) {
    throw new Error(`Proxy ${url} did not return a response`)
  }

  return response
}

test.describe('AI Gateway manager', () => {
  test.beforeEach(async ({ page }) => {
    // CRUD 用例聚焦业务流程；首次访问引导有独立交互，不应遮挡测试目标。
    await page.addInitScript(() => {
      window.localStorage.setItem('kong-rust:ai-gateway-tour', '2')
    })
  })

  test.beforeAll(async ({ page }) => {
    page.on('dialog', dialog => dialog.accept())
    await cleanup()
    streamingRequestStarted = new Promise(resolve => {
      resolveStreamingRequestStarted = resolve
    })
    mockUpstream = createServer((request, response) => {
      const chunks: string[] = []
      request.setEncoding('utf8')
      request.on('data', chunk => chunks.push(chunk))
      request.on('end', () => {
        const body = JSON.parse(chunks.join('')) as { model?: string, stream?: boolean }
        lastUpstreamModel = body.model ?? ''

        if (body.stream) {
          streamingResponse = response
          response.on('finish', () => {
            streamingUpstreamClosed = true
          })
          response.writeHead(200, {
            'Content-Type': 'text/event-stream',
            'Cache-Control': 'no-cache',
          })
          response.flushHeaders()
          response.write(
            'data: {"id":"chatcmpl-stream","object":"chat.completion.chunk",'
            + '"created":1,"model":"pw-model-v2","choices":[{"index":0,'
            + '"delta":{"content":"first"},"finish_reason":null}]}\n\n',
          )
          resolveStreamingRequestStarted()
          return
        }

        response.writeHead(200, { 'Content-Type': 'application/json' })
        response.end(JSON.stringify({
          id: 'chatcmpl-test',
          object: 'chat.completion',
          created: 0,
          model: lastUpstreamModel,
          choices: [{
            index: 0,
            message: { role: 'assistant', content: 'mock upstream ok' },
            finish_reason: 'stop',
          }],
          usage: { prompt_tokens: 1, completion_tokens: 1, total_tokens: 2 },
        }))
      })
    })
    await new Promise<void>((resolve, reject) => {
      mockUpstream.once('error', reject)
      mockUpstream.listen(0, '127.0.0.1', resolve)
    })
    const address = mockUpstream.address() as AddressInfo
    mockUpstreamEndpoint = `http://127.0.0.1:${address.port}/v1/chat/completions`
  })

  test.afterAll(async () => {
    try {
      await cleanup()
    } finally {
      await new Promise<void>((resolve, reject) => {
        mockUpstream.close(error => error ? reject(error) : resolve())
      })
    }
  })

  test('creates, reads, updates, calls, and deletes an AI endpoint', async ({ page }) => {
    await page.goto('/ai-gateway')
    await expect(page.getByRole('button', { name: 'Create Endpoint', exact: true })).toBeVisible()
    await page.getByRole('button', { name: 'Create Endpoint', exact: true }).click()
    await page.getByLabel('Endpoint name').fill(endpointName)
    await page.getByLabel('Path name').fill(endpointSlug)
    await page.getByLabel('Provider connection').selectOption('new')
    await page.getByLabel('Connection name').fill(providerName)
    await page.getByLabel('Provider', { exact: true }).selectOption('openai_compat')
    await page.getByLabel('Service URL').fill(mockUpstreamEndpoint)
    await page.getByLabel('API key (optional)').fill('test-only-placeholder')
    await page.getByLabel('Model name').fill('pw-model')
    await expect(page.getByText('Config JSON')).toHaveCount(0)
    await page.getByRole('button', { name: 'Publish Endpoint' }).click()

    const endpointCard = page.getByRole('article').filter({ hasText: endpointName })
    await expect(endpointCard).toContainText('Running')
    await expect(endpointCard).toContainText('pw-model')
    await expect(endpointCard).toContainText('100%')

    const createdRoute = (await listEntities('routes'))
      .find(route => route.name === `ai-${endpointSlug}`)
    const createdService = (await listEntities('services'))
      .find(service => service.name === `ai-${endpointSlug}`)
    const createdPlugin = (await listEntities('plugins'))
      .find(plugin => plugin.route?.id === createdRoute?.id)
    endpointOwnershipTag = createdService?.tags
      ?.find(tag => tag.startsWith('kr-ai-endpoint:')) ?? ''

    expect(createdService?.enabled).toBe(true)
    expect(createdRoute?.response_buffering).toBe(false)
    expect(createdPlugin?.enabled).toBe(true)
    expect(endpointOwnershipTag).not.toBe('')

    const endpointUrl = `http://127.0.0.1:8000/ai/${endpointSlug}/v1/chat/completions`
    const proxyResponse = await waitForProxy(page.request, endpointUrl, {
      model: 'ignored-by-config-model-source',
      messages: [{ role: 'user', content: 'hello' }],
    }, 'pw-model')
    expect(proxyResponse.ok()).toBe(true)
    expect(proxyResponse.headers()['content-type']).toContain('application/json')
    expect(proxyResponse.headers()['x-kong-llm-model']).toBe('pw-model')
    expect((await proxyResponse.json()).choices[0].message.content).toBe('mock upstream ok')
    expect(lastUpstreamModel).toBe('pw-model')

    await endpointCard.getByRole('button', { name: 'Test', exact: true }).click()
    const playground = page.locator('.ai-endpoint-playground')
    await playground.getByLabel('Message').fill('hello from the manager')
    await playground.getByRole('button', { name: 'Send test request' }).click()
    await expect(playground.locator('.ai-endpoint-test-metrics')).toContainText('200')
    await expect(playground.locator('pre')).toContainText('mock upstream ok')

    await endpointCard.getByRole('button', { name: 'Configure' }).click()
    await page.getByLabel('Endpoint name').fill(updatedEndpointName)
    await page.getByLabel('Path name').fill(updatedEndpointSlug)
    await page.getByLabel('Model name').fill('pw-model-v2')
    await page.getByRole('button', { name: 'Save changes' }).click()

    const updatedCard = page.getByRole('article').filter({ hasText: updatedEndpointName })
    await expect(updatedCard).toContainText('pw-model-v2')
    await expect(updatedCard).toContainText(`/ai/${updatedEndpointSlug}/v1/chat/completions`)

    await expect.poll(async () => {
      const oldPathResponse = await page.request.post(endpointUrl, {
        data: { messages: [] },
      })

      return oldPathResponse.status()
    }, {
      timeout: 5_000,
    }).toBe(404)

    const updatedEndpointUrl = `http://127.0.0.1:8000/ai/${updatedEndpointSlug}/v1/chat/completions`
    const updatedProxyResponse = await waitForProxy(page.request, updatedEndpointUrl, {
      model: 'ignored',
      messages: [{ role: 'user', content: 'hello again' }],
    }, 'pw-model-v2')
    expect(updatedProxyResponse.ok()).toBe(true)
    expect(updatedProxyResponse.headers()['x-kong-llm-model']).toBe('pw-model-v2')
    expect(lastUpstreamModel).toBe('pw-model-v2')

    const streamingFetch = fetch(updatedEndpointUrl, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        model: 'ignored',
        messages: [{ role: 'user', content: 'stream hello' }],
        stream: true,
      }),
    })

    await streamingRequestStarted

    try {
      const firstChunk = await Promise.race([
        streamingFetch.then(async response => {
          expect(response.ok).toBe(true)
          expect(response.headers.get('content-type')).toContain('text/event-stream')

          const reader = response.body?.getReader()

          if (!reader) {
            throw new Error('Streaming proxy response did not include a readable body')
          }

          const chunk = await reader.read()
          return new TextDecoder().decode(chunk.value)
        }),
        new Promise<never>((_resolve, reject) => {
          setTimeout(
            () => reject(new Error('Proxy did not forward the first SSE chunk while the upstream remained open')),
            5_000,
          )
        }),
      ])

      expect(streamingUpstreamClosed).toBe(false)
      expect(firstChunk).toContain('"content":"first"')
    } finally {
      streamingResponse?.end('data: [DONE]\n\n')
    }

    await updatedCard.getByRole('button', { name: 'Delete' }).click()
    await expect(updatedCard).toBeHidden()

    for (const endpoint of ['services', 'routes', 'plugins', 'ai-models']) {
      const managed = (await listEntities(endpoint))
        .filter(entity => entity.tags?.includes(endpointOwnershipTag))
      expect(managed).toHaveLength(0)
    }

    const retainedProvider = (await listEntities('ai-providers'))
      .find(provider => provider.name === providerName)
    expect(retainedProvider).toBeTruthy()
  })

  test('supports provider and advanced-model CRUD without JSON editors', async ({ page }) => {
    await page.goto('/ai-gateway/providers')
    const providerRow = page.getByRole('row').filter({ hasText: providerName })
    await expect(providerRow).toBeVisible()
    await providerRow.getByRole('button', { name: 'Edit' }).click()
    await expect(page.getByLabel('API Key (optional)')).toBeVisible()
    await expect(page.getByText('Auth Config JSON')).toHaveCount(0)
    await expect(page.getByText('Runtime Config JSON')).toHaveCount(0)
    await page.getByLabel('Name').fill(updatedProviderName)
    await page.getByLabel('Default Model').fill('pw-model-v2')
    await page.getByRole('button', { name: 'Save Provider' }).click()

    const updatedProviderRow = page.getByRole('row').filter({ hasText: updatedProviderName })
    await expect(updatedProviderRow).toContainText('pw-model-v2')

    await page.getByRole('link', { name: 'Advanced Models' }).click()
    await page.getByRole('button', { name: 'Create Model' }).click()
    await expect(page.getByText('Config JSON')).toHaveCount(0)
    await page.getByLabel('Group Name').fill(advancedModelGroup)
    await page.getByLabel('Provider', { exact: true }).selectOption({
      label: `${updatedProviderName} (openai_compat)`,
    })
    await page.getByLabel('Provider Model Name').fill('pw-model')
    await page.getByLabel('Max Input Tokens').fill('64000')
    await page.getByLabel('Custom input override (USD / 1M tokens)').fill('0')
    await page.getByLabel('Custom output override (USD / 1M tokens)').fill('1.250000000000')
    await expect(page.getByText(
      'Leave blank to use the current built-in price; enter 0 to make this direction free.',
    )).toHaveCount(2)
    await page.getByRole('button', { name: 'Save Model' }).click()

    let modelRow = page.getByRole('row').filter({ hasText: advancedModelGroup })
    await expect(modelRow).toContainText('64000')
    await expect(modelRow).toContainText('$0 / 1M tokens')
    await expect(modelRow).toContainText('$1.25 / 1M tokens')
    await expect(modelRow).toContainText('Custom override')

    await modelRow.getByRole('button', { name: 'View Usage' }).click()
    await expect(page).toHaveURL(/\/ai-gateway\/usage/)
    const modelUsageQuery = new URL(page.url()).searchParams
    expect(modelUsageQuery.get('model_group')).toBe(advancedModelGroup)
    expect(modelUsageQuery.get('actual_model')).toBe('pw-model')
    expect(modelUsageQuery.get('provider_id')).toBeTruthy()
    await page.goBack()
    await expect(page.getByRole('heading', { name: 'AI Models' })).toBeVisible()

    modelRow = page.getByRole('row').filter({ hasText: advancedModelGroup })
    await modelRow.getByRole('button', { name: 'Edit' }).click()
    await expect(page.getByLabel('Custom input override (USD / 1M tokens)'))
      .toHaveValue(/^0(?:\.0+)?$/)
    await expect(page.getByText('Effective pricing', { exact: true })).toBeVisible()
    await page.getByLabel('Provider Model Name').fill('pw-model-v2')
    await page.getByLabel('Max Input Tokens').fill('128000')
    await page.getByRole('button', { name: 'Save Model' }).click()
    await expect(modelRow).toContainText('pw-model-v2')
    await expect(modelRow).toContainText('128000')

    await page.getByRole('link', { name: 'Provider Connections' }).click()
    const guardedProviderRow = page.getByRole('row').filter({ hasText: updatedProviderName })
    await guardedProviderRow.getByRole('button', { name: 'Delete' }).click()
    await expect(page.getByText(/Cannot delete provider .* while 1 dependent AI model remain/)).toBeVisible()
    await expect(guardedProviderRow).toBeVisible()

    await page.getByRole('link', { name: 'Advanced Models' }).click()
    const modelToDelete = page.getByRole('row').filter({ hasText: advancedModelGroup })
    await modelToDelete.getByRole('button', { name: 'Delete' }).click()
    await expect(modelToDelete).toBeHidden()

    await page.getByRole('link', { name: 'Provider Connections' }).click()
    const providerToDelete = page.getByRole('row').filter({ hasText: updatedProviderName })
    await providerToDelete.getByRole('button', { name: 'Delete' }).click()
    await expect(providerToDelete).toBeHidden()
  })

  test('supports virtual-key CRUD and one-time secret rotation', async ({ page }) => {
    const exactBudgetLimit = '9007199254740992.123456789012'

    await page.goto('/ai-gateway/virtual-keys')
    await expect(page.getByText(/Virtual keys authenticate AI traffic/)).toBeVisible()
    await page.getByRole('button', { name: 'Create Virtual Key' }).click()
    await page.getByLabel('Name').fill(virtualKeyName)
    await page.getByLabel('Allowed Models').fill('model-a, model-b')
    await page.getByLabel('TPM Limit').fill('1000')
    await page.getByLabel('RPM Limit').fill('60')
    await page.getByLabel('Budget Limit (USD / Lifetime cumulative)').fill(exactBudgetLimit)

    const createRequestPromise = page.waitForRequest((request) => {
      return request.method() === 'POST'
        && new URL(request.url()).pathname.endsWith('/ai-virtual-keys')
    })
    await page.getByRole('button', { name: 'Save Virtual Key' }).click()
    const createRequestBody = (await createRequestPromise).postDataJSON() as Record<string, unknown>
    expect(createRequestBody.budget_limit_decimal).toBe(exactBudgetLimit)
    expect(createRequestBody).not.toHaveProperty('budget_limit')

    const createdSecret = await page.locator('.ai-gateway-secret input').inputValue()
    expect(createdSecret).toMatch(/^sk-kr-/)
    await page.getByRole('button', { name: 'Dismiss' }).click()

    const keyRow = page.getByRole('row').filter({ hasText: virtualKeyName })
    await expect(keyRow).toContainText(`0 / ${exactBudgetLimit} USD`)
    await expect(keyRow.getByText('Awaiting plugin', { exact: true })).toHaveCount(2)
    await expect(keyRow).toContainText('Capability: local memory')
    await expect(keyRow).toContainText('Capability: PostgreSQL authoritative ledger')
    await keyRow.getByRole('button', { name: 'View Usage' }).click()
    await expect(page).toHaveURL(/\/ai-gateway\/usage/)
    expect(new URL(page.url()).searchParams.get('virtual_key_id')).toBeTruthy()
    await page.goBack()
    await expect(page.getByRole('heading', { name: 'AI Virtual Keys' })).toBeVisible()

    const returnedKeyRow = page.getByRole('row').filter({ hasText: virtualKeyName })
    await returnedKeyRow.getByRole('button', { name: 'Edit' }).click()
    await expect(page.getByLabel('Budget Limit (USD / Lifetime cumulative)'))
      .toHaveValue(exactBudgetLimit)
    await page.getByLabel('TPM Limit').fill('2000')

    const updateRequestPromise = page.waitForRequest((request) => {
      return request.method() === 'PATCH'
        && new URL(request.url()).pathname.includes('/ai-virtual-keys/')
    })
    await page.getByRole('button', { name: 'Save Virtual Key' }).click()
    const updateRequestBody = (await updateRequestPromise).postDataJSON() as Record<string, unknown>
    expect(updateRequestBody.budget_limit_decimal).toBe(exactBudgetLimit)
    expect(updateRequestBody).not.toHaveProperty('budget_limit')
    await expect(returnedKeyRow).toContainText('2000 TPM')

    await returnedKeyRow.getByRole('button', { name: 'Edit' }).click()
    await page.getByLabel('Budget Limit (USD / Lifetime cumulative)').fill('')
    const clearBudgetRequestPromise = page.waitForRequest((request) => {
      return request.method() === 'PATCH'
        && new URL(request.url()).pathname.includes('/ai-virtual-keys/')
    })
    await page.getByRole('button', { name: 'Save Virtual Key' }).click()
    const clearBudgetRequestBody = (await clearBudgetRequestPromise)
      .postDataJSON() as Record<string, unknown>
    expect(clearBudgetRequestBody.budget_limit_decimal).toBeNull()

    await returnedKeyRow.getByRole('button', { name: 'Rotate' }).click()
    const rotatedSecret = await page.locator('.ai-gateway-secret input').inputValue()
    expect(rotatedSecret).toMatch(/^sk-kr-/)
    expect(rotatedSecret).not.toBe(createdSecret)

    await page.getByRole('button', { name: 'Dismiss' }).click()
    await returnedKeyRow.getByRole('button', { name: 'Delete' }).click()
    await expect(returnedKeyRow).toBeHidden()
  })

  test('renders virtual-key capability and effective enforcement projection', async ({ page }) => {
    await page.route('**/ai-virtual-keys**', async (route) => {
      if (route.request().method() !== 'GET') {
        await route.continue()
        return
      }

      await route.fulfill({
        contentType: 'application/json',
        body: JSON.stringify({
          data: [
            {
              id: 'projection-partial',
              name: 'projection-partial',
              key_prefix: 'sk-kr-proj',
              allowed_models: ['model-a'],
              tpm_limit: 2000,
              rpm_limit: 120,
              budget_limit: null,
              budget_used: null,
              budget_limit_decimal: '9007199254740992.123456789012',
              budget_used_decimal: '7205759403792793.698765431211',
              capability: {
                quota: 'local_memory',
                budget: 'postgres_authoritative',
              },
              quota_enforcement: 'configured_local_partial',
              quota_backend: 'local_memory',
              quota_scope: 'node',
              quota_window_seconds: 60,
              budget_status: 'warning',
              budget_financial_status: 'warning',
              budget_backend: 'postgres',
              budget_percentage_decimal: '80.000000000001',
              coverage_available: true,
              auth_endpoint_count: 4,
              enforced_endpoint_count: 2,
              policy_error_count: 1,
              pending_intent_count: 3,
              unresolved_intent_count: 0,
              enabled: true,
              expires_at: null,
              tags: null,
            },
            {
              id: 'projection-awaiting',
              name: 'projection-awaiting',
              key_prefix: 'sk-kr-wait',
              allowed_models: [],
              tpm_limit: 1000,
              rpm_limit: 60,
              budget_limit: null,
              budget_used: null,
              budget_limit_decimal: '100.000000000000',
              budget_used_decimal: '0.000000000000',
              capability: {
                quota: 'local_memory_ephemeral',
                budget: 'postgres_authoritative',
              },
              quota_enforcement: 'awaiting_plugin',
              quota_backend: 'local_memory',
              quota_scope: 'node',
              quota_window_seconds: 60,
              budget_status: 'awaiting_plugin',
              budget_financial_status: 'active',
              budget_backend: 'postgres',
              budget_percentage_decimal: '0.000000000000',
              coverage_available: false,
              auth_endpoint_count: null,
              enforced_endpoint_count: null,
              policy_error_count: null,
              pending_intent_count: 0,
              unresolved_intent_count: 0,
              enabled: true,
              expires_at: null,
              tags: null,
            },
            {
              id: 'projection-over-limit',
              name: 'projection-over-limit',
              key_prefix: 'sk-kr-over',
              allowed_models: ['model-a'],
              tpm_limit: 2000,
              rpm_limit: 120,
              budget_limit: null,
              budget_used: null,
              budget_limit_decimal: '9007199254740992.123456789012',
              budget_used_decimal: '9999999999999999.999999999999',
              capability: {
                quota: 'local_memory',
                budget: 'postgres_authoritative',
              },
              quota_enforcement: 'configured_local',
              quota_backend: 'local_memory',
              quota_scope: 'node',
              quota_window_seconds: 60,
              budget_status: 'exhausted',
              budget_financial_status: 'exhausted',
              budget_backend: 'postgres',
              budget_percentage_decimal: '111.022302462516',
              coverage_available: true,
              auth_endpoint_count: 1,
              enforced_endpoint_count: 1,
              policy_error_count: 0,
              pending_intent_count: 0,
              unresolved_intent_count: 0,
              enabled: true,
              expires_at: null,
              tags: null,
            },
            {
              id: 'projection-paused',
              name: 'projection-paused',
              key_prefix: 'sk-kr-pause',
              allowed_models: [],
              tpm_limit: null,
              rpm_limit: null,
              budget_limit: null,
              budget_used: null,
              budget_limit_decimal: null,
              budget_used_decimal: '12.500000000001',
              capability: {
                quota: 'local_memory',
                budget: 'postgres_authoritative',
              },
              quota_enforcement: 'unconfigured',
              quota_backend: 'local_memory',
              quota_scope: 'node',
              quota_window_seconds: 60,
              budget_status: 'paused',
              budget_financial_status: 'paused',
              budget_backend: 'postgres',
              budget_percentage_decimal: null,
              coverage_available: true,
              auth_endpoint_count: 0,
              enforced_endpoint_count: 0,
              policy_error_count: 0,
              pending_intent_count: 0,
              unresolved_intent_count: 0,
              enabled: true,
              expires_at: null,
              tags: null,
            },
            {
              id: 'projection-unconfigured',
              name: 'projection-unconfigured',
              key_prefix: 'sk-kr-empty',
              allowed_models: [],
              tpm_limit: null,
              rpm_limit: null,
              budget_limit: null,
              budget_used: null,
              budget_limit_decimal: null,
              budget_used_decimal: '0.000000000000',
              capability: {
                quota: 'local_memory',
                budget: 'postgres_authoritative',
              },
              quota_enforcement: 'unconfigured',
              quota_backend: 'local_memory',
              quota_scope: 'node',
              quota_window_seconds: 60,
              budget_status: 'unconfigured',
              budget_financial_status: 'unconfigured',
              budget_backend: 'postgres',
              budget_percentage_decimal: null,
              coverage_available: true,
              auth_endpoint_count: 0,
              enforced_endpoint_count: 0,
              policy_error_count: 0,
              pending_intent_count: 0,
              unresolved_intent_count: 0,
              enabled: true,
              expires_at: null,
              tags: null,
            },
            {
              id: 'projection-unresolved',
              name: 'projection-unresolved',
              key_prefix: 'sk-kr-unresolved',
              allowed_models: [],
              tpm_limit: 1000,
              rpm_limit: 60,
              budget_limit: null,
              budget_used: null,
              budget_limit_decimal: '100.000000000000',
              budget_used_decimal: '20.000000000000',
              capability: {
                quota: 'local_memory',
                budget: 'postgres_authoritative',
              },
              quota_enforcement: 'configured_local',
              quota_backend: 'local_memory',
              quota_scope: 'node',
              quota_window_seconds: 60,
              budget_status: 'unresolved',
              budget_financial_status: 'unresolved',
              budget_backend: 'postgres',
              budget_percentage_decimal: '20.000000000000',
              coverage_available: true,
              auth_endpoint_count: 1,
              enforced_endpoint_count: 1,
              policy_error_count: 0,
              pending_intent_count: 0,
              unresolved_intent_count: 2,
              enabled: true,
              expires_at: null,
              tags: null,
            },
            {
              id: 'projection-unsupported',
              name: 'projection-unsupported',
              key_prefix: 'sk-kr-nope',
              allowed_models: [],
              tpm_limit: null,
              rpm_limit: null,
              budget_limit: null,
              budget_used: null,
              budget_limit_decimal: null,
              budget_used_decimal: '0.000000000000',
              capability: {
                quota: 'unsupported',
                budget: 'unsupported',
              },
              quota_enforcement: 'unsupported',
              quota_backend: null,
              quota_scope: null,
              quota_window_seconds: null,
              budget_status: 'unsupported',
              budget_financial_status: 'unconfigured',
              budget_backend: null,
              budget_percentage_decimal: null,
              coverage_available: false,
              auth_endpoint_count: null,
              enforced_endpoint_count: null,
              policy_error_count: null,
              pending_intent_count: 0,
              unresolved_intent_count: 0,
              enabled: true,
              expires_at: null,
              tags: null,
            },
          ],
        }),
        status: 200,
      })
    })

    await page.goto('/ai-gateway/virtual-keys')

    const partialRow = page.getByRole('row').filter({ hasText: 'projection-partial' })
    await expect(partialRow).toContainText(
      '7205759403792793.698765431211 / 9007199254740992.123456789012 USD',
    )
    await expect(partialRow).toContainText('Partially enforced')
    await expect(partialRow).toContainText('Warning')
    await expect(partialRow).toContainText('80.000000000001% of lifetime budget')
    await expect(partialRow).toContainText('2 of 4 authenticated endpoints enforced')
    await expect(partialRow).toContainText('3 pending requests')
    await expect(partialRow.getByRole('progressbar')).toHaveAttribute(
      'aria-valuenow',
      '80.000000000001',
    )

    const awaitingRow = page.getByRole('row').filter({ hasText: 'projection-awaiting' })
    await expect(awaitingRow.getByText('Awaiting plugin', { exact: true })).toHaveCount(2)
    await expect(awaitingRow).toContainText('Policy coverage awaiting plugin mounting')

    const overLimitRow = page.getByRole('row').filter({ hasText: 'projection-over-limit' })
    await expect(overLimitRow).toContainText(
      '9999999999999999.999999999999 / 9007199254740992.123456789012 USD',
    )
    await expect(overLimitRow.getByText('Exhausted', { exact: true })).toBeVisible()
    await expect(overLimitRow).toContainText('111.022302462516% of lifetime budget')
    const overLimitProgress = overLimitRow.getByRole('progressbar', {
      name: 'Lifecycle budget usage',
    })
    await expect(overLimitProgress).toHaveAttribute('aria-valuenow', '100')
    await expect(overLimitProgress).toHaveAttribute(
      'aria-valuetext',
      '111.022302462516% of lifetime budget',
    )
    await expect(overLimitProgress.locator('span')).toHaveAttribute('style', 'width: 100%;')

    const pausedRow = page.getByRole('row').filter({ hasText: 'projection-paused' })
    await expect(pausedRow).toContainText('12.500000000001 USD')
    await expect(pausedRow.getByText('Paused', { exact: true })).toBeVisible()
    await expect(pausedRow).toContainText('historical usage remains visible')
    await expect(pausedRow.getByRole('progressbar')).toHaveCount(0)

    const unconfiguredRow = page.getByRole('row').filter({ hasText: 'projection-unconfigured' })
    await expect(unconfiguredRow).toContainText('0 USD')
    await expect(unconfiguredRow.getByText('Not configured', { exact: true })).toHaveCount(3)
    await expect(unconfiguredRow).toContainText('No lifecycle budget is configured')
    await expect(unconfiguredRow.getByRole('progressbar')).toHaveCount(0)

    const unresolvedRow = page.getByRole('row').filter({ hasText: 'projection-unresolved' })
    await expect(unresolvedRow.getByText('Reconciliation required', { exact: true })).toBeVisible()
    await expect(unresolvedRow).toContainText('reconciled before new budgeted requests')
    await expect(unresolvedRow).toContainText('2 unresolved requests')

    const unsupportedRow = page.getByRole('row').filter({ hasText: 'projection-unsupported' })
    await expect(unsupportedRow.getByText('Unsupported', { exact: true })).toHaveCount(2)
    await expect(unsupportedRow).toContainText('unsupported in this deployment mode')

    const editButton = overLimitRow.getByRole('button', { name: 'Edit' })
    await editButton.focus()
    await expect(editButton).toBeFocused()
    await page.keyboard.press('Enter')
    await expect(page.getByLabel('Name')).toHaveValue('projection-over-limit')
    await page.getByRole('button', { name: 'Cancel' }).click()

    await page.setViewportSize({ width: 390, height: 844 })
    await expect(overLimitRow.getByText('Exhausted', { exact: true })).toBeVisible()
    expect(await page.evaluate(() => document.documentElement.scrollWidth))
      .toBeLessThanOrEqual(390)

    await page.getByLabel('Language').selectOption('zh-CN')
    await expect(partialRow).toContainText('部分执行')
    await expect(partialRow).toContainText('预警')
    await expect(awaitingRow.getByText('等待挂载插件', { exact: true })).toHaveCount(2)
    await expect(overLimitRow.getByText('已耗尽', { exact: true })).toBeVisible()
    await expect(pausedRow.getByText('已暂停', { exact: true })).toBeVisible()
    await expect(unresolvedRow.getByText('需要对账', { exact: true })).toBeVisible()
    await expect(unsupportedRow.getByText('不支持', { exact: true })).toHaveCount(2)
    await page.getByLabel('语言').selectOption('en')
    await page.setViewportSize({ width: 1920, height: 1080 })
  })

  test('reconciles an unresolved budget intent with a stable operation id', async ({ page }) => {
    let reconciled = false
    let submittedOperationId = ''
    await page.route('**/ai-virtual-keys**', async (route) => {
      const request = route.request()
      const path = new URL(request.url()).pathname

      if (path.endsWith('/budget-reconciliations') && request.method() === 'POST') {
        const body = request.postDataJSON() as Record<string, unknown>
        expect(body.intent_id).toBe('11111111-1111-4111-8111-111111111111')
        expect(body.action).toBeUndefined()
        expect(body.cost_usd_decimal).toBe('1.25')
        expect(body.waive).toBe(false)
        expect(body.reason).toBe('INC-42 provider invoice')
        expect(body.operation_id).toMatch(/^[0-9a-f-]{36}$/)
        submittedOperationId = String(body.operation_id)
        reconciled = true
        await route.fulfill({
          contentType: 'application/json',
          body: JSON.stringify({ disposition: 'applied' }),
          status: 200,
        })
        return
      }

      if (path.endsWith('/budget-ledger') && request.method() === 'GET') {
        await route.fulfill({
          contentType: 'application/json',
          body: JSON.stringify({
            account: {
              budget_used_decimal: reconciled ? '1.250000000000' : '0.000000000000',
              pending_intent_count: 0,
              unresolved_intent_count: reconciled ? 0 : 1,
              accounting_revision: reconciled ? 2 : 1,
            },
            data: reconciled
              ? []
              : [{
                id: '11111111-1111-4111-8111-111111111111',
                virtual_key_id: 'ledger-key',
                kind: 'request',
                status: 'unresolved',
                request_id: 'request-ledger-1',
                observed_cost_usd_decimal: '1.250000000000',
                cost_status: 'calculated',
                cost_reasons: ['upstream_outcome_unknown'],
                created_at: '2026-07-26T00:00:00Z',
              }],
          }),
          status: 200,
        })
        return
      }

      if (request.method() === 'GET') {
        await route.fulfill({
          contentType: 'application/json',
          body: JSON.stringify({
            data: [{
              id: 'ledger-key',
              name: 'ledger-key',
              key_prefix: 'sk-ledger',
              allowed_models: [],
              tpm_limit: 1000,
              rpm_limit: 60,
              budget_limit_decimal: '10.000000000000',
              budget_used_decimal: '0.000000000000',
              capability: { quota: 'local_memory', budget: 'postgres_authoritative' },
              quota_enforcement: 'configured_local',
              budget_status: 'unresolved',
              budget_financial_status: 'unresolved',
              coverage_available: true,
              auth_endpoint_count: 1,
              enforced_endpoint_count: 1,
              policy_error_count: 0,
              pending_intent_count: 0,
              unresolved_intent_count: 1,
              enabled: true,
            }],
          }),
          status: 200,
        })
        return
      }

      await route.continue()
    })

    await page.goto('/ai-gateway/virtual-keys')
    const row = page.getByRole('row').filter({ hasText: 'ledger-key' })
    await row.getByRole('button', { name: 'Budget Ledger' }).click()
    await expect(page.getByText('request-ledger-1')).toBeVisible()
    await page.getByLabel('Audit reason (required)').fill('INC-42 provider invoice')
    await page.getByRole('button', { name: 'Reconcile' }).click()
    await expect(page.getByLabel('Cost (USD)')).toHaveValue('1.250000000000')
    await page.getByRole('button', { name: 'Apply reconciliation' }).click()

    await expect(page.getByText('No pending or unresolved budget intents.')).toBeVisible()
    expect(submittedOperationId).not.toBe('')
  })

  test('uses the Kong Rust brand and supports persistent bilingual switching on every page', async ({
    browser,
    page,
  }) => {
    await page.goto('/ai-gateway')
    const menuItems = await page.getByRole('navigation', { name: 'Main menu' })
      .getByRole('link').allTextContents()
    expect(menuItems[0]).toBe('AI Gateway')
    expect(menuItems[1]).toBe('Overview')
    await expect(page.getByText('Kong Rust Manager', { exact: true })).toBeVisible()
    await expect(page.locator('a[href="https://github.com/kong-rust/kong-rust"]')).toBeVisible()

    await page.getByLabel('Language').selectOption('zh-CN')
    await expect(page.getByRole('heading', { name: 'AI 接口', exact: true })).toBeVisible()
    await expect(page.getByRole('button', { name: '创建接口' })).toBeVisible()
    await expect(page.getByRole('link', { name: 'AI 网关' })).toBeVisible()
    await expect(page.getByRole('link', { name: '概览' })).toBeVisible()

    await page.goto('/services')
    await expect(page.getByRole('heading', { name: '网关服务', exact: true })).toBeVisible()
    await expect(page.getByRole('link', { name: '消费者' })).toBeVisible()

    await page.reload()
    await expect(page.getByRole('heading', { name: '网关服务', exact: true })).toBeVisible()
    await page.getByLabel('语言').selectOption('en')
    await expect(page.getByRole('heading', { name: 'Gateway Services', exact: true })).toBeVisible()

    await page.goto('/')
    await expect(page.getByText('Resources', { exact: true })).toBeVisible()
    await expect(page.getByText(/Konnect/i)).toHaveCount(0)

    const managerOrigin = new URL(page.url()).origin
    const zhContext = await browser.newContext({ locale: 'zh-CN' })
    const zhPage = await zhContext.newPage()

    try {
      await zhPage.goto(`${managerOrigin}/services`)
      await expect(zhPage.getByRole('heading', { name: '网关服务', exact: true })).toBeVisible()
      await zhPage.goto(`${managerOrigin}/`)
      await expect(zhPage.getByText('资源', { exact: true })).toBeVisible()
    } finally {
      await zhContext.close()
    }
  })
})
