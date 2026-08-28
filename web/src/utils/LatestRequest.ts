/**
 * Own one replaceable request for a rapidly changing UI query.
 *
 * Aborting saves network/server work when Axios honors the signal. The
 * generation check also prevents a stale response from winning if a transport
 * ignores cancellation or completes during the abort race.
 */
export class LatestRequest {
  private generation = 0
  private controller?: AbortController

  async run<T>(request: (signal: AbortSignal) => Promise<T>): Promise<T | undefined> {
    const generation = ++this.generation
    this.controller?.abort()
    const controller = new AbortController()
    this.controller = controller

    try {
      const result = await request(controller.signal)
      return controller.signal.aborted || generation !== this.generation ? undefined : result
    } catch (error) {
      if (controller.signal.aborted || generation !== this.generation) return undefined
      throw error
    } finally {
      if (generation === this.generation) this.controller = undefined
    }
  }

  cancel() {
    this.generation += 1
    this.controller?.abort()
    this.controller = undefined
  }
}

export interface ListSnapshot<T> {
  scope: string
  rows: readonly T[]
}

/** Bind a list snapshot to both the newest request generation and the exact UI
 * scope that requested it. Consumers can hide an old page synchronously while
 * the replacement request is still pending. */
export class LatestListRequest<T> {
  private readonly latest = new LatestRequest()

  async run(
    scope: string,
    request: (signal: AbortSignal) => Promise<readonly T[]>
  ): Promise<ListSnapshot<T> | undefined> {
    const rows = await this.latest.run(request)
    return rows === undefined ? undefined : { scope, rows }
  }

  cancel() {
    this.latest.cancel()
  }
}

export const currentListSnapshotRows = <T>(scope: string, snapshot?: ListSnapshot<T>) =>
  snapshot?.scope === scope ? snapshot.rows : undefined
