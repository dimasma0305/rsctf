import { createHash } from "node:crypto";
import { constants as fsConstants } from "node:fs";
import { lstat, mkdir, open, readlink, realpath } from "node:fs/promises";
import { basename, dirname, isAbsolute, join, relative, resolve, sep } from "node:path";

export const OBSERVER_METADATA_SCHEMA_VERSION = 5;
export const MAX_UNTRACKED_FILE_BYTES = 8 * 1024 * 1024;
export const MAX_UNTRACKED_TOTAL_BYTES = 64 * 1024 * 1024;
export const MAX_UNTRACKED_FILES = 4_096;
const UNTRACKED_READ_CONCURRENCY = 8;

function sha256(value) {
  return createHash("sha256").update(value).digest("hex");
}

function pathIsWithin(parent, candidate) {
  const child = relative(parent, candidate);
  return (
    child === "" ||
    (!isAbsolute(child) && child !== ".." && !child.startsWith(`..${sep}`))
  );
}

/** Refuse evidence output that can become part of the worktree being fingerprinted. */
export async function assertObserverOutputOutsideRepository(repository, directory) {
  if (
    typeof repository !== "string" ||
    !isAbsolute(repository) ||
    typeof directory !== "string" ||
    !isAbsolute(directory)
  ) {
    throw new Error("observer repository and output directory must be absolute paths");
  }

  const lexicalRepository = resolve(repository);
  const lexicalOutput = resolve(directory);
  if (pathIsWithin(lexicalRepository, lexicalOutput)) {
    throw new Error("observer output directory must be outside the repository");
  }
  const [physicalRepository, physicalOutputParent] = await Promise.all([
    realpath(lexicalRepository),
    realpath(dirname(lexicalOutput)),
  ]);
  const physicalOutput = resolve(physicalOutputParent, basename(lexicalOutput));
  if (pathIsWithin(physicalRepository, physicalOutput)) {
    throw new Error("observer output directory must be outside the repository");
  }
  return directory;
}

export async function createFreshObserverDirectory(directory) {
  if (typeof directory !== "string" || !isAbsolute(directory)) {
    throw new Error("observer output directory must be an absolute path");
  }
  // recursive:false makes directory ownership atomic. EEXIST is intentional:
  // evidence from two runs must never share CSV or metadata files.
  await mkdir(directory, { recursive: false, mode: 0o750 });
  return directory;
}

function validateRelativePath(path) {
  if (
    typeof path !== "string" ||
    path.length === 0 ||
    path.includes("\0") ||
    isAbsolute(path) ||
    path.split("/").includes("..")
  ) {
    throw new Error("observer untracked-file evidence contains an invalid path");
  }
  return path;
}

async function mapConcurrent(values, concurrency, task) {
  const results = new Array(values.length);
  let nextIndex = 0;
  await Promise.all(
    Array.from({ length: Math.min(concurrency, values.length) }, async () => {
      while (nextIndex < values.length) {
        const index = nextIndex++;
        results[index] = await task(values[index], index);
      }
    }),
  );
  return results;
}

async function readStableRegularFile(path, expected) {
  let handle;
  try {
    handle = await open(path, fsConstants.O_RDONLY | fsConstants.O_NOFOLLOW);
    const before = await handle.stat({ bigint: true });
    if (
      !before.isFile() ||
      before.size !== expected.size ||
      before.mtimeNs !== expected.mtimeNs ||
      before.ino !== expected.ino ||
      before.dev !== expected.dev
    ) {
      throw new Error(`observer untracked file changed while being fingerprinted: ${expected.path}`);
    }
    const content = Buffer.alloc(Number(before.size));
    let offset = 0;
    while (offset < content.length) {
      const { bytesRead } = await handle.read(content, offset, content.length - offset, offset);
      if (bytesRead === 0) break;
      offset += bytesRead;
    }
    const after = await handle.stat({ bigint: true });
    if (
      offset !== content.length ||
      after.size !== before.size ||
      after.mtimeNs !== before.mtimeNs ||
      after.ino !== before.ino ||
      after.dev !== before.dev
    ) {
      throw new Error(`observer untracked file changed while being fingerprinted: ${expected.path}`);
    }
    return content;
  } finally {
    await handle?.close();
  }
}

async function readStableSymlink(path, expected) {
  const content = await readlink(path, { encoding: "buffer" });
  const after = await lstat(path, { bigint: true });
  if (
    content.byteLength !== Number(expected.size) ||
    !after.isSymbolicLink() ||
    after.size !== expected.size ||
    after.mtimeNs !== expected.mtimeNs ||
    after.ino !== expected.ino ||
    after.dev !== expected.dev
  ) {
    throw new Error(`observer untracked symlink changed while being fingerprinted: ${expected.path}`);
  }
  return content;
}

/**
 * Read the exact Git-blob bytes for untracked regular files and symlinks.
 * Special files are rejected, and both individual and aggregate reads are
 * bounded so an observer cannot follow a device/FIFO or exhaust host memory.
 */
