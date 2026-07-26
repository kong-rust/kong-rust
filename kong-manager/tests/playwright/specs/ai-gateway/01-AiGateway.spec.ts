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
    await page.goto('/ai-gateway/virtual-keys')
    await expect(page.getByText(/Keys authenticate proxy traffic/)).toBeVisible()
    await page.getByRole('button', { name: 'Create Virtual Key' }).click()
    await page.getByLabel('Name').fill(virtualKeyName)
    await page.getByLabel('Allowed Models').fill('model-a, model-b')
    await page.getByLabel('TPM Limit').fill('1000')
    await page.getByLabel('RPM Limit').fill('60')
    await page.getByRole('button', { name: 'Save Virtual Key' }).click()

    const createdSecret = await page.locator('.ai-gateway-secret input').inputValue()
    expect(createdSecret).toMatch(/^sk-kr-/)
    await page.getByRole('button', { name: 'Dismiss' }).click()

    const keyRow = page.getByRole('row').filter({ hasText: virtualKeyName })
    await keyRow.getByRole('button', { name: 'View Usage' }).click()
    await expect(page).toHaveURL(/\/ai-gateway\/usage/)
    expect(new URL(page.url()).searchParams.get('virtual_key_id')).toBeTruthy()
    await page.goBack()
    await expect(page.getByRole('heading', { name: 'AI Virtual Keys' })).toBeVisible()

    const returnedKeyRow = page.getByRole('row').filter({ hasText: virtualKeyName })
    await returnedKeyRow.getByRole('button', { name: 'Edit' }).click()
    await page.getByLabel('TPM Limit').fill('2000')
    await page.getByRole('button', { name: 'Save Virtual Key' }).click()
    await expect(returnedKeyRow).toContainText('2000 TPM')

    await returnedKeyRow.getByRole('button', { name: 'Rotate' }).click()
    const rotatedSecret = await page.locator('.ai-gateway-secret input').inputValue()
    expect(rotatedSecret).toMatch(/^sk-kr-/)
    expect(rotatedSecret).not.toBe(createdSecret)

    await page.getByRole('button', { name: 'Dismiss' }).click()
    await returnedKeyRow.getByRole('button', { name: 'Delete' }).click()
    await expect(returnedKeyRow).toBeHidden()
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

    const zhContext = await browser.newContext({ locale: 'zh-CN' })
    const zhPage = await zhContext.newPage()

    try {
      await zhPage.goto(`${process.env.KM_TEST_GUI_URL}/services`)
      await expect(zhPage.getByRole('heading', { name: '网关服务', exact: true })).toBeVisible()
      await zhPage.goto(`${process.env.KM_TEST_GUI_URL}/`)
      await expect(zhPage.getByText('资源', { exact: true })).toBeVisible()
    } finally {
      await zhContext.close()
    }
  })
})
