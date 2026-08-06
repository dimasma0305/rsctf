const HASH_PATH = /^\/assets\/([0-9a-fA-F]{64})\/[^/?#\r\n]+$/;

export function assetHashFromPath(path) {
  const match = String(path || "").match(HASH_PATH);
  return match ? match[1].toLowerCase() : null;
}

export function assetRange(sequence, size, rangeBytes) {
  if (
    !Number.isSafeInteger(sequence) ||
    sequence < 0 ||
    !Number.isSafeInteger(size) ||
    size <= 0 ||
    !Number.isSafeInteger(rangeBytes) ||
    rangeBytes <= 0 ||
    rangeBytes > size
  ) {
    throw new Error("invalid attachment range inputs");
  }
  const slots = Math.ceil(size / rangeBytes);
  const start = (sequence % slots) * rangeBytes;
  const end = Math.min(start + rangeBytes, size) - 1;
  return { start, end, length: end - start + 1 };
}
