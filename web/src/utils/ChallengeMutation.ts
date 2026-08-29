export interface ChallengeMutationOperation {
  digest: string
  id: string
}

interface PreparedChallengeMutation<T> {
  operation: ChallengeMutationOperation
  payload: T & { operationId: string; expectedRevision?: number }
}

export function challengeRevision(challenge: unknown): number | undefined {
  if (!challenge || typeof challenge !== 'object') return undefined
  const revision = (challenge as { revision?: unknown }).revision
  return typeof revision === 'number' && Number.isSafeInteger(revision) && revision >= 0 ? revision : undefined
}

/** Keep one idempotency key for retries of the same challenge mutation. */
export function prepareChallengeMutation<T extends object>(
  payload: T,
  expectedRevision: number | undefined,
  previous: ChallengeMutationOperation | null | undefined,
  createId: () => string = () => crypto.randomUUID()
): PreparedChallengeMutation<T> {
  const compatiblePayload = payload as T & { operationId?: unknown; expectedRevision?: unknown }
  const { operationId: _operationId, expectedRevision: _expectedRevision, ...body } = compatiblePayload
  const digest = JSON.stringify({ expectedRevision, body })
  const operation = previous?.digest === digest ? previous : { digest, id: createId() }

  return {
    operation,
    payload: {
      ...body,
      operationId: operation.id,
      ...(expectedRevision === undefined ? {} : { expectedRevision }),
    } as T & { operationId: string; expectedRevision?: number },
  }
}
