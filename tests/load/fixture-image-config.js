const LOCAL_IMAGE_ID = /^sha256:[0-9a-f]{64}$/i;

export function isImmutableImageReference(value) {
  if (typeof value !== "string" || value !== value.trim()) return false;
  if (LOCAL_IMAGE_ID.test(value)) return true;
  const separator = value.lastIndexOf("@");
  if (
    separator <= 0 ||
    separator !== value.indexOf("@") ||
    /\s/.test(value)
  ) {
    return false;
  }
  return LOCAL_IMAGE_ID.test(value.slice(separator + 1));
}

function requiredPort(value) {
  if (typeof value !== "string" || !/^[1-9][0-9]*$/.test(value)) {
    throw new Error("KOTH_CONTAINER_PORT must be an integer from 1 through 65535");
  }
  const port = Number(value);
  if (!Number.isSafeInteger(port) || port > 65_535) {
    throw new Error("KOTH_CONTAINER_PORT must be an integer from 1 through 65535");
  }
  return port;
}

/** Optional capacity-mode KotH image/port pair. Both values are one contract. */
export function kothContainerOverride(environment = process.env) {
  const image = environment.KOTH_CONTAINER_IMAGE;
  const port = environment.KOTH_CONTAINER_PORT;
  if (image === undefined && port === undefined) return null;
  if (image === undefined || port === undefined) {
    throw new Error(
      "KOTH_CONTAINER_IMAGE and KOTH_CONTAINER_PORT must be set together",
    );
  }
  if (!isImmutableImageReference(image)) {
    throw new Error(
      "KOTH_CONTAINER_IMAGE must be a repository digest or Docker image ID",
    );
  }
  return Object.freeze({ image, port: requiredPort(port) });
}

export function assertSuccessfulBuildResponse(model, label) {
  if (model?.buildStatus !== "Success") {
    const diagnostic =
      typeof model?.lastBuildLog === "string" ? `: ${model.lastBuildLog}` : "";
    throw new Error(`${label} immutable rebuild did not succeed${diagnostic}`);
  }
}

/** Validate the durable record after the synchronous rebuild response. */
export function assertImmutableBuildRecord(record, requestedImage, label) {
  const requested =
    typeof requestedImage === "string" ? requestedImage.trim() : "";
  if (!requested || record?.containerImage !== requested) {
    throw new Error(`${label} rebuild persisted a different image definition`);
  }
  if (Number(record?.buildStatus) !== 1) {
    throw new Error(`${label} rebuild did not persist Success`);
  }
  if (!isImmutableImageReference(record?.buildImageDigest)) {
    throw new Error(`${label} rebuild did not persist an immutable image digest`);
  }
  return record.buildImageDigest;
}
