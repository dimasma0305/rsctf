import assert from "node:assert/strict";
import {
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  symlinkSync,
  writeFileSync,
} from "node:fs";
import { execFileSync } from "node:child_process";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

import {
  OBSERVER_METADATA_SCHEMA_VERSION,
  assertObserverOutputOutsideRepository,
  createFreshObserverDirectory,
  gitWorktreeMetadata,
  readUntrackedWorktreeFiles,
} from "../observer-evidence.js";

const revision = "0123456789abcdef0123456789abcdef01234567";

test("creates one fresh observer directory and refuses to reuse it", async () => {
  const parent = mkdtempSync(join(tmpdir(), "rsctf-observer-evidence-test-"));
  const output = join(parent, "run");
  try {
    await createFreshObserverDirectory(output);
    const sentinel = join(output, "sentinel.txt");
    writeFileSync(sentinel, "first-run\n");
    await assert.rejects(() => createFreshObserverDirectory(output), /EEXIST/);
    assert.equal(readFileSync(sentinel, "utf8"), "first-run\n");
  } finally {
    rmSync(parent, { recursive: true, force: true });
  }
});

test("refuses observer output inside the repository, including through a symlink", async () => {
  const parent = mkdtempSync(join(tmpdir(), "rsctf-observer-output-test-"));
  const repository = join(parent, "repo");
  const outside = join(parent, "outside");
  mkdirSync(repository);
  mkdirSync(outside);
  symlinkSync(repository, join(outside, "repo-link"), "dir");
  try {
    await assert.rejects(
      () =>
        assertObserverOutputOutsideRepository(
          repository,
          join(repository, "evidence"),
        ),
      /outside the repository/,
    );
    await assert.rejects(
      () =>
        assertObserverOutputOutsideRepository(
          repository,
          join(outside, "repo-link", "evidence"),
        ),
      /outside the repository/,
    );
    await assert.doesNotReject(() =>
      assertObserverOutputOutsideRepository(
        repository,
        join(outside, "evidence"),
      ),
    );
  } finally {
    rmSync(parent, { recursive: true, force: true });
  }
});

test("records clean and dirty worktrees with content-sensitive fingerprints", () => {
  assert.equal(OBSERVER_METADATA_SCHEMA_VERSION, 5);
  const clean = gitWorktreeMetadata({ revision, status: "", trackedDiff: "" });
  assert.equal(clean.gitCommit, revision);
  assert.equal(clean.gitDirty, false);
  assert.equal(clean.gitStatusEntries, 0);
  assert.match(clean.gitWorktreeSha256, /^[a-f0-9]{64}$/);

  const dirty = gitWorktreeMetadata({
    revision,
    status: " M tests/load/observe.mjs\n?? tests/load/new-file.js",
    trackedDiff:
      "diff --git a/tests/load/observe.mjs b/tests/load/observe.mjs\n+changed\n",
    untrackedFiles: [
      { path: "tests/load/new-file.js", content: "export const model = 2;\n" },
    ],
  });
  assert.equal(dirty.gitDirty, true);
  assert.equal(dirty.gitStatusEntries, 2);
  assert.equal(dirty.gitTrackedEntries, 1);
  assert.equal(dirty.gitUntrackedEntries, 1);
  assert.equal(dirty.gitUntrackedContentBytes, 24);
  assert.deepEqual(
    dirty.gitUntrackedFiles.map(({ path }) => path),
    ["tests/load/new-file.js"],
  );
  assert.notEqual(dirty.gitWorktreeSha256, clean.gitWorktreeSha256);

  const differentContent = gitWorktreeMetadata({
    revision,
    status: " M tests/load/observe.mjs\n?? tests/load/new-file.js",
    trackedDiff:
      "diff --git a/tests/load/observe.mjs b/tests/load/observe.mjs\n+different\n",
    untrackedFiles: [
      { path: "tests/load/new-file.js", content: "export const model = 2;\n" },
    ],
  });
  assert.notEqual(differentContent.gitWorktreeSha256, dirty.gitWorktreeSha256);

  const differentUntrackedContent = gitWorktreeMetadata({
    revision,
    status: " M tests/load/observe.mjs\n?? tests/load/new-file.js",
    trackedDiff:
      "diff --git a/tests/load/observe.mjs b/tests/load/observe.mjs\n+changed\n",
    untrackedFiles: [
      { path: "tests/load/new-file.js", content: "export const model = 3;\n" },
    ],
  });
  assert.notEqual(
    differentUntrackedContent.gitUntrackedContentSha256,
    dirty.gitUntrackedContentSha256,
  );
  assert.notEqual(
    differentUntrackedContent.gitWorktreeSha256,
    dirty.gitWorktreeSha256,
  );

  const withoutTrailingWhitespace = gitWorktreeMetadata({
    revision,
    status: " M tests/load/observe.mjs",
    trackedDiff: Buffer.from("diff --git a/file b/file\n+changed\n"),
  });
  const withTrailingWhitespace = gitWorktreeMetadata({
    revision,
    status: " M tests/load/observe.mjs",
    trackedDiff: Buffer.from("diff --git a/file b/file\n+changed   \n"),
  });
  assert.notEqual(
    withTrailingWhitespace.gitTrackedDiffSha256,
    withoutTrailingWhitespace.gitTrackedDiffSha256,
  );
  assert.notEqual(
    withTrailingWhitespace.gitWorktreeSha256,
    withoutTrailingWhitespace.gitWorktreeSha256,
  );
});

