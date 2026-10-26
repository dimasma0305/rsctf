// Estimate JSON response working data without allocating a second serialized
// response. Work and traversal memory stay bounded even for cyclic/huge models.
const MAX_OBJECTS = 65_536
const MAX_EDGES = 262_144

export const estimateCacheBytes = (data: unknown, ceiling: number): number => {
  const seen = new WeakSet<object>()
  const pending: unknown[] = [data]
  let bytes = 0,
    objects = 0,
    edges = 0
  try {
    while (pending.length) {
      const value = pending.pop()
      if (value === null || value === undefined) continue
      if (typeof value === 'string') bytes += value.length * 2
      else if (typeof value === 'function') return ceiling + 1
      else if (typeof value !== 'object') bytes += 8
      else if (!seen.has(value)) {
        seen.add(value)
        if (++objects > MAX_OBJECTS) return ceiling + 1
        bytes += 64
        if (value instanceof ArrayBuffer) bytes += value.byteLength
        else if (typeof SharedArrayBuffer !== 'undefined' && value instanceof SharedArrayBuffer)
          bytes += value.byteLength
        else if (ArrayBuffer.isView(value)) bytes += value.buffer.byteLength
        else if (typeof Blob !== 'undefined' && value instanceof Blob) bytes += value.size
        else if (Array.isArray(value)) {
          if ((edges += value.length) > MAX_EDGES) return ceiling + 1
          bytes += value.length * 8
          for (const child of value) pending.push(child)
        } else {
          for (const key in value) {
            if (++edges > MAX_EDGES) return ceiling + 1
            if (!Object.prototype.hasOwnProperty.call(value, key)) continue
            bytes += 16 + key.length * 2
            pending.push((value as Record<string, unknown>)[key])
          }
        }
      }
      if (bytes > ceiling) return bytes
    }
  } catch {
    return ceiling + 1
  }
  return bytes
}
