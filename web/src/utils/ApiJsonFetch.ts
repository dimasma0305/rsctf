import axios, { type AxiosInstance, type AxiosResponse } from 'axios'
import api from '@Api'

export type ApiJsonFetchInit = {
  readonly headers?: Readonly<Record<string, string>>
  readonly signal?: AbortSignal | null
}

type ApiJsonFetchResponse = Pick<Response, 'ok' | 'status' | 'json'> & {
  readonly retryAfter: string | null
}

const responseHeader = (response: AxiosResponse, name: string): string | null => {
  const headers = response.headers as unknown as {
    get?: (key: string) => unknown
    [key: string]: unknown
  }
  const value = typeof headers.get === 'function' ? headers.get(name) : headers[name.toLowerCase()]
  return value == null ? null : String(value)
}

const jsonResponse = (response: AxiosResponse): ApiJsonFetchResponse => ({
  ok: response.status >= 200 && response.status < 300,
  status: response.status,
  retryAfter: responseHeader(response, 'retry-after'),
  // Axios has already decoded the JSON body. Keep the small fetch-shaped
  // surface used by the arena without returning request headers, cookies, or
  // the short-lived Event-VPN proof attached by its interceptor.
  json: async () => response.data,
})

/**
 * Adapt the configured API client to the read-only fetch shape used by the
 * arena. HTTP errors remain inspectable through `ok`/`status`, while network
 * errors and AbortSignals retain native-fetch rejection semantics.
 */
export const createApiJsonFetch =
  (instance: AxiosInstance) =>
  async (path: string, init: ApiJsonFetchInit = {}): Promise<ApiJsonFetchResponse> => {
    // This adapter is for the same-origin API singleton. Refuse absolute,
    // protocol-relative, and backslash-normalized URLs before an interceptor
    // could attach a session-bound Event-VPN proof to another origin.
    if (!path.startsWith('/') || path.startsWith('//') || path.includes('\\')) {
      throw new TypeError('API JSON fetch requires a same-origin absolute path')
    }
    try {
      return jsonResponse(
        await instance.get(path, {
          headers: init.headers,
          signal: init.signal ?? undefined,
        })
      )
    } catch (error) {
      if (axios.isAxiosError(error) && error.response) return jsonResponse(error.response)
      throw error
    }
  }

// `api.instance` owns EventVpnProof and ServerClock interceptors. Consumers
// receive only the response body/status surface, never proof or session data.
export const apiJsonFetch = createApiJsonFetch(api.instance)
