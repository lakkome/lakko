<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# LAK-RC-05 — About 5,600 lines of Rust have no production caller, and 2,819 of them ship inside the wasm binary delivered to browsers

- **Severity:** Low (finder's proposal; `audit-verify` calibrates)
- **Confidence:** CONFIRMED
- **Workstream:** recon
- **Tracking id:** LAK-RC-05
- **Status:** Open
- **Component:** `crates/lakko-tuf`, `crates/lakko-presence`, `crates/lakko-sync`,
  `crates/lakko-storage`, `crates/lakko-core`
- **Invariants touched:** #5 indirectly — see the `lakko-presence` case
- **Vendor claim contradicted:** `docs/security/threat-model.md` asset table and
  invariant #5, both of which cite *"presence = blinded per-conversation tokens"*
  in the present tense as an active protection for the social graph; and the
  coerced-maintainer row, whose release-integrity story would be `lakko-tuf`'s job

## Preconditions

None — this is a property of the tree.

## Mechanism

Reverse dependencies, measured with `cargo tree -i <crate> --workspace`:

| Crate | LOC | Reverse deps | Reachable from client code? |
|---|---:|---|---|
| `lakko-tuf` | 3,211 | **none** | no — linked into nothing |
| `lakko-storage` | 1,357 | `lakko-ffi-wasm` only | no |
| `lakko-sync` | 1,035 | `lakko-ffi-wasm` only | no |
| `lakko-presence` | 427 | `lakko-ffi-wasm` only | no |
| `lakko-core` | 12 | **none** | no |

`lakko-tuf`'s only occurrences outside its own tree are a comment in
`crates/lakko-transport/Cargo.toml:79` and a doc reference in
`crates/lakko-transport/src/doh_fetch.rs:343`. Neither is a dependency.

For the three wasm-reachable crates, grepping `apps/web/src` (excluding
`*.test.ts`) for their exported symbols — `PresenceToken`, `deriveToken`,
`PresenceTracker`, `HybridLogicalClock`, `EditChain`, `EncryptedStore`, `Store` —
returns three hits, all of which are prose:

```
apps/web/src/wasm.ts:11            * EditChain, ContactRequest, …) are usable as regular JS classes.
apps/web/src/auth/device-key.ts:92 //    IDB-backed `EncryptedStore` (1.13c).
apps/web/src/routes/Chat.tsx:30    * `EditChain` LWW for per-message drafts (the lakko-sync
```

Zero call sites. The same greps against `apps/ios/Lakko` and
`apps/android/app/src/main` return zero, which follows — none of the three is a
dependency of `lakko-ffi-uniffi`.

Because the wasm bindings are `#[wasm_bindgen]` public exports, they are retained
rather than tree-shaken, so `lakko-presence` + `lakko-sync` + `lakko-storage`
(2,819 lines) compile into the wasm artefact every browser downloads.

## Reproduction

```
for c in lakko-tuf lakko-core lakko-presence lakko-sync lakko-storage; do
  echo "== $c"; cargo tree -i "$c" --workspace | tail -n +2 | grep -oE 'lakko-[a-z-]+' | sort -u
done
grep -rn 'PresenceToken\|deriveToken\|PresenceTracker\|HybridLogicalClock' \
  apps/web/src --include='*.ts' --include='*.tsx' | grep -v '\.test\.'
```

## Impact

Three distinct consequences, of which the second is the one that makes this a
finding rather than an observation.

**Unreviewed surface presented as reviewed.** 5,600 lines carry SPDX headers,
tests and — in `lakko-tuf`'s case — five fuzz targets, and appear in `CLAUDE.md`'s
module map alongside load-bearing crates. A reader, including the next auditor,
reasonably allocates attention to them. `lakko-tuf` in particular is 3,211 lines
of TUF metadata verification with a delegation walk and a fuzz harness, all of
which look exactly like a live control and none of which runs.

**A documented mitigation that does not exist.** `docs/security/threat-model.md`
lists the social graph's protection as *"Add-contact = encrypted envelope to a
per-recipient pseudonym; presence = blinded per-conversation tokens"*, repeats
*"presence = blinded per-conversation tokens"* in invariant #5, and cites
*"presence blinded per-conversation"* again in the mass-correlation adversary row.
`lakko-presence` is wired to nothing on any platform, and its module doc
(`crates/lakko-presence/src/lib.rs:29-30`, `:55`) says its `presence_seed` is to
come from an MLS exporter labelled `"lakko/presence/v1"` — which does not exist in
`lakko-mls`. So one third of the stated social-graph defence is a crate with no
caller and no key source. The correct present-tense statement is that Lakko has
**no presence feature at all**, which is a stronger privacy position than the one
claimed but a different one.

Likewise `lakko-tuf`: the coerced-maintainer row's *"a backdoored binary cannot
ride the release channel without forking the public hash record"* has no verifier
in any shipped artefact.

**Payload and attack surface.** 2,819 lines of unused Rust in the browser bundle
is a download cost and an exported-API surface reachable from any script in the
page. Not an attack in itself — but it is surface that no threat model covers
because nobody believes it is there.

## Recommendation

- **Remove `lakko-presence`, `lakko-sync` and `lakko-storage` from
  `lakko-ffi-wasm`'s exports** until something calls them. They are recoverable
  from git; leaving them exported ships them.
- **Correct the threat model.** Delete the three present-tense references to
  blinded presence tokens, or mark them as planned. As above, the honest statement
  ("no presence signal is emitted") is the better claim.
- **Mark `lakko-tuf` and `lakko-core` in the module map as having no consumer.**
  `CLAUDE.md` already does exactly this for `lakko-core` — *"An empty placeholder,
  not an umbrella … this row exists so nobody hunts for a re-export surface that
  was never built"* — which is the right treatment and should be extended to
  `lakko-tuf`. The threat model's coerced-maintainer row should be softened to
  match.

## Notes

- `lakko-core`'s 12 lines are already documented as a placeholder and are listed
  here only for completeness; it contributes nothing to the impact above.
- This is Low because nothing is *broken*. The severity comes entirely from
  misdirection: a reader of the documentation believes two defences are in force
  that are not, and allocates review attention to code that is not reachable.
  `audit-verify` may reasonably judge the `lakko-presence` half separately, since
  a threat model citing a non-existent mitigation for invariant #5 is arguably
  Moderate-impact documentation drift rather than Minor.
- I did **not** verify that the wasm bundler retains these exports; I inferred it
  from `#[wasm_bindgen]` export semantics. Measuring the actual contribution to
  `lakko_ffi_wasm_bg.wasm` would settle the payload half and I did not do it.
