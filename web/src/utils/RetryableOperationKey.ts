import { createUuid } from './Uuid'

type OperationKeyStorage = Pick<Storage, 'getItem' | 'setItem' | 'removeItem'>

const UUID_PATTERN = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i

const browserSessionStorage = (): OperationKeyStorage | undefined => {
  try {
    return globalThis.sessionStorage
  } catch {
    return undefined
  }
}

/** Keeps one durable mutation identity until the server acknowledges it. */
export class RetryableOperationKey {
  private value: string | null = null

  constructor(
    private readonly create: () => string = createUuid,
    private readonly storageKey?: string,
    private readonly storage: OperationKeyStorage | undefined = browserSessionStorage()
  ) {
    if (!storageKey || !storage) return
    try {
      const stored = storage.getItem(storageKey)
      if (stored && UUID_PATTERN.test(stored)) this.value = stored
      else if (stored) storage.removeItem(storageKey)
    } catch {
      // Storage failures keep the operation safe for this component lifetime.
    }
  }

  claim(): string {
    if (this.value === null) {
      this.value = this.create()
      if (this.storageKey && this.storage) {
        try {
          this.storage.setItem(this.storageKey, this.value)
        } catch {
          // The in-memory identity still protects retries until unmount.
        }
      }
    }
    return this.value
  }

  complete(operationId: string): void {
    if (this.value !== operationId) return
    this.value = null
    if (this.storageKey && this.storage) {
      try {
        this.storage.removeItem(this.storageKey)
      } catch {
        // Completion is authoritative even when storage cleanup is unavailable.
      }
    }
  }
}
