import { isAxiosError } from 'axios'
import { apiService } from '@/services/apiService'
import type {
  AiUsageApiErrorBody,
  AiUsagePage,
  AiUsageSummary,
} from '../aiUsageTypes'

type QueryParams = Record<string, boolean | number | string | undefined>

const compactParams = (params: QueryParams) => Object.fromEntries(
  Object.entries(params).filter(([, value]) => value !== '' && value !== undefined),
)

export class AiUsageRequestError extends Error {
  status: number | null
  errorCode: string | null

  constructor(message: string, status: number | null, errorCode: string | null) {
    super(message)
    this.name = 'AiUsageRequestError'
    this.status = status
    this.errorCode = errorCode
  }
}

const normalizeError = (error: unknown, fallback: string) => {
  if (error instanceof AiUsageRequestError) {
    return error
  }

  if (isAxiosError<AiUsageApiErrorBody>(error)) {
    return new AiUsageRequestError(
      error.response?.data?.message || error.message || fallback,
      error.response?.status ?? null,
      error.response?.data?.error_code ?? null,
    )
  }

  return new AiUsageRequestError(
    error instanceof Error ? error.message : fallback,
    null,
    null,
  )
}

export const aiUsageService = {
  async summary(params: QueryParams, signal?: AbortSignal) {
    try {
      const { data } = await apiService.get<AiUsageSummary>('ai-usage/summary', {
        params: compactParams(params),
        signal,
      })

      return data
    } catch (error) {
      throw normalizeError(error, 'Unable to load AI usage summary')
    }
  },

  async list(params: QueryParams, signal?: AbortSignal) {
    try {
      const { data } = await apiService.get<AiUsagePage>('ai-usage', {
        params: compactParams(params),
        signal,
      })

      return data
    } catch (error) {
      throw normalizeError(error, 'Unable to load AI usage logs')
    }
  },
}
