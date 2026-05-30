<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# `apps/shared/webrtc-cpp-headers/` — vendored libwebrtc public C++ headers

The native SFrame shims under
[`apps/shared/sframe-bridge/`](../sframe-bridge/) (N-3),
[`apps/ios/.../SFrameAttach.mm`](../../ios/) (N-4), and
[`apps/android/lakko-sframe/.../sframe_jni.cc`](../../android/) (N-5)
subclass `webrtc::FrameEncryptorInterface` and
`webrtc::FrameDecryptorInterface` and hand the resulting pointer
back to libwebrtc's runtime through the non-public attach points
documented in [`docs/security/known-gaps.md`](../../../docs/security/known-gaps.md)
G-28. To compile those subclasses we need the libwebrtc public
C++ headers — and the headers MUST match the binary the
platform's WebRTC artefact ships, or the vtable layout the
compiler emits will not line up with what libwebrtc's runtime
expects at the JNI / Obj-C++ boundary. A mismatched vtable is a
silent C++ ABI break: the call dispatches to the wrong member
function, returns garbage, and the call itself "works" — exactly
the kind of failure mode the audit pack must not let through.

## Why vendor instead of fetching at build time?

Three reasons:

1. **Reproducibility.** The headers are part of the cryptographic
   trust boundary (they declare the vtable the AEAD path
   delivers ciphertext through). Reproducible builds (`GA-4`)
   need them committed to the tree, not pulled at build time
   from a network endpoint that could be poisoned, redirected,
   or yanked.
2. **Offline builds.** The maintainer must be able to build the
   workspace on a censored / air-gapped network. A build-time
   `curl` is incompatible with that posture.
3. **Pin-to-binary.** The headers are pinned to **chromium /
   external / webrtc, M125 (branch-heads/6422)** — the exact
   milestone stasel/WebRTC 125.0.0 and io.github.webrtc-sdk:android
   125.6422.07 statically link. A vendored copy makes that
   pin loud at PR-review time.

## Layout

```
apps/shared/webrtc-cpp-headers/
├── README.md            — this file
├── M125/                — version-pinned subset (current)
│   ├── LICENSE          — BSD-3-Clause, copied verbatim from upstream
│   ├── SHA256SUMS       — pinned hash for every vendored header
│   ├── fetch.sh         — fetch + verify script (the only sanctioned
│   │                      way to populate `include/`)
│   └── include/         — the headers themselves (gitignored;
│                          populated by `fetch.sh`)
└── .gitignore           — keeps fetched `*.h` out of the tree by
                           default; `SHA256SUMS` is the single source
                           of truth at commit time
```

## Why `include/` is gitignored

A subtle posture choice — committing the headers would make
review easier (you see the diff when the milestone bumps), but
it would also let a contributor silently edit a vendored header
and bypass the upstream-truth invariant. Gitignoring `include/`
and committing only `SHA256SUMS` means:

- The only sanctioned way to populate `include/` is `fetch.sh`,
  which downloads from the pinned upstream URL and refuses to
  write a file whose sha256 does not match the committed pin.
- A bump of the milestone is a deliberate two-step PR: update
  `SHA256SUMS` (committed) + re-run `fetch.sh` locally. The
  `SHA256SUMS` diff is what reviewers audit.

## Adding a new vendored header

1. Add the file path (relative to `webrtc/src/`) to `fetch.sh`'s
   `FILES` array.
2. Re-run `fetch.sh`. It will download, leave the file in
   `include/`, and print a fresh `sha256sum` line you copy into
   `SHA256SUMS`.
3. Commit `SHA256SUMS` + the `fetch.sh` change. Do NOT commit
   the `.h` (gitignored).

## Milestone bumps

When stasel/WebRTC or io.github.webrtc-sdk:android bumps to a
new M-line:

1. Create a sibling directory `MNNN/` (e.g., `M137/`) with the
   same shape — `LICENSE` + `SHA256SUMS` + `fetch.sh` (with
   a different upstream tag) + empty `include/`.
2. Update [`apps/shared/sframe-bridge/CMakeLists.txt`](../sframe-bridge/CMakeLists.txt)'s
   `WEBRTC_HEADERS_PATH` symlink target.
3. Keep the old `MNNN/` directory around until the next release
   train has shipped to all clients — a downgrade is then a
   one-symlink revert.

## License

Every vendored header is BSD-3-Clause, copyright Google /
The WebRTC project authors (see `MNNN/LICENSE`). Lakko's
AGPLv3 compatibility with BSD-3-Clause is documented in
[`docs/adr/0006-licensing.md`](../../../docs/adr/0006-licensing.md).
