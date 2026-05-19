<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# Lakko — Cryptographic primitives in use

Single index of every primitive currently shipped. Every row
cites the live code path the primitive lives in so the
reviewer can jump straight to the call site.

## Symmetric

| Primitive | Algorithm / parameters | Code path | Used for |
|---|---|---|---|
| AEAD (default) | ChaCha20-Poly1305 (RFC 8439), 256-bit key, 96-bit nonce, 128-bit tag | [`crates/lakko-crypto/src/aead.rs`](../../crates/lakko-crypto/src/aead.rs) | Backup envelope, history bundle, attachment CEKs, per-message wire encryption |
| AEAD (interop) | AES-256-GCM (NIST SP 800-38D) | same crate | FIPS-friendly MLS ciphersuite, SFrame on AES-NI devices |
| Hash | BLAKE3 (32-byte output) | [`crates/lakko-crypto/src/hash.rs`](../../crates/lakko-crypto/src/hash.rs) | DeviceGroup `group_id` derivation, content-addressable blob ids |
| KDF | HKDF-SHA-256 / HKDF-SHA-384 | [`crates/lakko-crypto/src/kdf.rs`](../../crates/lakko-crypto/src/kdf.rs) | Device root key derivation, exporter→AEAD-key derivation |
| Password KDF | Argon2id (RFC 9106), memory-hard, 64 MB / 3 iterations / parallelism 1 | inside `crates/lakko-backup` | Recovery-phrase → backup KEK |

## Asymmetric

| Primitive | Algorithm / parameters | Code path | Used for |
|---|---|---|---|
| Signing | Ed25519 (RFC 8032) | [`crates/lakko-crypto/src/signing.rs`](../../crates/lakko-crypto/src/signing.rs) | Identity credentials, MLS leaf signatures, transparency-log roots, capability links |
| Key agreement | X25519 (RFC 7748) | [`crates/lakko-crypto/src/agreement.rs`](../../crates/lakko-crypto/src/agreement.rs) | HPKE base mode, MLS init-key material |
| HPKE | RFC 9180, mode_base, X25519-HKDF-SHA256, ChaCha20-Poly1305 | OpenMLS-internal (RFC 9420 §5) | MLS Welcome envelopes, contact-request envelopes |

## Protocol-level

| Primitive | Reference | Code path | Notes |
|---|---|---|---|
| MLS group keying | RFC 9420 | [`crates/lakko-mls/`](../../crates/lakko-mls/), wraps [openmls](https://github.com/openmls/openmls) | 1×1 = degenerate group of 2. Channels >50k = two-tier (PublisherGroup + sender-key) — see `docs/adr/0001-mls-only.md` |
| MLS exporter | RFC 9420 §8.5 | [`crates/lakko-mls/src/group.rs::Group::export_secret`](../../crates/lakko-mls/src/group.rs) | Labels we coin: `lakko/call/v1` (SFrame seed, Phase 3), `lakko/history-bundle/v1` (Phase 2a3d8) |
| OPAQUE-PAKE | RFC-draft `opaque`, ristretto255 curve | [`crates/lakko-auth-client/`](../../crates/lakko-auth-client/) + [`services/auth/`](../../services/auth/) | Registration / login / recover; Argon2id KSF client-side |
| VOPRF (discovery) | RFC 9497 mode-VOPRF | [`crates/lakko-discovery-client/`](../../crates/lakko-discovery-client/) | Username lookup with server-blinded queries |
| Equix PoW | Equix algorithm (Tor's onion-service PoW lineage) | [`crates/lakko-pow/`](../../crates/lakko-pow/) | Anti-spam on auth endpoints + on contact-request envelopes |
| WebAuthn / passkey | FIDO2 + `webauthn-rs` 0.5 | [`services/auth/`](../../services/auth/) + per-client integration | Passkey opt-in (Phase 2a2) |
| Call-admission JWT | JWS Compact (RFC 7515) + EdDSA (RFC 8037), Ed25519 signing key + JWK publication (RFC 8037 OKP form) | [`crates/lakko-call-jwt/`](../../crates/lakko-call-jwt/src/lib.rs) (mint + verify + JWKS) + [`services/auth/src/call_jwt.rs`](../../services/auth/src/call_jwt.rs) (handlers + key persistence) | Per-call admission token the SFU validates; 1-hour `exp`, public key published at `/.well-known/jwks.json`. Hand-rolled JWS — no `jsonwebtoken` dep. See [`calls.md`](./calls.md) C-15..C-23 (Phase 3b4) |

## Cipher suite pins (MLS)

Production-shipped suites (see
[`crates/lakko-mls/src/ciphersuite.rs`](../../crates/lakko-mls/src/ciphersuite.rs)):

- `MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519` — current
  production default.
- (Roadmap) `MLS_256_DHKEMP384_AES256GCM_SHA384_P384` —
  FIPS-friendly track.
- (Roadmap) `MLS_256_DHKEMX25519_CHACHA20POLY1305_SHA256_Ed25519`
  — fast track.

Per-suite parameters are pinned at compile time — runtime
selection between suites lands when the second suite ships.

## Random source

All random material funnels through
[`crates/lakko-crypto/src/random.rs::fill`](../../crates/lakko-crypto/src/random.rs)
which wraps the `getrandom` crate. On every supported
platform this is the OS CSPRNG:

- iOS / macOS: `SecRandomCopyBytes`
- Android: `/dev/urandom` (kernel CSPRNG since API 26)
- Web: `crypto.getRandomValues` via the `getrandom` `js`
  feature
- Linux: `getrandom(2)` syscall
- Windows: `BCryptGenRandom`

`SecureRandom` on Android is also seeded from `/dev/urandom`
since API 26 — see the matching note in
[`apps/android/.../auth/DeviceIdentityStore.kt`](../../apps/android/app/src/main/kotlin/me/lakko/app/auth/DeviceIdentityStore.kt).

## Forbidden constructs

CI gates enforce these — see `CLAUDE.md` "Code-style rules":

- No `unwrap()` / `expect()` in production code.
- No `unsafe` outside `lakko-ffi-uniffi` / `lakko-ffi-wasm`
  (and there it carries a documented `// SAFETY:` comment).
- No `panic!()` in library code (only in binary startup
  paths).
- `clippy::all`, `clippy::pedantic`, `clippy::nursery` with
  `-D warnings`.
- `cargo deny` blocks any new dependency outside the
  curated allowlist + license set (AGPL-3.0-only / MIT /
  Apache-2.0).
