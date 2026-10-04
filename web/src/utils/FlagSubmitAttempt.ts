import { createUuid } from './Uuid'

export interface FlagSubmitAttemptInput {
  gameId: number
  challengeId: number
  flag: string
  proof?: string
}

export interface FlagSubmitAttemptPayload {
  attemptId: string
  flag: string
  proof?: string
}

export interface FlagSubmitAttemptResult {
  attemptId: string
  submissionId: number
  firstAcknowledgement: boolean
}

export interface FlagSubmitDispatch {
  /** Only the owner performs UI side effects after the shared request settles. */
  owner: boolean
  attemptId: string
  result: Promise<FlagSubmitAttemptResult>
}

interface AttemptEntry {
  semanticKey: string
  attemptId: string
  payload?: Promise<FlagSubmitAttemptPayload>
  inFlight?: Promise<FlagSubmitAttemptResult>
  submissionId?: number
  acknowledged: boolean
}

function scopeKey(input: FlagSubmitAttemptInput): string {
  return `${input.gameId}:${input.challengeId}`
}

function semanticKey(input: FlagSubmitAttemptInput): string {
  return JSON.stringify([input.flag, input.proof ?? null])
}

export function createOpaqueSubmitAttemptId(): string {
  return createUuid()
}

/**
 * Owns the one retry identity and encrypted wire payload for each open
 * challenge. A transport failure keeps both; a terminal verdict explicitly
 * releases them. PostgreSQL idempotency remains the correctness authority.
 */
export class FlagSubmitAttemptOwner {
  private readonly attempts = new Map<string, AttemptEntry>()

  constructor(private readonly createAttemptId: () => string = createOpaqueSubmitAttemptId) {}

  begin(
    input: FlagSubmitAttemptInput,
    prepare: (attemptId: string) => Promise<FlagSubmitAttemptPayload>,
    send: (payload: FlagSubmitAttemptPayload) => Promise<number>
  ): FlagSubmitDispatch {
    const scope = scopeKey(input)
    const semantic = semanticKey(input)
    let entry = this.attempts.get(scope)

    // React state updates are not a synchronous double-click lock. The ref
    // owner is: a second dispatch never starts another request, even if its
    // callback runs before the button renders disabled.
    if (entry?.inFlight) {
      return {
        owner: false,
        attemptId: entry.attemptId,
        result: entry.inFlight,
      }
    }

    if (!entry || entry.semanticKey !== semantic) {
      entry = {
        semanticKey: semantic,
        attemptId: this.createAttemptId(),
        acknowledged: false,
      }
      this.attempts.set(scope, entry)
    }
    const ownedEntry = entry

    if (!ownedEntry.payload) {
      ownedEntry.payload = prepare(ownedEntry.attemptId).catch((error: unknown) => {
        // No HTTP request could have committed when local preparation failed.
        if (this.attempts.get(scope) === ownedEntry && ownedEntry.submissionId === undefined) {
          this.attempts.delete(scope)
        }
        throw error
      })
    }

    const operation = (async (): Promise<FlagSubmitAttemptResult> => {
      if (ownedEntry.submissionId === undefined) {
        const payload = await ownedEntry.payload!
        if (payload.attemptId !== ownedEntry.attemptId) {
          throw new Error('Prepared submission payload changed its attempt identity')
        }
        ownedEntry.submissionId = await send(payload)
      }
      const firstAcknowledgement = !ownedEntry.acknowledged
      ownedEntry.acknowledged = true
      return {
        attemptId: ownedEntry.attemptId,
        submissionId: ownedEntry.submissionId,
        firstAcknowledgement,
      }
    })()
    ownedEntry.inFlight = operation
    void operation.then(
      () => {
        if (ownedEntry.inFlight === operation) ownedEntry.inFlight = undefined
      },
      () => {
        if (ownedEntry.inFlight === operation) ownedEntry.inFlight = undefined
      }
    )

    return {
      owner: true,
      attemptId: ownedEntry.attemptId,
      result: operation,
    }
  }

  complete(gameId: number, challengeId: number, attemptId: string): void {
    const scope = `${gameId}:${challengeId}`
    if (this.attempts.get(scope)?.attemptId === attemptId) this.attempts.delete(scope)
  }
}
