import { createUuid } from './Uuid'

export interface MutationLease {
  digest: string
  generation: number
  operationId: string
  signal: AbortSignal
}

/** Synchronous owner for one retryable HTTP mutation generation. */
export class RetryableMutationOwner {
  private controller: AbortController | null = null
  private digest: string | null = null
  private generation = 0
  private operationId: string | null = null

  constructor(private readonly createId: () => string = createUuid) {}

  claim(digest: string, requestedOperationId?: string): MutationLease | null {
    if (this.controller) return null
    if (this.digest !== digest || !this.operationId) {
      this.digest = digest
      this.operationId = requestedOperationId ?? this.createId()
    } else if (requestedOperationId && requestedOperationId !== this.operationId) {
      this.digest = digest
      this.operationId = requestedOperationId
    }
    this.generation += 1
    this.controller = new AbortController()
    return {
      digest,
      generation: this.generation,
      operationId: this.operationId,
      signal: this.controller.signal,
    }
  }

  owns(lease: MutationLease): boolean {
    return (
      this.controller !== null &&
      this.digest === lease.digest &&
      this.generation === lease.generation &&
      this.operationId === lease.operationId
    )
  }

  isActive(): boolean {
    return this.controller !== null
  }

  settle(lease: MutationLease, committed: boolean): boolean {
    if (!this.owns(lease)) return false
    this.controller = null
    if (committed) {
      this.digest = null
      this.operationId = null
    }
    return true
  }

  cancel(): void {
    this.generation += 1
    this.controller?.abort()
    this.controller = null
    this.digest = null
    this.operationId = null
  }
}
