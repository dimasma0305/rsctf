import assert from "node:assert/strict";
import test from "node:test";

import {
  assertImmutableBuildRecord,
  assertSuccessfulBuildResponse,
  isImmutableImageReference,
  kothContainerOverride,
} from "../fixture-image-config.js";

const localImage =
  "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const repositoryDigest =
  "registry.example/ctf/hill@sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

test("KotH image overrides require one immutable image and exact port pair", () => {
  assert.equal(kothContainerOverride({}), null);
  assert.deepEqual(
    kothContainerOverride({
      KOTH_CONTAINER_IMAGE: localImage,
      KOTH_CONTAINER_PORT: "8080",
    }),
    { image: localImage, port: 8080 },
  );
  assert.deepEqual(
    kothContainerOverride({
      KOTH_CONTAINER_IMAGE: repositoryDigest,
      KOTH_CONTAINER_PORT: "443",
    }),
    { image: repositoryDigest, port: 443 },
  );
});

test("KotH image overrides reject mutable, partial, and invalid definitions", () => {
  assert.equal(isImmutableImageReference(localImage), true);
  assert.equal(isImmutableImageReference(repositoryDigest), true);
  assert.equal(isImmutableImageReference("nginx:latest"), false);

  assert.throws(
    () => kothContainerOverride({ KOTH_CONTAINER_IMAGE: localImage }),
    /must be set together/,
  );
  assert.throws(
    () =>
      kothContainerOverride({
        KOTH_CONTAINER_IMAGE: "nginx:latest",
        KOTH_CONTAINER_PORT: "80",
      }),
    /repository digest or Docker image ID/,
  );
  for (const port of ["0", "65536", "80.5", " 80", ""]) {
    assert.throws(
      () =>
        kothContainerOverride({
          KOTH_CONTAINER_IMAGE: localImage,
          KOTH_CONTAINER_PORT: port,
        }),
      /integer from 1 through 65535/,
    );
  }
});

test("immutable rebuild acceptance requires both Success and its durable digest", () => {
  assert.doesNotThrow(() =>
    assertSuccessfulBuildResponse({ buildStatus: "Success" }, "KotH"),
  );
  assert.equal(
    assertImmutableBuildRecord(
      {
        containerImage: localImage,
        buildStatus: 1,
        buildImageDigest: localImage,
      },
      localImage,
      "KotH",
    ),
    localImage,
  );
  assert.throws(
    () =>
      assertSuccessfulBuildResponse(
        { buildStatus: "Failed", lastBuildLog: "inspect failed" },
        "KotH",
      ),
    /inspect failed/,
  );
  assert.throws(
    () =>
      assertImmutableBuildRecord(
        {
          containerImage: localImage,
          buildStatus: 1,
          buildImageDigest: "nginx:latest",
        },
        localImage,
        "KotH",
      ),
    /immutable image digest/,
  );
  assert.throws(
    () =>
      assertImmutableBuildRecord(
        {
          containerImage: repositoryDigest,
          buildStatus: 1,
          buildImageDigest: repositoryDigest,
        },
        localImage,
        "KotH",
      ),
    /different image definition/,
  );
});