export async function readUntrackedWorktreeFiles(
  repository,
  paths,
  {
    maxFileBytes = MAX_UNTRACKED_FILE_BYTES,
    maxTotalBytes = MAX_UNTRACKED_TOTAL_BYTES,
    maxFiles = MAX_UNTRACKED_FILES,
    concurrency = UNTRACKED_READ_CONCURRENCY,
  } = {},
) {
  const root = resolve(repository);
  if (!isAbsolute(repository) || !Array.isArray(paths)) {
    throw new Error("observer untracked-file reader requires an absolute repository and path array");
  }
  for (const [value, label] of [
    [maxFileBytes, "per-file byte limit"],
    [maxTotalBytes, "total byte limit"],
    [maxFiles, "file-count limit"],
    [concurrency, "read concurrency"],
  ]) {
    if (!Number.isSafeInteger(value) || value < 1) {
      throw new Error(`observer untracked ${label} must be a positive safe integer`);
    }
  }
  if (paths.length > maxFiles) {
    throw new Error(`observer untracked-file count exceeds ${maxFiles}`);
  }
  const unique = new Set();
  const normalized = paths.map((path) => {
    const relative = validateRelativePath(path);
    if (unique.has(relative)) {
      throw new Error(`observer untracked-file evidence contains duplicate path ${relative}`);
    }
    unique.add(relative);
    const absolute = resolve(join(root, relative));
    if (absolute !== root && !absolute.startsWith(`${root}/`)) {
      throw new Error("observer untracked-file evidence escapes the repository");
    }
    return { path: relative, absolute };
  });

  const descriptors = await mapConcurrent(normalized, concurrency, async ({ path, absolute }) => {
    const stats = await lstat(absolute, { bigint: true });
    const kind = stats.isFile() ? "file" : stats.isSymbolicLink() ? "symlink" : null;
    if (!kind) {
      throw new Error(`observer refuses non-regular untracked path ${path}`);
    }
    const size = Number(stats.size);
    if (!Number.isSafeInteger(size) || size < 0 || size > maxFileBytes) {
      throw new Error(`observer untracked file ${path} exceeds ${maxFileBytes} bytes`);
    }
    return {
      path,
      absolute,
      kind,
      size: stats.size,
      mtimeNs: stats.mtimeNs,
      ino: stats.ino,
      dev: stats.dev,
    };
  });
  const totalBytes = descriptors.reduce((total, descriptor) => total + Number(descriptor.size), 0);
  if (!Number.isSafeInteger(totalBytes) || totalBytes > maxTotalBytes) {
    throw new Error(`observer untracked-file bytes exceed ${maxTotalBytes}`);
  }

  return mapConcurrent(descriptors, concurrency, async (descriptor) => ({
    path: descriptor.path,
    kind: descriptor.kind,
    content:
      descriptor.kind === "symlink"
        ? await readStableSymlink(descriptor.absolute, descriptor)
        : await readStableRegularFile(descriptor.absolute, descriptor),
  }));
}

function untrackedContentManifest(files) {
  if (!Array.isArray(files)) {
    throw new Error("observer untracked-file evidence must be an array");
  }

  const paths = new Set();
  const entries = files
    .map(({ path, kind = "file", content }) => {
      validateRelativePath(path);
      if (kind !== "file" && kind !== "symlink") {
        throw new Error(`observer untracked-file evidence contains an invalid kind for ${path}`);
      }
      if (!(typeof content === "string" || content instanceof Uint8Array)) {
        throw new Error(`observer could not fingerprint untracked file ${path}`);
      }
      if (paths.has(path)) {
        throw new Error(`observer untracked-file evidence contains duplicate path ${path}`);
      }
      paths.add(path);
      const bytes = Buffer.from(content);
      return Object.freeze({
        path,
        kind,
        bytes: bytes.byteLength,
        sha256: sha256(bytes),
      });
    })
    .sort((left, right) => (left.path < right.path ? -1 : left.path > right.path ? 1 : 0));
  const manifest = entries
    .map(
      ({ path, kind, bytes, sha256: contentSha256 }) =>
        `${path.length}:${path}\0${kind}\0${bytes}:${contentSha256}`,
    )
    .join("\n");

  return Object.freeze({
    entries: Object.freeze(entries),
    bytes: entries.reduce((total, entry) => total + entry.bytes, 0),
    sha256: sha256(manifest),
  });
}

export function gitWorktreeMetadata({ revision, status, trackedDiff, untrackedFiles = [] }) {
  if (typeof revision !== "string" || !/^[a-f0-9]{40,64}$/i.test(revision.trim())) {
    throw new Error("observer could not determine an exact git revision");
  }
  if (
    typeof status !== "string" ||
    !(typeof trackedDiff === "string" || trackedDiff instanceof Uint8Array)
  ) {
    throw new Error("observer git status must be text and diff evidence must be bytes");
  }

  const gitCommit = revision.trim().toLowerCase();
  const normalizedStatus = status.replaceAll("\r\n", "\n").trim();
  const rawDiff = Buffer.from(trackedDiff);
  const entries = normalizedStatus ? normalizedStatus.split("\n").filter(Boolean) : [];
  const untrackedEntries = entries.filter((entry) => entry.startsWith("?? ")).length;
  const trackedEntries = entries.length - untrackedEntries;
  const untracked = untrackedContentManifest(untrackedFiles);
  if (untracked.entries.length !== untrackedEntries) {
    throw new Error(
      `observer untracked-file evidence is incomplete (${untracked.entries.length}/${untrackedEntries})`,
    );
  }
  const gitDirty = entries.length > 0 || rawDiff.byteLength > 0;
  const worktreeHasher = createHash("sha256");
  worktreeHasher.update(gitCommit);
  worktreeHasher.update("\0");
  worktreeHasher.update(normalizedStatus);
  worktreeHasher.update("\0");
  worktreeHasher.update(rawDiff);
  worktreeHasher.update("\0");
  worktreeHasher.update(untracked.sha256);

  return Object.freeze({
    gitCommit,
    gitDirty,
    gitStatusEntries: entries.length,
    gitTrackedEntries: trackedEntries,
    gitUntrackedEntries: untrackedEntries,
    gitStatusSha256: sha256(normalizedStatus),
    gitTrackedDiffSha256: sha256(rawDiff),
    gitUntrackedContentBytes: untracked.bytes,
    gitUntrackedContentSha256: untracked.sha256,
    gitUntrackedFiles: untracked.entries,
    gitWorktreeSha256: worktreeHasher.digest("hex"),
  });
}
