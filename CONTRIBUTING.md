# Contributing

We welcome contributions from the community. Please read the following guidelines carefully to maximize the chances of your PR being merged.

## Coding Style

- Format Rust changes with `cargo fmt --all`.
- Install hooks with `lefthook install` to auto-format staged Rust files on `git commit`.
- Run the workspace test suite with `cargo test --workspace`.

## Host-interface guardrails

The core crates are intentionally **not** a runtime-owned system layer.
Contributions should preserve that boundary:

- do not add built-in WASI shims or runtime-owned filesystem, network, clock,
  random, or process adapters to the core engine;
- prefer explicit embedder-supplied host imports and resolver / ACL policy over
  convenience wrappers that implicitly widen guest capabilities;
- treat fail-closed import resolution as the expected default posture when
  adding or extending host-interface surfaces;
- keep security-sensitive docs in sync when behavior changes:
  `README.md`, `SUPPORT_MATRIX.md`, and `THREAT_MODEL.md`.

If you need a concrete reference shape, start from
`examples/hello-host/README.md`, which demonstrates an explicit host import
without WASI.

## Platform-coupling guardrails

The core crates (`razero-wasm`, `razero-interp`, `razero-decoder`,
`razero-features`, `razero`) should not import directly from `razero-platform`.
Platform-specific functionality (mmap, signals, CPU detection) is intentionally
centralized behind `razero-platform`, and core crates access it only through
re-exports from crates that legitimately need it (`razero-compiler` for JIT,
`razero-secmem` for guard-page allocation).

When adding new platform-dependent behavior:

- do not add `use razero_platform::...` to `razero-wasm`, `razero-interp`,
  `razero-decoder`, or `razero-features`;
- do not add `std::fs`, `std::net`, `std::process`, `std::env`, or
  `std::path` imports to core crates in production code;
- if a core crate needs a platform type (e.g. `GuardPageError`), get it from
  `razero-secmem` rather than `razero-platform` directly;
- the `razero` crate's `filecache` module is gated behind the `filecache`
  Cargo feature and is the only core surface that performs filesystem I/O;
- the `razero-compiler` crate legitimately uses `razero-platform` for JIT
  codegen, mmap, and signal handling — this is expected and acceptable.

## Benchmarks

The manual benchmark workflow for Workstream 1 is anchored on
`razero/benches/secbench.rs`.

- Build or run the benchmark target with:
  - `cargo bench -p razero --bench secbench`
- The canonical roadmap baseline groups are:
  - `secbench/compile_time`
  - `secbench/execution_baseline`
  - `secbench/trap_overhead`
  - `secbench/memory_grow`
- Other `secbench/*` groups are still useful diagnostics, but they are
  supplemental to the roadmap baseline set.
- Some groups depend on the `fac-ssa` workload executing successfully in the
  current runtime. When that prerequisite is not met, the benchmark prints a
  skip message instead of reporting misleading numbers.
- To focus on one group while iterating, pass a Criterion filter after `--`, for
  example:
  - `cargo bench -p razero --bench secbench -- secbench/compile_time`
- For meaningful comparisons, run the same command on similar hardware and under
  comparable system load. Treat the results as comparative signals across
  revisions, not absolute pass/fail thresholds.

## DCO

We require DCO signoff line in every commit to this repo.

The sign-off is a simple line at the end of the explanation for the
patch, which certifies that you wrote it or otherwise have the right to
pass it on as an open-source patch. The rules are pretty simple: if you
can certify the below (from
[developercertificate.org](https://developercertificate.org/)):

```
Developer Certificate of Origin
Version 1.1
Copyright (C) 2004, 2006 The Linux Foundation and its contributors.
660 York Street, Suite 102,
San Francisco, CA 94110 USA
Everyone is permitted to copy and distribute verbatim copies of this
license document, but changing it is not allowed.
Developer's Certificate of Origin 1.1
By making a contribution to this project, I certify that:
(a) The contribution was created in whole or in part by me and I
    have the right to submit it under the open source license
    indicated in the file; or
(b) The contribution is based upon previous work that, to the best
    of my knowledge, is covered under an appropriate open source
    license and I have the right under that license to submit that
    work with modifications, whether created in whole or in part
    by me, under the same open source license (unless I am
    permitted to submit under a different license), as indicated
    in the file; or
(c) The contribution was provided directly to me by some other
    person who certified (a), (b) or (c) and I have not modified
    it.
(d) I understand and agree that this project and the contribution
    are public and that a record of the contribution (including all
    personal information I submit with it, including my sign-off) is
    maintained indefinitely and may be redistributed consistent with
    this project or the open source license(s) involved.
```

then you just add a line to every git commit message:

    Signed-off-by: Joe Smith <joe@gmail.com>

using your real name (sorry, no pseudonyms or anonymous contributions.)

You can add the sign off when creating the git commit via `git commit -s`.

## Code Reviews

* The pull request title should describe what the change does and not embed issue numbers.
The pull request should only be blank when the change is minor. Any feature should include
a description of the change and what motivated it. If the change or design changes through
review, please keep the title and description updated accordingly.
* A single approval is sufficient to merge. If a reviewer asks for
changes in a PR they should be addressed before the PR is merged,
even if another reviewer has already approved the PR.
* During the review, address the comments and commit the changes
_without_ squashing the commits. This facilitates incremental reviews
since the reviewer does not go through all the code again to find out
what has changed since the last review. When a change goes out of sync with main,
please rebase and force push, keeping the original commits where practical.
* Commits are squashed prior to merging a pull request, using the title
as commit message by default. Maintainers may request contributors to
edit the pull request tite to ensure that it remains descriptive as a
commit message. Alternatively, maintainers may change the commit message directly.
