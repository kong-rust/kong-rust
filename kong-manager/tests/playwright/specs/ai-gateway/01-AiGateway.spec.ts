import { expect } from '@playwright/test'
import baseTest from '@pw/base-test'
import axios from 'axios'
import { createServer, type Server, type ServerResponse } from 'node:http'
import type { AddressInfo } from 'node:net'

const test = baseTest()
const suffix = `${Date.now()}`
const providerName = `pw-ai-provider-${suffix}`
const modelGroup = `pw-ai-model-${suffix}`
const virtualKeyName = `pw-ai-key-${suffix}`
const routeResourceName = `ai-${modelGroup}`
const tag = `pw-ai-gateway-${suffix}`
let mockUpstream: Server
let mockUpstreamEndpoint = ''
let lastUpstreamModel = ''
let streamingResponse: ServerResponse | null = null
let streamingUpstreamClosed = false
let streamingRequestStarted: Promise<void>
let resolveStreamingRequestStarted: () => void

interface KongEntity {
  id: string
  name?: string
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

const deleteMatching = async (
  endpoint: string,
  matches: (entity: KongEntity) => boolean,
) => {
  const { data } = await axios.get<{ data: KongEntity[] }>(`${apiUrl()}/${endpoint}`, {
    params: { size: 1000 },
  })

  for (const entity of data.data.filter(matches)) {
    await axios.delete(`${apiUrl()}/${endpoint}/${entity.id}`)
  }
}

const cleanup = async () => {
  await deleteMatching(
    'plugins',
    entity => entity.config?.model_group === modelGroup || entity.config?.model === modelGroup,
  )
  await deleteMatching('routes', entity => entity.name === routeResourceName)
  await deleteMatching('services', entity => entity.name === routeResourceName)
  await deleteMatching('ai-virtual-keys', entity => entity.name === virtualKeyName)
  await deleteMatching('ai-models', entity => entity.name === modelGroup)
  await deleteMatching('ai-providers', entity => entity.name === providerName)
}

test.describe('AI Gateway manager', () => {
  test.beforeAll(async () => {
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
            + '"created":1,"model":"deepseek-chat","choices":[{"index":0,'
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

  test('configures and safely cleans up an AI proxy route', async ({ page }) => {
    page.on('dialog', dialog => dialog.accept())

    await page.goto('/ai-gateway/providers')
    await page.getByRole('button', { name: 'Create Provider' }).click()
    await page.getByLabel('Name').fill(providerName)
    await page.getByLabel('Provider Type').selectOption('openai_compat')
    await page.getByLabel('Endpoint URL').fill(mockUpstreamEndpoint)
    await page.getByLabel('Default Model').fill('deepseek-chat')
    await page.getByLabel('Auth Config JSON').fill(
      '{"header_name":"Authorization","header_value":"test-only-placeholder"}',
    )
    await page.getByLabel('Tags').fill(tag)
    await page.getByRole('button', { name: 'Save Provider' }).click()

    const providerRow = page.getByRole('row').filter({ hasText: providerName })
    await expect(providerRow).toBeVisible()

    await page.getByRole('link', { name: 'Models', exact: true }).click()
    await page.getByRole('button', { name: 'Create Model' }).click()
    await page.getByLabel('Group Name').fill(modelGroup)
    await page.getByLabel('Provider', { exact: true }).selectOption({
      label: `${providerName} (openai_compat)`,
    })
    await page.getByLabel('Provider Model Name').fill('deepseek-chat')
    await page.getByLabel('Max Input Tokens').fill('64000')
    await page.getByLabel('Tags').fill(tag)
    await page.getByRole('button', { name: 'Save Model' }).click()

    const modelRow = page.getByRole('row').filter({ hasText: modelGroup })
    await expect(modelRow).toBeVisible()

    await page.getByRole('link', { name: 'Providers', exact: true }).click()
    const guardedProviderRow = page.getByRole('row').filter({ hasText: providerName })
    await guardedProviderRow.getByRole('button', { name: 'Delete' }).click()
    await expect(page.getByText(/Cannot delete provider .* while 1 dependent AI model remain/)).toBeVisible()
    await expect(guardedProviderRow).toBeVisible()

    await page.getByRole('link', { name: 'Models', exact: true }).click()
    const routeModelRow = page.getByRole('row').filter({ hasText: modelGroup })
    await routeModelRow.getByRole('button', { name: 'Create Route' }).click()
    await page.getByRole('button', { name: 'Create Proxy Route' }).click()

    const endpointInput = page.locator('.ai-gateway-secret input')
    await expect(page.getByText('AI proxy route is ready')).toBeVisible()
    await expect(endpointInput).toHaveValue(
      `http://127.0.0.1:8000/ai/${modelGroup}/v1/chat/completions`,
    )
    const { data: createdRoute } = await axios.get<KongEntity>(
      `${apiUrl()}/routes/${routeResourceName}`,
    )
    expect(createdRoute.response_buffering).toBe(false)

    const proxyResponse = await page.request.post(await endpointInput.inputValue(), {
      data: {
        model: modelGroup,
        messages: [{ role: 'user', content: 'hello' }],
      },
    })
    expect(proxyResponse.ok()).toBe(true)
    expect(proxyResponse.headers()['content-type']).toContain('application/json')
    expect(proxyResponse.headers()['x-kong-llm-model']).toBe('deepseek-chat')
    expect((await proxyResponse.json()).choices[0].message.content).toBe('mock upstream ok')
    expect(lastUpstreamModel).toBe('deepseek-chat')

    const streamingFetch = fetch(await endpointInput.inputValue(), {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        model: modelGroup,
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

    await page.getByRole('link', { name: 'Virtual Keys', exact: true }).click()
    await expect(page.getByText(/management metadata only/)).toBeVisible()
    await page.getByRole('button', { name: 'Create Virtual Key' }).click()
    await page.getByLabel('Name').fill(virtualKeyName)
    await page.getByLabel('Allowed Models').fill(modelGroup)
    await page.getByLabel('TPM Limit').fill('1000')
    await page.getByLabel('RPM Limit').fill('60')
    await page.getByLabel('Tags').fill(tag)
    await page.getByRole('button', { name: 'Save Virtual Key' }).click()

    const createdSecret = await page.locator('.ai-gateway-secret input').inputValue()
    expect(createdSecret).toMatch(/^sk-kr-/)
    await expect(page.getByRole('button', { name: 'Create Virtual Key' })).toBeDisabled()
    await expect(page.getByRole('button', { name: 'Rotate' })).toBeDisabled()

    await page.getByRole('button', { name: 'Dismiss' }).click()
    await page.getByRole('button', { name: 'Rotate' }).click()
    const rotatedSecret = await page.locator('.ai-gateway-secret input').inputValue()
    expect(rotatedSecret).toMatch(/^sk-kr-/)
    expect(rotatedSecret).not.toBe(createdSecret)

    await page.getByRole('button', { name: 'Dismiss' }).click()
    await page.getByRole('button', { name: 'Delete' }).click()
    await expect(page.getByText('No AI virtual keys')).toBeVisible()

    await page.getByRole('link', { name: 'Models', exact: true }).click()
    const modelToDelete = page.getByRole('row').filter({ hasText: modelGroup })
    await modelToDelete
      .getByRole('button', { name: 'Delete' }).click()
    await expect(modelToDelete).toBeHidden()

    await page.getByRole('link', { name: 'Providers', exact: true }).click()
    const providerToDelete = page.getByRole('row').filter({ hasText: providerName })
    await providerToDelete
      .getByRole('button', { name: 'Delete' }).click()
    await expect(providerToDelete).toBeHidden()
  })
})
