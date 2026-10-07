<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0014: WebAuthn action binding via a minimal webauthn-rs-core challenge-injection fork

- **Status:** Accepted — implemented end-to-end. Server-side 2026-06-04; the
  two items this line listed as *pending* both landed and the line was stale
  until the 2026-09-02 documentation audit corrected it: the **fork-delta CI
  guard** is [`services/auth/tests/fork_delta_guard.rs`](../../services/auth/tests/fork_delta_guard.rs)
  (it pins the hash of the vendored `core.rs` so an accidental edit to the
  upstream-verbatim verification path fails CI), and the **client-side wiring**
  is live on all three platforms — web
  ([`webauthn-client.ts`](../../apps/web/src/auth/webauthn-client.ts),
  `PasskeysSection.tsx`, `Devices.tsx`), iOS
  ([`WebauthnActionBinding.swift`](../../apps/ios/Lakko/Webauthn/WebauthnActionBinding.swift),
  `WebauthnClient.swift`, `DevicesScreen.swift`) and Android
  (`ui/devices/DevicesScreen.kt`). See also
  [ADR-0015](./0015-device-removal-authorization.md), which took the
  remove-device half from client-gated to peer-verified.
- **Amended 2026-10-06 by [ADR-0065](./0065-account-key-reset.md)** (section
  "Amended 2026-10-06: fresh-auth grant for delete and add-device"):
  `DELETE /account` now requires a fresh-auth grant (header
  `x-lakko-fresh-grant`, minted by a password or passkey sign-in within the
  last ten minutes) for EVERY account, checked before the action grant; an
  account with a passkey still needs this ADR's action grant as well. "A
  password-only account is deleted on the session token alone" no longer
  holds.
- **Date:** 2026-06-04
- **Context:** G-13 (`docs/security/known-gaps.md`) — bind a passkey assertion to the specific action it authorises, so a verified assertion cannot be replayed against (or reordered onto) a different request inside the same TLS session.

## Context

Today a passkey assertion proves *"the user has the registered
device"* but does not bind to *which action* the request carries.
The `webauthn_states` row ties an assertion to a single-use
`state_id`, but nothing ties the **authenticator's signature** to
the concrete operation being authorised. G-13 calls for mixing the
OPAQUE message hash (more generally, an **action hash**) into the
WebAuthn assertion challenge so the authenticator's signature
cryptographically covers the action.

The standard way to do this is RP-controlled: set the WebAuthn
`challenge` = `H(domain || action_hash || server_nonce)`. The
authenticator signs over `clientDataJSON` (which echoes the
challenge), so the signature attests to the action.

### Why this is not a one-line change (spike result, 2026-06-04)

`services/auth` uses the **safe high-level `webauthn-rs 0.5.5`**
API deliberately. That API generates the challenge internally and
exposes **no hook** to set it:

- `Webauthn::start_passkey_authentication` /
  `start_discoverable_authentication` call the core's
  `generate_challenge()` (random) — no parameter for a custom
  challenge.
