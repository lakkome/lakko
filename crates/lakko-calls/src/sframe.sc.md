<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->
<!-- GENERATED SPEC (codespeak) for crates/lakko-calls/src/sframe.rs — behavioral contract, reviewer-facing. Keep in lock-step with the module; the spec-drift gate fails CI if sframe.rs changes and this file does not. -->

# `sframe` — SFrame per-frame media AEAD pipeline (spec)

**Module:** `crates/lakko-calls/src/sframe.rs`
**One line:** Turn the MLS group's per-call exporter secret into per-participant AEAD keys and seal/open individual media frames (RFC 9605), so the SFU forwards ciphertext it can never read.

## Why this module exists

The SFU routes media without decrypting it. That only holds if every frame is E2EE-sealed under a key the SFU never sees. This module is the keying-and-AEAD core that makes that true: it takes the 32-byte per-call seed (which every participant derives identically from their own MLS group at the same epoch) and produces, for each `(sender_leaf, stream)` KID, a distinct AEAD key + nonce salt + monotone frame counter. A receiver re-derives the *sender's* key from the KID carried in the SFrame header — which the SFU forwards verbatim — so any participant can open any other's frames while the SFU holds none of it. It is the crypto backstop behind invariant #4 (no plaintext media): a mis-routed, cross-call, or cross-epoch frame is not merely undelivered, it is undecryptable.

This module is the keying math only. The rotation choreography (re-export after a commit, overlap window) is a caller/`CallKeyRing` concern; the header codec is `header.rs`.

## Two-layer key derivation

```
MLS exporter  (label = "lakko/call/v1", ctx = call_id, len = 32)
      │
      ▼   CallKey  — 32-byte seed, byte-identical on every participant at this epoch
      │
      │   where KID = (sender_leaf << 8) | stream_index   (ADR-0035)
      │   HKDF-SHA-256 over  "lakko/sframe/v1/key"  || KID(BE u64)   → AEAD key
      │   HKDF-SHA-256 over  "lakko/sframe/v1/nonce" || KID(BE u64)  → 12-byte nonce salt
      ▼
ParticipantKey  — per-(leaf, stream) key + salt + counter
      │
      │   per frame:  nonce = salt XOR (counter as BE u32 in the low 4 bytes)   (RFC 8439 §2.4 / RFC 9605 §4.4)
      ▼   ChaCha20-Poly1305 seal / open
sealed frame
```

The two HKDF info strings are domain-separated so a context-confusion attacker (key-as-nonce, nonce-as-key) lands on a distinct label. Both the key and the salt fold the **full composite KID** `(sender_leaf << 8) | stream_index` (ADR-0035, superseding the ADR-0034 per-media-TYPE byte), so distinct leaves **and** each of a single leaf's distinct streams (audio, camera, screen-share — any number) get distinct key material (see C-3). The KID is the only per-stream discriminator that survives an SFU (which forwards the header verbatim), so it — not a local media-type tag or a transceiver MID — is what keeps every one of a participant's concurrent streams off a shared `(key, nonce)` space. Two video streams that both mapped to `Video` under ADR-0034's tag now key into disjoint spaces.

## Public contract

