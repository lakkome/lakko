<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# Reproducible-build verification (GA-4)

A reproducible build is the SLSA L3 prerequisite: independent builders, given
the same source + pinned toolchain, must produce byte-identical artifacts, so a
compromised builder cannot slip a backdoor past the published hash.

## Pieces

- [`compare-build-hashes.sh`](compare-build-hashes.sh) — the single source of
  truth for the diff + the known-flaky filter. Used by every job in
  [`.github/workflows/repro.yml`](../../.github/workflows/repro.yml) AND the
  local verify below, so the comparison semantics live in exactly one place.
- `.github/workflows/repro.yml` — three advisory (`continue-on-error`) jobs:
  - **`repro`** — two clean release builds on the SAME runner; catches
    in-process non-determinism (timestamps, codegen randomness).
  - **`web-repro`** — the same for `apps/web/dist` (Vite + wasm-pack); no
    known-flaky artifact, so EVERY file must match.
  - **`cross-runner-build` + `cross-runner-verify`** — a build per ephemeral
    runner via a matrix, then a cross-runner diff; catches host-state leaks
    (kernel / libc / env), the SLSA L3 concern.

## Determinism knobs

Both the CI jobs and the local verify set:

- `RUSTFLAGS="--remap-path-prefix=<src>=. -C symbol-mangling-version=v0"` — strip
  the absolute source path out of debuginfo + use the stable v0 symbol mangling.
- `SOURCE_DATE_EPOCH=1735689600` (2025-01-01 UTC, fixed) — pin any timestamp a
  build step might embed.
- `CARGO_INCREMENTAL=0` — incremental artifacts are not reproducible.

## Known flaky

`liblakko_ffi_uniffi.rlib` — uniffi 0.28's proc-macros embed non-deterministic
state across independent builds (tracked upstream). It is the only filtered
artifact; every other `.rlib` + every `lakko-svc-*` binary must match
byte-for-byte. Override with `LAKKO_REPRO_KNOWN_FLAKY` (an extended-regex
alternation); pass an empty string to compare every artifact.

## Run it locally

A single crate's rlib (fast, isolates one artifact via a targeted clean):

```bash
export RUSTFLAGS="--remap-path-prefix=$PWD=. -C symbol-mangling-version=v0"
export SOURCE_DATE_EPOCH=1735689600 CARGO_INCREMENTAL=0

cargo build -p lakko-tuf --release --locked
(cd target/release && sha256sum deps/liblakko_tuf-*.rlib | sort) > /tmp/a.txt
cargo clean -p lakko-tuf --release
cargo build -p lakko-tuf --release --locked
(cd target/release && sha256sum deps/liblakko_tuf-*.rlib | sort) > /tmp/b.txt

LAKKO_REPRO_KNOWN_FLAKY="" ci/repro/compare-build-hashes.sh /tmp/a.txt /tmp/b.txt "lakko-tuf rlib"
```

The whole workspace (what the `repro` CI job does; ~20-25 min, needs a `cargo
clean` between the two builds):

```bash
export RUSTFLAGS="--remap-path-prefix=$PWD=. -C symbol-mangling-version=v0"
export SOURCE_DATE_EPOCH=1735689600 CARGO_INCREMENTAL=0
hash_release() { find target/release -maxdepth 1 -type f \
  \( -name 'lakko-svc-*' -o -name '*.rlib' \) | sort | xargs sha256sum | sort; }

cargo build --workspace --release --locked; hash_release > /tmp/a.txt
cargo clean
cargo build --workspace --release --locked; hash_release > /tmp/b.txt
ci/repro/compare-build-hashes.sh /tmp/a.txt /tmp/b.txt "release artifacts"
```

## Status

Single-artifact + the local mechanism are verified reproducible firsthand
(`lakko-tuf` rlib is byte-identical across two clean builds with the knobs
above). The full-workspace + cross-runner jobs stay `continue-on-error`
(advisory) until byte-identicality is stable workspace-wide — promote to gating
once the only diff is the known-flaky uniffi rlib. Tracked in
`~/.claude/plans/1-nested-wreath.md` §8.
