import { createUuid } from './Uuid'

type OperationKeyStorage = Pick<Storage, 'getItem' | 'setItem' | 'removeItem'>

interface StoredOperationIdentity {
  operationId: string
  touchedAt: number
}

interface StoredOperationManifest {
  version: 1
  entries: Record<string, StoredOperationIdentity>
}

export const RETRYABLE_OPERATION_STORAGE_KEY = 'rsctf:retryable-operation-identities:v1'
export const RETRYABLE_OPERATION_STORAGE_LIMIT = 32

const UUID_PATTERN = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i
const MAX_MANIFEST_BYTES = 64 * 1024
const activeScopesByStorage = new WeakMap<object, Map<string, number>>()

const browserSessionStorage = (): OperationKeyStorage | undefined => {
  try {
    return globalThis.sessionStorage
  } catch {
    return undefined
  }
}

function legacyOperationId(stored: string): string | null {
  if (UUID_PATTERN.test(stored)) return stored
  if (stored.length > 1_024) return null
  try {
    const parsed = JSON.parse(stored) as { id?: unknown; operationId?: unknown }
    const candidate = typeof parsed.operationId === 'string' ? parsed.operationId : parsed.id
    return typeof candidate === 'string' && UUID_PATTERN.test(candidate) ? candidate : null
  } catch {
    return null
  }
}

function activeScopes(storage: OperationKeyStorage): Map<string, number> {
  let scopes = activeScopesByStorage.get(storage)
  if (!scopes) {
    scopes = new Map()
    activeScopesByStorage.set(storage, scopes)
  }
  return scopes
}

function readManifest(storage: OperationKeyStorage): StoredOperationManifest {
  try {
    const stored = storage.getItem(RETRYABLE_OPERATION_STORAGE_KEY)
    if (!stored || stored.length > MAX_MANIFEST_BYTES) return { version: 1, entries: {} }
    const parsed = JSON.parse(stored) as Partial<StoredOperationManifest>
    if (parsed.version !== 1 || !parsed.entries || typeof parsed.entries !== 'object') {
      return { version: 1, entries: {} }
    }

    const entries: Record<string, StoredOperationIdentity> = {}
    for (const [scope, candidate] of Object.entries(parsed.entries)) {
      if (
        scope.length === 0 ||
        scope.length > 256 ||
        !candidate ||
        typeof candidate !== 'object' ||
        !UUID_PATTERN.test(candidate.operationId) ||
        !Number.isFinite(candidate.touchedAt) ||
        candidate.touchedAt < 0
      ) {
        continue
      }
      entries[scope] = { operationId: candidate.operationId, touchedAt: candidate.touchedAt }
    }
    return { version: 1, entries }
  } catch {
    return { version: 1, entries: {} }
  }
}

function pruneManifest(
  storage: OperationKeyStorage,
  manifest: StoredOperationManifest,
  preserveScope?: string
): StoredOperationManifest {
  const active = activeScopes(storage)
  const entries = Object.entries(manifest.entries)
  if (entries.length <= RETRYABLE_OPERATION_STORAGE_LIMIT) return manifest

  entries.sort(([leftScope, left], [rightScope, right]) => {
    const leftProtected = leftScope === preserveScope || active.has(leftScope)
    const rightProtected = rightScope === preserveScope || active.has(rightScope)
    if (leftProtected !== rightProtected) return leftProtected ? 1 : -1
    if (left.touchedAt !== right.touchedAt) return left.touchedAt - right.touchedAt
    return leftScope.localeCompare(rightScope)
  })

  const retained = new Map(entries)
  for (const [scope] of entries) {
    if (retained.size <= RETRYABLE_OPERATION_STORAGE_LIMIT) break
    if (scope === preserveScope || active.has(scope)) continue
    retained.delete(scope)
  }
  return { version: 1, entries: Object.fromEntries(retained) }
}

