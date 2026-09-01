# Fast local development

## One integration owner

Choose one integration worktree for the active batch. Only its owner may stage or
commit there. Mutating agents use dedicated worktrees or return an unstaged patch;
read-only agents may inspect the integration worktree. This prevents partial commits,
overlapping resets, and fixes that exist only in an abandoned worktree.

Before integrating a batch, audit sibling worktrees once with `git worktree list
--porcelain`, status, unique commits, and patch-equivalence checks. Classify each
candidate as already integrated/stale, dirty and needing recovery, or genuinely
missing. Do not repeatedly rescan unchanged worktrees during the same batch.

## Local-first feedback loop

Use GitHub Actions as the final clean-room gate, not as the development debugger:

1. Reproduce with the smallest focused local test or exact failing harness phase.
2. Implement a cohesive batch and keep a short failure ledger. Repair related
   failures together instead of rebuilding after every small edit.
3. Run static checks and focused regressions while the source is changing. Once the
   batch settles, run the applicable broader Rust, frontend, database, container,
   browser, accessibility, and load checks exactly once.
4. When behavior depends on the deployed proxy, browser, worker, or multi-host
   topology, preview the exact integration worktree once on `https://dev.1pc.tf`.
   Keep development data and resources isolated from production and clean up
   disposable resources after verification.
5. Inspect the staged diff and batch commits before the first push. Push the locally
   green batch once, then use CI and the immutable release pipeline as the final gate.

If CI fails, reproduce its failure locally before another code push. A remote rerun
without a code change is appropriate only for a documented runner or external-service
failure.

## Keep the host responsive

Use only `scripts/bounded-cargo.sh` and `scripts/bounded-frontend.sh`; they share one
cross-worktree compile slot and enforce CPU and memory ceilings. Do not launch raw or
parallel builds from other worktrees. Prefer focused test filters during iteration and
reuse the shared target/cache rather than producing one target directory per worktree.

Do not run a full Rust and frontend gate after each integration commit. A batch may
have small local checkpoint commits, but consolidate and review it before pushing.
Clean only verified disposable schemas, labeled test containers, temporary build
directories, and other generated resources; never infer that unrelated worktree data
is safe to remove.
