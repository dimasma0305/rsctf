export const SIGNALR_RECORD_SEPARATOR = String.fromCharCode(0x1e);

export function validAbuseClose(code) {
  return code === 1008 || code === 1009;
}

export function signalrHandshake() {
  return `{"protocol":"json","version":1}${SIGNALR_RECORD_SEPARATOR}`;
}

export function unsupportedSignalrInvocation(sequence) {
  return `${JSON.stringify({ type: 1, invocationId: String(sequence), target: 'Upload', arguments: ['x'] })}${SIGNALR_RECORD_SEPARATOR}`;
}
