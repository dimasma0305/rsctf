export const BLOB_OPERATION_HEADER = 'X-RSCTF-Operation-Id'

export interface BlobUploadOperation {
  fingerprint: string
  id: string
}

const fileFingerprint = (file: File): string => [file.name, file.size, file.lastModified, file.type].join(':')

/** Keep one identity for retries of the same selected file, and rotate it as
 * soon as the user selects a different request payload. */
export const retainBlobUploadOperation = (
  current: BlobUploadOperation | null,
  file: File,
  createId: () => string = () => crypto.randomUUID()
): BlobUploadOperation => {
  const fingerprint = fileFingerprint(file)
  return current?.fingerprint === fingerprint ? current : { fingerprint, id: createId() }
}
