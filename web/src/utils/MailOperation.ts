import { createUuid } from './Uuid'

export interface MailOperationOwner {
  operationId: string
  signature: string
  controller: AbortController
  running: boolean
}

export interface MailOperationStart {
  owner: MailOperationOwner
  started: boolean
}

/**
 * Acquire a synchronous owner before captcha, password hashing, or API work.
 * An ambiguous retry of the same semantic form reuses its operation ID, while
 * an edited form starts a new intent and cancels stale browser work.
 */
export const beginMailOperation = (current: MailOperationOwner | null, signature: string): MailOperationStart => {
  if (current?.running) return { owner: current, started: false }
  if (current?.signature === signature) {
    current.controller = new AbortController()
    current.running = true
    return { owner: current, started: true }
  }
  current?.controller.abort()
  return {
    owner: {
      operationId: createUuid(),
      signature,
      controller: new AbortController(),
      running: true,
    },
    started: true,
  }
}

/** Keep an ambiguous intent for explicit retry; clear only a known response. */
export const finishMailOperation = (owner: MailOperationOwner, completed: boolean): MailOperationOwner | null => {
  owner.running = false
  return completed ? null : owner
}
