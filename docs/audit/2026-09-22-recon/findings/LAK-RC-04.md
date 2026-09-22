<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# LAK-RC-04 — All of the project's unsafe code lives in the one crate its unsafe policy does not cover, and that crate is not fuzzed

- **Severity:** Medium (finder's proposal; `audit-verify` calibrates)
- **Confidence:** CONFIRMED (for the policy inversion and the absent fuzzing);
  no memory-safety defect is claimed — see Impact
- **Workstream:** recon
- **Tracking id:** LAK-RC-04
- **Status:** Open
- **Component:** `crates/lakko-sframe-native/src/lib.rs:57-71`,
  `crates/lakko-ffi-uniffi/src/lib.rs:28`, `crates/lakko-ffi-wasm/src/lib.rs:68`
- **Invariants touched:** the `CLAUDE.md` "Code-style rules" unsafe policy
- **Vendor claim contradicted:** `CLAUDE.md`: *"`#![forbid(unsafe_code)]` on every
  crate, except `lakko-ffi-uniffi` and `lakko-ffi-wasm` (which carry justified
  unsafe — document it with a `// SAFETY:` comment)."*

## Preconditions

None for the policy gap. For any latent defect to be reachable, an attacker would
need to influence bytes arriving on the media path — which the SFU, by design, is
assumed to be able to do.

## Mechanism

Measured across `crates/*/src` and `services/*/src`:

| Crate | Attribute | site | `unsafe {` blocks |
|---|---|---|---:|
| `lakko-ffi-uniffi` | `#![forbid(unsafe_code)]` | `src/lib.rs:28` | 0 |
| `lakko-ffi-wasm` | `#![forbid(unsafe_code)]` | `src/lib.rs:68` | 0 |
| `lakko-sframe-native` | `#![allow(unsafe_code)]` | `src/lib.rs:70` | **98** |
| `services/obfs-proxy` | *(no attribute)* | — | 0 |

The two crates the policy names as exceptions contain no unsafe at all. The crate
that contains every unsafe block in the project is not named. Its own header is
candid about why (`src/lib.rs:57-58`):

```rust
//! The crate carries `#![forbid(unsafe_code)]` would block the
//! `extern "C"` boundary, so we relax to `#![deny(unsafe_op_in_unsafe_fn)]`
```

— which is the correct engineering decision for a C ABI, and is exactly the
decision the policy statement fails to record.

Further measurements on that file: 19 `pub unsafe extern "C" fn`, and **38
`// SAFETY:` comments against 98 `unsafe {` blocks**.

And it has **no fuzz target**: `ls crates/lakko-sframe-native/fuzz` does not
exist, while 69 targets exist across the rest of the workspace. The functions
without coverage are the ones that take `*const u8` / `*mut u8` plus a length from
a C++ caller and reconstruct slices — the entry point for every SFrame frame
arriving from an untrusted SFU on iOS and Android.

## Reproduction

```
grep -n 'forbid(unsafe_code)\|allow(unsafe_code)' crates/lakko-*/src/lib.rs
grep -c 'unsafe {'  crates/lakko-sframe-native/src/lib.rs   # 98
grep -c 'SAFETY:'   crates/lakko-sframe-native/src/lib.rs   # 38
ls crates/lakko-sframe-native/fuzz                          # no such directory
```

## Impact

**I am not claiming a memory-safety defect.** I did not read the 98 blocks, and
this engagement does not run the implementation workstream. What I am claiming is
that the project's own stated control does not cover the code it exists for, and
that the uncovered code is the highest-consequence unsafe surface in the system:

- it is the only place raw pointers cross a language boundary;
- it sits under the C++ bridge, the Obj-C++ shim and the JNI module, i.e. three
  further layers of manual lifetime management;
- every frame on that path originates with a relay the threat model assumes is
  fully compromised;
- a memory-safety defect there is reachable remotely and is, by the severity
  rubric, Catastrophic-class impact if one exists.

The 38:98 comment ratio is a ratio, not a count of undocumented blocks — one
`// SAFETY:` can legitimately govern several blocks in a function. It is offered
as a signal that the documentation requirement in the same policy sentence has
not been applied uniformly, not as an assertion that 60 blocks are unjustified.

The policy inversion also has a second-order cost that is the real reason this is
filed rather than treated as a doc nit: a reviewer, a new contributor, or a future
audit reading that sentence concludes the project has two small, known unsafe
surfaces at the binding layer. It has one, elsewhere, five times the size, at the
media layer.

## Recommendation

1. **Correct the policy sentence** to name `lakko-sframe-native` as the exception
   and to state why (`extern "C"` cannot coexist with `forbid`), and to record
   that the two FFI crates it currently names are in fact `forbid`-clean. Add
   `#![forbid(unsafe_code)]` to `services/obfs-proxy`, which has no unsafe and is
   simply missing the attribute.
2. **Add a fuzz target for the C ABI.** The entry points take
   `(*const u8, usize)` pairs and an opaque handle; a harness that drives
   `lakko_sframe_*_open_framed` with arbitrary bytes and arbitrary lengths against
   a fixed key is cheap and directly exercises the slice reconstruction. This is
   the single highest-value fuzz target the project does not have.
3. **Audit the 98 blocks for `// SAFETY:` coverage** as a mechanical pass, so the
   ratio becomes a count.

## Notes

- `lakko-sframe-native` does carry `#![deny(unsafe_op_in_unsafe_fn)]`
  (`src/lib.rs:71`), which is the right lint and materially better than nothing —
  it forces every operation inside an `unsafe fn` into an explicit block rather
  than inheriting the function's unsafety. That is partly why 98 blocks exist for
  19 functions, and it should be read as discipline rather than as sprawl.
- The crate has 10 unit tests, 6 integration tests and 3 cross-stack golden tests
  per `CLAUDE.md`; I did not run them separately from the workspace total.
- `apps/shared/sframe-bridge/*.cc` (1,775 lines of C++ across `apps/shared`) is
  the layer above and has its own manual refcounting. It is outside this
  engagement entirely and no claim is made about it.
