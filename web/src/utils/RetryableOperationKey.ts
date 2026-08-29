import { createUuid } from './Uuid'

/** Keeps one durable mutation identity until the server acknowledges it. */
export class RetryableOperationKey {
  private value: string | null = null

  constructor(private readonly create: () => string = createUuid) {}

  claim(): string {
    this.value ??= this.create()
    return this.value
  }

  complete(operationId: string): void {
    if (this.value === operationId) this.value = null
  }
}
