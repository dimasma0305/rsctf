export const SIGNALR_RECORD_SEPARATOR = String.fromCharCode(0x1e);

export function validAbuseClose(code, oversized = false) {
  return code === (oversized ? 1009 : 1008);
}

export function signalrHandshake() {
  return `{"protocol":"json","version":1}${SIGNALR_RECORD_SEPARATOR}`;
}

export function unsupportedSignalrInvocation(sequence) {
  return `${JSON.stringify({ type: 1, invocationId: String(sequence), target: "Upload", arguments: ["x"] })}${SIGNALR_RECORD_SEPARATOR}`;
}

export function signalrPing() {
  return `{"type":6}${SIGNALR_RECORD_SEPARATOR}`;
}