- `CallKey::derive(group, identity, call_id)` — pulls the per-call seed from the live MLS exporter. Deterministic per epoch: two parties in the same group at the same epoch reach the same 32 bytes (C-1), so no shared-secret exchange is needed. Fails with `Error::Exporter` only if the MLS group rejects the label/length.
- `CallKey::from_seed(seed)` — construct from a known seed; documented test-only/FFI-only, production goes through `derive`.
- `CallKey::participant_key(kid)` — the primitive: derive the per-`(leaf, stream)` `ParticipantKey` from a composite SFrame `kid = (sender_leaf << 8) | stream_index` (ADR-0035). A **receiver** reads the KID from the SFrame header (forwarded verbatim by the SFU) and calls this directly — it needs NO local media-type or track-kind signal. Fresh counter = 0.
- `CallKey::participant_key_for(sender_leaf, stream_index)` — the **sender-facing** convenience: composes the KID via `SframeHeader::kid_for` (fail-closed on `stream_index > 255`) then calls `participant_key`. The sender assigns `stream_index` in a stable local order (0 = first stream, 1..N = additional streams).
- `ParticipantKey::{kid, sender_leaf, stream_index}()` — accessors: `kid()` is the composite; `sender_leaf()` / `stream_index()` split it (`kid >> 8` / `kid & 0xFF`).
- `ParticipantKey::seal(aad, plaintext)` — pre-increments the counter, then AEAD-seals; returns `(frame_counter, ciphertext_with_tag)`. The caller must embed `frame_counter` in the header so the receiver can rebuild the nonce.
- `ParticipantKey::open(aad, ciphertext, frame_counter)` — stateless AEAD-open under the caller-supplied counter. Any authentication failure (wrong key / AAD / counter / tamper) is `Error::Crypto`.
- `ParticipantKey::seal_framed(plaintext)` / `open_framed(wire)` — the ADR-0013 wire contract: seal produces `SframeHeader(kid, ctr) || ciphertext` (where `kid` is this key's composite `(leaf, stream)`) with the **encoded header bytes bound as the AEAD AAD**; open parses the header, checks the KID against this key's `kid`, and AEAD-opens. Because the AAD is derived from `(kid, ctr)` alone (never host RTP metadata), a web transform and a native shim feed identical AAD and interop byte-for-byte.
- `call_key_fingerprint(key)` — BLAKE3 of the seed for diagnostic/SAS-style display; never exposes the raw bytes.

## Invariants upheld

- **C-1 / C-2 (seed agreement + cross-party seal/open):** the seed is a deterministic MLS-exporter output; a receiver re-derives the sender's `ParticipantKey` from its own group view and opens.
- **C-3 (per-leaf key separation):** distinct `sender_leaf` values yield distinct AEAD keys AND distinct nonce salts (the leaf is folded — as the high bits of the KID — into both HKDF derivations). Test `participant_keys_differ_across_leaves`.
- **ADR-0035 (per-stream key separation — supersedes ADR-0034):** a single leaf's concurrent streams — audio (stream 0) and TWO video streams (camera = 1, screen-share = 2), any number — derive pairwise-distinct AEAD keys AND pairwise-distinct nonce salts (the FULL KID is folded into both HKDF derivations), so their independent per-stream counters can never reuse a `(key, nonce)` pair — sealing the same plaintext at the same counter on any two streams yields different ciphertext. This closes the two-video-stream hole ADR-0034's per-media-TYPE tag left open (both video streams mapped to one tag and re-collided). Test `three_streams_of_one_leaf_get_distinct_keys_and_nonces`; `participant_key_reports_its_leaf_and_stream` pins the injective `(leaf, stream) → key` mapping at the numeric boundary (leaf 1/stream 0 vs leaf 0/stream 255 do not alias).
- **C-5 (strictly monotone counter, no nonce reuse):** `seal` increments once per frame and never reuses a `(key, nonce)` pair. At `u32::MAX` the seal path returns `Error::CounterExhausted` — checked *before* any state mutation, so the key stays usable for a re-keyed retry, and the refusal is idempotent (never wraps to 0, which would replay the first frame's nonce). Tests `counter_increments_strictly`, `counter_exhaustion_returns_typed_error`, `counter_exhaustion_is_idempotent`, `counter_max_frame_still_opens_on_receiver`.
- **C-6 (AAD binding):** the SFrame header bytes are the AAD; a wrong-AAD open fails authentication. Test `wrong_aad_rejects_open`.
- **C-7 (counter binding):** the counter feeds the nonce, so a wrong counter yields a wrong nonce and the AEAD rejects. Test `wrong_counter_rejects_open`; and at the framed layer a header spliced onto another frame's body is rejected (`open_framed_rejects_a_header_spliced_onto_another_frames_body`).
- **C-55 / ADR-0013 (cross-platform AAD = wire header):** `open_framed` binds the *literal on-wire header slice* as AAD, not a re-encode of the parsed `(kid, ctr)` — because `SframeHeader::decode` accepts non-minimal encodings, re-encoding could differ from what the sender authenticated. A pinned golden vector (`GOLDEN_WIRE_HEX`) is asserted identical here and on web/iOS/Android/native-C-ABI, so a frame sealed on any stack opens on any other (`golden_vector_seals_to_the_canonical_cross_stack_wire`, `golden_vector_opens_to_the_plaintext`).
- **Invariant #4 (no plaintext fallback):** every open is an AEAD authentication; a frame that fails to decrypt is dropped, never surfaced as plaintext. Cross-call / cross-epoch isolation is the crypto backstop (`frame_sealed_under_one_call_key_does_not_open_under_another`).

## Failure modes

- `Error::Exporter` — MLS `export_secret` rejected the label/length (`CallKey::derive` only).
- `Error::Crypto` — HKDF rejected an input length (practically unreachable), OR a `participant_key_for` stream index that does not fit the KID's 8-bit stream field (a wrapped `HeaderError::StreamIndexOverflow` — fail-closed, ADR-0035), OR any AEAD authentication failure, OR in `open_framed`: a malformed header, a KID that does not match this key's `(leaf, stream)` KID (rejected up front before any AEAD work), or a counter exceeding the 32-bit keying range.
- `Error::CounterExhausted` — the 32-bit counter would roll over on `seal` / `seal_framed`. Caller MUST trigger a fresh MLS commit → fresh CallKey → fresh ParticipantKey.
- **No panics, no `unwrap`/`expect` in the production paths.** The only `unwrap`s are in `#[cfg(test)]` code. `set_counter_for_test` (which could reset the counter and cause nonce reuse) is compiled out of the release crate via `#[cfg(test)]`, so no production caller can rewind the counter.

## What this module does NOT do (do not add it here)

- **No replay protection.** `open` / `open_framed` are deliberately STATELESS — they derive the nonce from the supplied counter and AEAD-open, tracking no receive high-water. The same sealed wire opens repeatedly. Dropping a replayed or out-of-order counter is an APP-LAYER responsibility (the receiver, or the SFU's per-sender state, tracks the monotone header counter). This boundary is pinned by `open_framed_is_stateless_so_replay_is_an_app_layer_concern` so a future change that makes open stateful breaks loudly.
- **No rotation choreography.** It does not re-export on an epoch change, hold an overlap window, or prune old keys — that is the caller / `CallKeyRing`. This module owns the keying math for a single epoch.
- **No header codec.** It calls into `header::SframeHeader` for `encode`/`decode`; it does not itself parse the config byte or the extended fields.
- **No transport / SFU logic.** It never sees a socket; it seals/opens byte buffers.

## Attacker exposure

`open_framed` runs on `wire` bytes handed over by an **untrusted SFU or peer** before authentication succeeds. It parses the attacker-controlled header via `SframeHeader::decode` (the panic-free, bounded codec fuzzed by `crates/lakko-calls/fuzz/fuzz_targets/header_decode.rs`), then enforces the KID-matches-this-leaf guard and the 32-bit counter range before touching the AEAD. The ciphertext body is only ever consumed by ChaCha20-Poly1305 `open`, so a forged or tampered frame is an authentication failure, not a parse crash or a plaintext leak. There is no sframe-specific fuzz target beyond the header codec; the AEAD open path is covered by the tamper/wrong-key/wrong-counter/splice/cross-call unit tests listed above.

## References

- ADR-0013 (cross-platform SFrame AEAD AAD = the header bytes), ADR-0001 (MLS exporter for call media keys), ADR-0029 (mid-call rekey — the `CallKeyRing` that drives this module's rotation), ADR-0035 (per-`(leaf, stream)` KID key separation — the full KID folded into `participant_key`; supersedes the ADR-0034 per-media-TYPE tag)
- Invariants C-1, C-2, C-3, C-5, C-6, C-7, C-55 (`docs/security/calls.md`); security invariant #4 (no plaintext fallback)
- Test fixtures create their MLS groups with `lakko_mls::GroupKind::Group` (ADR-0062 made the kind a required argument of `Group::create`); nothing in this module reads the kind.
- Sibling spec: `crates/lakko-calls/src/header.sc.md` (the header codec this module seals over, incl. the `kid_for` / `leaf_of` / `stream_of` KID helpers)
- Key tests (in-module `#[cfg(test)]`): `participant_key_round_trips`, `participant_keys_differ_across_leaves`, `three_streams_of_one_leaf_get_distinct_keys_and_nonces`, `participant_key_reports_its_leaf_and_stream`, `counter_increments_strictly`, `counter_exhaustion_returns_typed_error`, `wrong_aad_rejects_open`, `wrong_counter_rejects_open`, `golden_vector_seals_to_the_canonical_cross_stack_wire`, `open_framed_rejects_wrong_leaf_key`, `open_framed_rejects_a_header_spliced_onto_another_frames_body`, `open_framed_is_stateless_so_replay_is_an_app_layer_concern`, `frame_sealed_under_one_call_key_does_not_open_under_another`