- The low-level `webauthn-rs-core 0.5.5` is no better out of the
  box: `ChallengeAuthenticateBuilder` has no challenge setter,
  `WebauthnCore::generate_challenge_authenticate` randomises
  internally, and the resulting `AuthenticationState.challenge`
  field is **`pub(crate)`** (`interface.rs:48`) — not settable from
  outside the crate. `WebauthnCore` itself is only constructible via
  `new_unsafe_experts_only`, which carries an explicit upstream
  warning ("UNSAFE … signature MAY change WITHIN MINOR VERSIONS …
  you MUST uphold undocumented invariants … bypasses and full
  account compromises").

So a cryptographic challenge binding requires either **(a)** owning
the WebAuthn assertion-verification code, or **(b)** a fork that
adds a challenge-injection seam. Verification is the dangerous part
of WebAuthn (rpIdHash, flags, user-verification policy, counter
regression, COSE signature over `authenticatorData || SHA256(clientDataJSON)`),
and reimplementing it is exactly what using `webauthn-rs` was meant
to avoid.

### Threat severity

G-13 itself rates the threat **"implausible but not impossible"**
(a request reorder inside one authenticated TLS session) and slots
it to Phase 2c. The fix must therefore not *introduce* a larger
risk than the one it closes.

## Decision

Implement the cryptographic binding with a **minimal vendored fork
of `webauthn-rs-core`** that adds exactly one challenge-injection
entry point, and **reuse upstream's verification verbatim**.

1. **Vendor** `webauthn-rs-core 0.5.5` under
   `crates/vendor/webauthn-rs-core/` and pin it via
   `[patch.crates-io]` in the workspace `Cargo.toml`. The vendored
   copy is byte-identical to upstream **except** for one added
   method:

   ```rust
   // webauthn-rs-core, core.rs — identical to
   // generate_challenge_authenticate EXCEPT it consumes a
   // caller-supplied Challenge instead of self.generate_challenge().
   pub fn generate_challenge_authenticate_with_challenge(
       &self,
       challenge_builder: ChallengeAuthenticateBuilder,
       chal: Challenge,
   ) -> Result<(RequestChallengeResponse, AuthenticationState), WebauthnError>
   ```

   The **verification path (`authenticate_credential`) is NOT
   touched** — we keep upstream's audited assertion checks verbatim.
   The only new security-relevant surface is "the challenge is
   whatever the caller passed," which is precisely the RP's
   prerogative under the WebAuthn spec.

2. **Action-bound auth flow** (a *new* endpoint pair, additive — the
   existing passkey/discoverable login flows on the safe high-level
   `Webauthn` are untouched):

   - `/auth/action/start`: caller supplies the `action_hash`
     (32 bytes; for an OPAQUE-authorised request this is
     `BLAKE3(opaque_message_bytes)`, but the server treats it as an
     opaque 32-byte action identifier). The server derives:

     ```text
     challenge = BLAKE3_keyed("lakko/webauthn-action/v1",
                              action_hash || server_nonce_32)   // 32 bytes
     ```

     builds the allow-list `ChallengeAuthenticateBuilder` for the
     account's passkeys, calls
     `generate_challenge_authenticate_with_challenge(builder, Challenge::new(challenge))`,
     persists the returned `AuthenticationState` **plus the
     `action_hash`** in `webauthn_states`, and returns the
     `RequestChallengeResponse`.
   - `/auth/action/finish`: loads the state, runs
     `authenticate_credential(&credential, &state)` (upstream,
     verbatim) — which fails unless the authenticator signed over
     **our** challenge — and additionally re-checks the caller's
     presented `action_hash` equals the stored one (defence in
     depth; the cryptographic guarantee is already in the signature).

3. **Single-use + binding invariants** carry over from the existing
   flow: the state row is deleted on `/finish`, so a verified
   action-bound assertion cannot be replayed; and because the
   challenge is a function of `action_hash`, an assertion produced
   for action A fails verification if presented for action B (the
   stored state's challenge — and thus the expected signature —
   differs).

## Alternatives considered

- **Server-side state binding only** (store `action_hash`, compare
  on `/finish`, leave the random challenge). Rejected as the
  *primary* mechanism: the authenticator's signature would still not
  cover the action, so it is server-enforced, not cryptographic —
  exactly the property G-13 asks to upgrade. (It is, however,
  retained as the defence-in-depth check in step 2.)
- **Full hand-roll of assertion verification.** Rejected: it puts
  the entire dangerous verification surface (the part upstream warns
  about) on us, for an *implausible* threat. The minimal fork keeps
  verification upstream.
- **serde round-trip hack** (serialize `AuthenticationState`, patch
  the `challenge` JSON field, deserialize). Rejected: depends on the
  private serde representation of `HumanBinaryData` / field names,
  which upstream may change in a minor release — fragile and
  silent-break-prone for a security-critical value.
- **WebAuthn extension (`txAuthSimple`).** Rejected: deprecated and
  unsupported by modern authenticators.
- **Wait for upstream challenge-injection support.** Tracked as a
  fallback; not actionable now.

## Consequences

- **We own a fork of a security-critical dependency.** Mitigations:
  the delta is a single additive method (auditable in one diff
  against the pinned upstream tag); `cargo deny` / `cargo audit`
  continue to run; the Nix flake pins the source; a CONTRIBUTING
  note documents "re-base on each upstream `webauthn-rs-core` bump
  and re-confirm the one-method delta + that `authenticate_credential`
  is unchanged."
- **An ADR + property tests are mandatory** (this ADR; tests below).
  Per the repo's crypto-change posture, the action-binding path
  should be in scope for the Phase 2 external crypto audit.
- The existing username+passkey and discoverable login flows are
  **unchanged** and stay on the safe high-level `Webauthn` — the
  fork is used only by the new action-bound endpoints.

## Test plan (property + integration)

1. **Valid bound assertion accepted.** A synthetic authenticator
   signs the issued (action-bound) challenge → `/finish` succeeds.
2. **Action swap rejected.** An assertion produced for `action_hash
   A` presented under a state issued for `action_hash B` fails
   verification (challenge mismatch), not merely the server-side
   `action_hash` compare.
3. **Challenge tamper rejected.** Flipping any byte of the issued
   challenge before signing → verification fails.
4. **Replay rejected.** A second `/finish` with the same `state_id`
   fails (row deleted after first use).
5. **Fork-delta guard.** A test (or CI check) asserting the vendored
   `authenticate_credential` is byte-identical to the pinned upstream
   (e.g. a checked-in hash of the function body / file), so an
   accidental edit to the verification path is caught. — **Done:**
   `services/auth/tests/fork_delta_guard.rs` pins the SHA-256 of the
   vendored `core.rs` (LF-normalized) plus structural assertions
   (exactly one fork-delta method; `authenticate_credential` /
   `verify_credential_internal` present). Any edit to the file fails CI
   with a re-pin procedure. The pinned state was confirmed verbatim on
   2026-06-04 by diffing against the registry-cached pristine 0.5.5: a
   single additive hunk (the fork method), zero deletions, zero changes
   to the verification path.

## References

- G-13 — `docs/security/known-gaps.md`.
- Security invariants #1/#7 (`CLAUDE.md`) — hardware-backed device
  keys; no PII; this strengthens per-action attestation on top.
- `webauthn-rs-core 0.5.5` — `core.rs`
  (`generate_challenge_authenticate`, `authenticate_credential`),
  `interface.rs` (`AuthenticationState`), `internals.rs`
  (`Challenge::new`).