test("fingerprints symlink blobs without following targets and bounds regular files", async () => {
  const parent = mkdtempSync(join(tmpdir(), "rsctf-observer-worktree-test-"));
  const repository = join(parent, "repo");
  const outside = join(parent, "outside-secret");
  mkdirSync(repository);
  writeFileSync(join(repository, "source.js"), "export const value = 1;\n");
  writeFileSync(outside, "first secret contents\n");
  symlinkSync("../outside-secret", join(repository, "outside-link"));
  try {
    const files = await readUntrackedWorktreeFiles(repository, [
      "source.js",
      "outside-link",
    ]);
    const link = files.find(({ path }) => path === "outside-link");
    assert.equal(link.kind, "symlink");
    assert.equal(Buffer.from(link.content).toString(), "../outside-secret");

    const first = gitWorktreeMetadata({
      revision,
      status: "?? source.js\n?? outside-link",
      trackedDiff: Buffer.alloc(0),
      untrackedFiles: files,
    });
    assert.deepEqual(
      first.gitUntrackedFiles.map(({ path, kind }) => ({ path, kind })),
      [
        { path: "outside-link", kind: "symlink" },
        { path: "source.js", kind: "file" },
      ],
    );

    writeFileSync(outside, "different target bytes that must not be read\n");
    const afterTargetChange = gitWorktreeMetadata({
      revision,
      status: "?? source.js\n?? outside-link",
      trackedDiff: Buffer.alloc(0),
      untrackedFiles: await readUntrackedWorktreeFiles(repository, [
        "source.js",
        "outside-link",
      ]),
    });
    assert.equal(
      afterTargetChange.gitUntrackedContentSha256,
      first.gitUntrackedContentSha256,
    );

    await assert.rejects(
      () =>
        readUntrackedWorktreeFiles(repository, ["source.js"], {
          maxFileBytes: 4,
        }),
      /exceeds 4 bytes/,
    );
    mkdirSync(join(repository, "special-directory"));
    await assert.rejects(
      () => readUntrackedWorktreeFiles(repository, ["special-directory"]),
      /refuses non-regular/,
    );
    const fifo = join(repository, "special-fifo");
    execFileSync("mkfifo", [fifo]);
    await assert.rejects(
      () => readUntrackedWorktreeFiles(repository, ["special-fifo"]),
      /refuses non-regular/,
    );
  } finally {
    rmSync(parent, { recursive: true, force: true });
  }
});

test("refuses ambiguous git metadata", () => {
  assert.throws(
    () =>
      gitWorktreeMetadata({
        revision: "not-a-commit",
        status: "",
        trackedDiff: "",
      }),
    /exact git revision/,
  );
  assert.throws(
    () => gitWorktreeMetadata({ revision, status: null, trackedDiff: "" }),
    /status must be text/,
  );
  assert.throws(
    () =>
      gitWorktreeMetadata({
        revision,
        status: "?? tests/load/model-v2.js",
        trackedDiff: "",
      }),
    /incomplete \(0\/1\)/,
  );
  assert.throws(
    () =>
      gitWorktreeMetadata({
        revision,
        status: "?? tests/load/model-v2.js",
        trackedDiff: "",
        untrackedFiles: [
          { path: "tests/load/model-v2.js", content: "one" },
          { path: "tests/load/model-v2.js", content: "two" },
        ],
      }),
    /duplicate path/,
  );
});
