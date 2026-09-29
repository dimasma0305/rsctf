export const AGENT_SIGNATURE_LABEL_MAX_LENGTH = 64
export const AGENT_SIGNATURE_PATTERN_MAX_LENGTH = 512
const SIGNATURE_KEY = /^[a-z0-9][a-z0-9-]{0,39}$/

/** Client-side key checks; the server compiles the pattern and rejects over-broad ones. */
export const agentSignatureKeyProblem = (key: string, signatures: { key: string; builtin: boolean }[]) => {
  if (!SIGNATURE_KEY.test(key)) return 'format' as const
  const existing = signatures.find((signature) => signature.key === key)
  if (existing?.builtin) return 'builtin' as const
  if (existing) return 'duplicate' as const
  return null
}