function writeManifest(
  storage: OperationKeyStorage,
  manifest: StoredOperationManifest,
  preserveScope?: string
): boolean {
  const bounded = pruneManifest(storage, manifest, preserveScope)
  try {
    storage.setItem(RETRYABLE_OPERATION_STORAGE_KEY, JSON.stringify(bounded))
    return true
  } catch {
    // A full session store may reject even the bounded replacement. Retry with
    // only identities in active use (plus this owner's current retry identity)
    // before falling back to memory for the component lifetime.
    const active = activeScopes(storage)
    const essentialEntries = Object.fromEntries(
      Object.entries(bounded.entries).filter(([scope]) => scope === preserveScope || active.has(scope))
    )
    try {
      storage.setItem(
        RETRYABLE_OPERATION_STORAGE_KEY,
        JSON.stringify({ version: 1, entries: essentialEntries } satisfies StoredOperationManifest)
      )
      return true
    } catch {
      return false
    }
  }
}

/** Keeps one durable mutation identity until the server acknowledges it. */
export class RetryableOperationKey {
  private value: string | null = null
  private active = false
  private restoredFromLegacyKey = false
  private readonly create: () => string
  private readonly scope?: string
  private readonly storage: OperationKeyStorage | undefined
  private readonly now: () => number

  constructor(
    create: () => string = createUuid,
    scope?: string,
    storage: OperationKeyStorage | undefined = browserSessionStorage(),
    now: () => number = Date.now
  ) {
    this.create = create
    this.scope = scope
    this.storage = storage
    this.now = now
    if (!scope || !storage) return
    const stored = readManifest(storage).entries[scope]
    if (stored) {
      this.value = stored.operationId
      return
    }

    // Migrate the original one-sessionStorage-key-per-scope representation on
    // first use. Removal happens only after the manifest write succeeds.
    try {
      const legacy = storage.getItem(scope)
      const legacyId = legacy ? legacyOperationId(legacy) : null
      if (legacyId) {
        this.value = legacyId
        this.restoredFromLegacyKey = true
      } else if (legacy) {
        storage.removeItem(scope)
      }
    } catch {
      // Storage failures keep the operation safe for this component lifetime.
    }
  }

  claim(): string {
    if (this.value === null) this.value = this.create()
    this.activate()
    this.persist()
    return this.value
  }

  /** Release the in-page lease while retaining the identity for a later retry. */
  release(): void {
    this.deactivate()
    if (!this.scope || !this.storage || !this.value) return
    writeManifest(this.storage, readManifest(this.storage), this.scope)
  }

  complete(operationId: string): void {
    if (this.value !== operationId) return
    this.deactivate()
    this.value = null
    if (!this.scope || !this.storage) return

    const manifest = readManifest(this.storage)
    if (manifest.entries[this.scope]?.operationId === operationId) delete manifest.entries[this.scope]
    writeManifest(this.storage, manifest)
    try {
      this.storage.removeItem(this.scope)
    } catch {
      // Completion is authoritative even when legacy storage cleanup fails.
    }
  }

  private activate(): void {
    if (this.active || !this.scope || !this.storage) return
    this.active = true
    const scopes = activeScopes(this.storage)
    scopes.set(this.scope, (scopes.get(this.scope) ?? 0) + 1)
  }

  private deactivate(): void {
    if (!this.active || !this.scope || !this.storage) return
    this.active = false
    const scopes = activeScopes(this.storage)
    const remaining = (scopes.get(this.scope) ?? 1) - 1
    if (remaining > 0) scopes.set(this.scope, remaining)
    else scopes.delete(this.scope)
  }

  private persist(): void {
    if (!this.scope || !this.storage || !this.value) return
    const manifest = readManifest(this.storage)
    manifest.entries[this.scope] = { operationId: this.value, touchedAt: this.now() }
    if (!writeManifest(this.storage, manifest, this.scope)) return
    if (!this.restoredFromLegacyKey) return
    try {
      this.storage.removeItem(this.scope)
      this.restoredFromLegacyKey = false
    } catch {
      // The bounded manifest is authoritative even if old-key cleanup fails.
    }
  }
}
