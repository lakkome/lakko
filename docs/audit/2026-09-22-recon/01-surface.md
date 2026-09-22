<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# 01 — Surface map and claim verification

**Frozen commit:** `ab1a85e8e112dd656fadfc01904e518bdfebbb79`
**Working tree at freeze:** clean except the untracked engagement directory.
**Every number below came out of a command run this session.** Where I did not
measure something, the note says so rather than leaving a gap the reader fills in.

---

## 0. What this note is, and what it is not

This is the independent map. It was built by reading the tree, not by reading
`CLAUDE.md` or `docs/security/`. Those were opened only afterwards, to compare.

Two sentences the reader must keep apart, because the rest of the document depends
on it:

- **"Found nothing"** — I looked and there was nothing there.
- **"Did not reach"** — I did not look. §7 lists every one of these.

A surface I did not reach is **unknown**, not clear.

---

## 1. Component inventory (measured)

### 1.1 Repository totals

```
git ls-files | grep -E '\.(rs|ts|tsx|swift|kt)$' | wc -l   →  1780 files
```

Hand-written client source, generated UniFFI bindings and the wasm blob excluded,
build directories excluded, git-tracked only:

| Client | Files | Lines |
|---|---:|---:|
| `apps/web` (TS/TSX) | 483 | 129,519 |
| `apps/ios` (Swift) | 364 | 122,156 |
| `apps/android` (Kotlin) | 394 | 133,476 |
| `apps/shared` (C++ bridge) | 7 | 1,775 |

Generated bindings, **excluded above and counted separately** because they are
machine-written and no human reviews them:
`apps/android/.../uniffi/lakko_ffi_uniffi.kt` = 27,528 lines;
`apps/ios/Sources/LakkoCore/lakko_ffi_uniffi.swift` = 22,292 lines.

### 1.2 Rust crates (`src/` only, measured)

| Crate | LOC | Files | Production consumer? |
|---|---:|---:|---|
| `lakko-ffi-wasm` | 17,593 | 23 | web |
| `lakko-ffi-uniffi` | 16,857 | 17 | iOS, Android |
| `lakko-mls` | 14,978 | 16 | both FFI surfaces |
| `lakko-transport` | 14,782 | 38 | `lakko-ffi-uniffi`, `obfs-proxy` |
| `lakko-contacts` | 12,172 | 21 | all three FFI surfaces |
| `lakko-calls` | 8,680 | 14 | FFI + `lakko-sframe-native` |
| `lakko-tuf` | 3,211 | 3 | **none — zero reverse dependencies** |
| `lakko-transparency` | 2,777 | 8 | both FFI surfaces, `services/*` |
| `lakko-channels` | 2,721 | 8 | FFI |
| `lakko-attest` | 2,211 | 4 | FFI, `services/auth` |
| `lakko-ffi-extension` | 2,199 | 9 | iOS NSE |
| `lakko-attachments` | 2,131 | 1 | FFI |
| `lakko-backup` | 2,101 | 6 | FFI |
| `lakko-sframe-native` | 1,508 | 1 | C++ bridge (iOS/Android) |
| `lakko-storage` | 1,357 | 6 | **`lakko-ffi-wasm` only; no web call site** |
| `lakko-crypto` | 1,289 | 9 | most crates |
| `lakko-mnemonic` | 1,134 | 5 | FFI, `services/auth` |
| `lakko-sync` | 1,035 | 4 | **`lakko-ffi-wasm` only; no web call site** |
| `lakko-shamir` | 962 | 2 | both FFI surfaces |
| `lakko-discovery-client` | 883 | 4 | both FFI surfaces |
| `lakko-auth-client` | 795 | 7 | both FFI surfaces |
| `lakko-sfu-proto` | 688 | 1 | `services/sfu`, FFI |
| `lakko-call-jwt` | 664 | 1 | `services/auth`, FFI |
| `lakko-push` | 601 | 1 | FFI |
| `lakko-auth-token` | 511 | 1 | `services/*` |
| `lakko-pow-server` | 482 | 1 | `services/{auth,discovery,push-gw}` |
| `lakko-presence` | 427 | 1 | **`lakko-ffi-wasm` only; no web call site** |
| `lakko-proto` | 318 | 1 | `lakko-mls`, `lakko-calls`, FFI |
| `lakko-pow` | 304 | 1 | FFI, `services/discovery` |
| `lakko-core` | 12 | 1 | **none — zero reverse dependencies** |

Reverse dependencies measured with `cargo tree -i <crate> --workspace`.
Web call sites measured by grepping `apps/web/src` for the exported symbol,
excluding `*.test.ts`.

**Result: ~5,600 lines of Rust have no production caller.** `lakko-tuf` (3,211
lines, 5 fuzz targets) is a workspace member linked into nothing.
`lakko-presence` + `lakko-sync` + `lakko-storage` (2,819 lines) are exported
through `#[wasm_bindgen]`, so they **ship inside the wasm binary delivered to
every browser**, but the only `apps/web/src` occurrences of their symbols are
three doc comments (`wasm.ts:11`, `auth/device-key.ts:92`, `routes/Chat.tsx:30`).
See **LAK-RC-05**.

### 1.3 Services (`src/` only, measured)

| Service | LOC | systemd unit | In `deploy.sh` | nginx route |
|---|---:|---|---|---|
| `auth` | 12,646 | yes | yes | `/api/auth/` |
| `sfu` | 8,776 | yes | yes | `sfu.conf` |
| `discovery` | 7,751 | yes | yes | `/api/discovery/` |
| `push-gw` | 4,394 | yes | yes | `/api/push/` |
| `channels` | 4,562 | yes | yes | `/api/channels/` |
| `transparency` | 3,756 | **no** | **no** | **none** |
| `gif-proxy` | 3,048 | yes | yes | `/api/gifs/` |
| `storage` | 1,733 | yes | yes | `/api/storage/` |
| `ds` | 891 | yes | yes | `/ds/` |
| `witness` | 826 | **no** | **no** | **none** |
| `obfs-proxy` | 656 | **no** | **no** | **none** |
| `transport-rendezvous` | 29 | **no** | **no** | **none** |
| `edge` | 28 | **no** | **no** | **none** |

`infra/systemd/` contains 8 service units + `lakko-coturn.service` + `lakko.target`.
`infra/scripts/deploy.sh:34` reads
`SERVICES=(auth ds storage discovery push-gw channels sfu gif-proxy)`.
`grep -rn "transparency|witness|obfs" infra/` returns **nothing**.

This is the single most consequential fact in this note and it drives §4.1.

---

## 2. Attack-surface inventory

### 2.1 Network endpoints, with the authorisation actually in force

Routes extracted from `Router::new()` chains in each service's `src/`.
"Auth" = what the code enforces, read at the route/middleware level.

| Service | Route | Auth in force |
|---|---|---|
| `ds` | `POST/GET /ds/group/{group_hex}/messages` | **none** |
| `ds` | `POST/GET /ds/mailbox/{device_hex}/messages` | **none** |
| `channels` | `POST /v1/channels` | **none** |
| `channels` | `POST/GET /v1/channels/{id}/posts` | **none** |
| `channels` | `POST/GET /v1/channels/{id}/subscribers` | **none** |
| `channels` | `POST/GET /v1/channels/{id}/subscribers/{pk}/envelope(s)` | **none** |
| `channels` | `POST /v1/channels/{id}/rotate`, `/rotate/stage`, `/rotate/commit` | **none** |
| `channels` | `GET /v1/channels/{id}/epoch` | **none** |
| `storage` | `PUT/GET/HEAD /blob/{hex}` | **none** (content-addressed) |
| `storage` | `PUT/GET/DELETE /backup` | session token (`auth::require_session`) |
| `auth` | `/signup/*`, `/login/*`, `/recovery/login/start`, `/password/reset/start` | PoW |
| `auth` | `/webauthn/auth/{start,finish}`, `/totp/recover`, `/vault/recover/start` | PoW + anonymous |
| `auth` | `/device/request`, `/device/request/status` | anonymous |
| `auth` | `/vault*`, `/totp/enroll/*`, `/account*`, `/call/jwt`, `/call/turn-credential`, `/auth/genesis/attest`, `/auth/backup/head`, `/device/requests`, `/webauthn/register/*`, `/webauthn/credentials` | bearer session |
| `auth` | `GET /.well-known/jwks.json` | public |
| `discovery` | `/discovery/oprf`, `/lookup`, `/channel/lookup` | PoW |
| `discovery` | `/discovery/{register,identity,keypackage,invite,call-invite,group-invite}` (POST) | session token |
| `discovery` | `GET /discovery/keypackage/{account_id}`, `GET /discovery/invite/{blob_id}` | unauthenticated read |
| `push-gw` | `/push/{register,unregister,notify}`, `/push/group/{subscribe,unsubscribe}`, `/push/notify-group` | PoW on subscribe; see §7 |
| `gif-proxy` | `/gifs/{trending,search,media/{id}/{variant}}` | session token |
| `sfu` | `GET /v1/connect` (WS), `GET /v1/calls/{call_id}` | call JWT |
| `transparency` | `POST /transparency/append` | shared service secret — **not deployed** |
| `transparency` | `GET /transparency/{root,proof/{idx},operator-key}` | public — **not deployed** |
| `witness` | `POST /witness/cosign`, `GET /witness/key` | cosign secret — **not deployed** |

Every service also exposes `/health`, `/ready`, `/version` unauthenticated.

**`services/ds` contains no authentication code of any kind.**
`grep -rn "require_session|Authorization|bearer|Bearer|token" services/ds/src/`
returns zero lines. The same grep over `services/channels/src/` returns zero lines.
Authorisation on both is entirely capability-by-obscurity: possession of the
32-byte pseudonym (DS) or the 16-byte channel id (channels). See **LAK-RC-01**
and **LAK-RC-02**.

### 2.2 Wire parsers reachable from an untrusted source

69 fuzz targets exist (`ls crates/*/fuzz/fuzz_targets/*.rs services/*/fuzz/fuzz_targets/*.rs | wc -l` → 69).
The coverage is genuinely good on the *envelope codec* layer — every
`lakko-contacts` envelope kind, every `lakko-attest` blob kind, the MLS wire
types, the transparency proofs, the TUF metadata, the SS-2022 and ECH parsers.

What is **not** fuzzed, measured by the absence of a target directory:

| Crate / service | Has `fuzz/`? | Untrusted input it parses |
|---|---|---|
| `lakko-sframe-native` | **no** | every media frame, through a raw-pointer C ABI |
| `lakko-crypto` | **no** | AEAD/HKDF inputs (thin wrappers) |
| `lakko-storage` | **no** | local ciphertext at rest |
| `lakko-discovery-client` | **no** | OPRF server responses |
| `lakko-auth-client` | **no** | OPAQUE server responses |
| `lakko-pow` / `lakko-pow-server` | **no** | client-submitted PoW proofs |
| `services/ds` | **no** | the unauthenticated journal body (1 MiB cap) |
| `services/discovery` | **no** | bundle/KeyPackage/invite upload bodies |
| `services/channels` | **no** | post + envelope + rotation bodies |
| `services/auth` | **no** | OPAQUE, WebAuthn, TOTP, vault bodies |
| `services/push-gw` | **no** | registration + notify bodies |
| `services/storage` | **no** | blob + backup bodies |
| `services/sfu` | **yes** (`client_frame_decode`) | WS frames |

`services/sfu` is the only service with a fuzz target. Seven of the eight
*deployed* services parse attacker-controlled HTTP bodies with no fuzz coverage.
I did not review those parsers — most are `serde_json` over `axum::Json`, which
bounds allocation via `DefaultBodyLimit`, so the likely exposure is logic rather
than memory. **That is a hypothesis I did not test**; see §7.

`lakko-sframe-native` is the one to worry about and is called out separately:
**98 `unsafe { … }` blocks, 19 `unsafe extern "C"` functions, 38 `// SAFETY:`
comments**, no fuzz target, and it sits directly under the C++/JNI/Obj-C++ media
path where every frame from an untrusted SFU arrives. See **LAK-RC-04**.

### 2.3 FFI boundaries

Five places where a type system ends:

1. `lakko-ffi-uniffi` → Swift/Kotlin. `#![forbid(unsafe_code)]` at `src/lib.rs:28`.
2. `lakko-ffi-wasm` → JS. `#![forbid(unsafe_code)]` at `src/lib.rs:68`.
3. `lakko-ffi-extension` → Swift (iOS NSE only), decrypt-only.
4. `lakko-sframe-native` → C ABI. `#![allow(unsafe_code)]` at `src/lib.rs:70`,
   `#![deny(unsafe_op_in_unsafe_fn)]` at `:71`.
5. `apps/shared/sframe-bridge/*.cc` → libwebrtc C++, thence the iOS Obj-C++ shim
   and the Android JNI module.

Measured `unsafe` occurrences across all of `crates/*/src` and `services/*/src`:
`lakko-sframe-native` 118; `lakko-contacts`, `lakko-crypto`, `lakko-mls`,
`lakko-transport`, `lakko-tuf` 1 each (all in prose, not code —
`forbid(unsafe_code)` is present in each).

### 2.4 Persistence and cross-process surfaces

| Platform | Store | Protection |
|---|---|---|
| web | IndexedDB (MLS snapshot, message log, call log, read watermarks, AccountIdentity seed, session token) | AES-256-GCM under a `CryptoKey` with `extractable:false` |
| web | `localStorage` (contact list, pins, sent requests, known groups, channel counters, rotation state, removed-device markers) | **plaintext** |
| web | `sessionStorage` (restore marker) | plaintext |
| iOS | Keychain, app group `group.me.lakko.dev.shared` | `kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly` (26 sites), `…AfterFirstUnlock` (3 sites, all `Push/DevicePushId.swift`) |
| Android | `AndroidKeyStore` + `EncryptedSharedPreferences` | TEE-backed AES; StrongBox requested only for the device root key |

Cross-process: the iOS NotificationServiceExtension reads the AccountIdentity
seed and the MLS snapshot from the shared keychain group (ADR-0041 / C-59); the
web service worker opens the same IndexedDB; Android background decrypt likewise.
I read the claimed no-write discipline in the module docs but **did not verify it
against the extension's code paths** — §7.

---

## 3. Key and trust-boundary map

### 3.1 Long-lived secrets

| Secret | Generated | Derived from | At rest | Readable by |
|---|---|---|---|---|
| BIP39 recovery phrase (24 words) | client, registration | CSPRNG | shown once; entropy sealed in the auth word vault (ADR-0049) | user; auth server holds `verifier ‖ sealed_entropy` under `LAKKO_VAULT_ENC_KEY` |
| AccountIdentity seed (64 B) | client | phrase seed (ADR-0017) | per-platform seed store, wrapped under device root key | app process; **on iOS also the NSE** |
| Account Ed25519 identity key | client | AccountIdentity seed | derived on demand | app process |
| Per-device MLS signature key | client | CSPRNG | OpenMLS provider snapshot, wrapped | app process (+ iOS NSE, read-only) |
| Web device root key | client | **HKDF-SHA-512 of the OPAQUE export key** (`auth/device-key.ts:71-105`) or of the phrase seed (`:127-153`) | non-extractable `CryptoKey` in IDB | WebCrypto only |
| iOS/Android device root key | client | see §3.3 | Keychain / AndroidKeyStore | app (+ NSE on iOS) |
| OPAQUE password file | server, registration | password | `services/auth` DB | auth server |
| Auth attestation Ed25519 key | server, first boot | CSPRNG | `services/auth` DB (migration 0010) | auth server |
| Call-JWT Ed25519 key | server | CSPRNG | `services/auth` DB | auth server |
| KT operator Ed25519 key | server | CSPRNG | `services/transparency` DB | **service not deployed** |
| Witness Ed25519 key | server | CSPRNG | `services/witness` DB | **service not deployed** |
| Release-signing key | offline, `tools/release-manifest` | CSPRNG | operator's disk, mode 0600 | operator |
| Per-group MLS epoch secrets | MLS | RFC 9420 ratchet | provider snapshot | group members |
| Per-call SFrame seed | MLS exporter `lakko/call/v1` | epoch secret | memory | call participants |
| Per-group push routing secret | client (owner) | CSPRNG | per-platform store | group members |
| Channel epoch seed | `PublisherGroup` exporter | MLS epoch | client store; sealed per subscriber | publishers + subscribers |

### 3.2 Boundaries

```
  user ── password ──┐
                     ├─ OPAQUE ─→ [auth]  holds: password file, session HMAC secret,
  user ── phrase ────┤                            attest key, call-JWT key, TOTP
          │                                       secrets (AEAD), word-vault blobs,
          │                                       contact hashes, backup version counter
          │
          └─→ AccountIdentity seed ─→ Ed25519 identity ─→ MLS leaves
                                          │
                                          ├─→ [discovery] holds: identity bundles,
                                          │    KeyPackages, OPRF key, invite blobs
                                          │
                                          └─→ MLS epoch secrets ─→ [ds] holds ciphertext
                                                    │                  keyed by group_id
                                                    ├─→ SFrame ─→ [sfu] holds ciphertext
                                                    └─→ exporter ─→ [channels] ciphertext
```

Three boundaries are worth naming because a reader will otherwise assume they are
tighter than they are:

- **`[ds]` has no authorisation boundary at all.** The group_id *is* the
  capability and it is distributed in `/g/` invite links (ADR-0028).
- **`[channels]` has no authorisation boundary at all.** The channel id is
  resolvable from the public handle through `discovery`.
- **The web device root key is not device-bound.** It is HKDF of the OPAQUE
  export key or of the phrase seed, with no device entropy
  (`apps/web/src/auth/device-key.ts:71-153`). `extractable:false` stops JS
  reading the bytes; it does not stop anyone who has the password from
  re-deriving the identical key. See **LAK-RC-06**.

### 3.3 Hardware backing, per platform (measured)

Invariant #7 requires "Secure Enclave / StrongBox / TPM 2.0 / non-extractable
WebCrypto".

- **Android** — `setIsStrongBoxBacked(true)` is called once, for the device root
  key, with an explicit fallback to `false`
  (`auth/DeviceKeyStore.kt:254`, `:280`). The MLS snapshot wrap key
  (`chat/IdentitySnapshotStore.kt:243`) and the DeviceGroup snapshot wrap key
  (`devices/DeviceGroupSnapshotStore.kt:203`) carry a comment saying StrongBox is
  **deliberately not used**, for throughput. TEE-backed, not StrongBox.
- **iOS** — `grep -rn "SecureEnclave|kSecAttrTokenID" apps/ios/Lakko` returns
  **zero** hits. There is no Secure Enclave key. Everything is an ordinary
  Keychain item at `AfterFirstUnlockThisDeviceOnly`. That is device-bound and
  protected by hardware-derived class keys, which is a defensible reading of the
  invariant — but the invariant names the Secure Enclave and nothing uses it.
- **web** — non-extractable WebCrypto, which the invariant explicitly permits.
  See the caveat in §3.2.

`kSecAttrSynchronizable` is never set explicitly anywhere in `apps/ios`; the code
relies on the Foundation default of `false` (noted in two comments,
`Auth/DeviceKeyStore.swift:61`, `Push/DevicePushId.swift:52`). The default is
correct, so this is an observation, not a finding.

---

## 4. Claim verification log

### 4.1 "The system is deployed and these invariants hold" — **FALSE for three of them**

| Claim | Verdict | Evidence |
|---|---|---|
| The KT log detects operator equivocation | **not enforced anywhere in the deployed system** | `transparency` has no systemd unit, is not in `deploy.sh:34`, has no nginx route; and the client check is off by default (`apps/web/src/transparency/config.ts` — "Unset → the whole log check is OFF") |
| Witness co-signing gives split-view resistance (C-57, ADR-0020/0039) | **not enforced** | `witness` likewise undeployed; `pinnedWitnessKeys()` returns `[]` when unset → gate OFF |
| Anti-censorship transports are wired (ADR-0026) | **not deployed** | `obfs-proxy` has no unit, no deploy entry, no nginx route |

This is not a disagreement with the vendor — `docs/security/external-review-scope.md`
§7 states it plainly and accurately. The divergence is that `CLAUDE.md`'s ADR
table describes ADR-0019/0020/0036/0039 as "Built", "Accepted", "landed
end-to-end", with no marker that the resulting service runs nowhere. A reader of
the primary developer document would conclude invariant #6 is enforced. It is
enforced only in its client-side TOFU-pin half. See **LAK-RC-03**.

### 4.2 Build-time security pins: none is supplied by the production build

Web pins referenced in `apps/web/src`
(`grep -rhoE 'VITE_LAKKO_[A-Z_]+'`, 12 distinct). Uncommented assignments in
`apps/web/.env.example`: **zero** (`grep -E '^VITE_LAKKO_[A-Z_]+=' ` → no output).
`.github/workflows/deploy-web.yml` sets no `VITE_*` variable in its build step
(lines 88-94 are `npm ci && npm run build` with only `CARGO_*`/`RUST_*` in `env:`).
`**/.env` is gitignored (`.gitignore:29`), so `actions/checkout` cannot supply one.

Therefore the CI-deployed web client ships with the transparency check off, the
witness quorum off, `VITE_LAKKO_DEVICE_ATTEST_KEY` unset (so the ADR-0015 pinned
verifying key falls back to fetching JWKS from the server it is meant to
distrust), and the ADR-0038 release notice dormant.

The manual path `just deploy-web` (`justfile:798`) builds on a developer machine
where an untracked `.env` *could* supply them. **I cannot observe that file and I
do not claim to know whether the running site has the pins.** That
unobservability is itself the point: a security-critical build input with no
tracked source of truth and no build-time assertion. See **LAK-RC-03**.

### 4.3 CI gating — 10 of 11 workflows never run automatically

| Workflow | Triggers |
|---|---|
| `spec-drift.yml` | `pull_request` |
| `ci.yml` | `workflow_dispatch` only — `push` and `pull_request` are **commented out** |
| `fuzz-quick.yml` | `workflow_dispatch` only (schedule commented out) |
| `censorship-lab`, `deploy-web`, `g3-4-mobile`, `recovery-e2e`, `repro`, `sframe-bridge`, `sframe-interop`, `sfu-e2e` | `workflow_dispatch` only |

So `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test --workspace`,
`reuse lint` and `cargo deny` gate nothing. `CLAUDE.md` states "CI blocks
otherwise" for SPDX headers and "`-D warnings` in CI" for clippy, and lists a
pre-commit checklist framed as enforced. Neither statement is true of the shipped
configuration. The scope brief §7 is accurate here; `CLAUDE.md` is not.
See **LAK-RC-07**.

The one automatic gate checks *documentation-spec drift*.

### 4.4 Test counts (measured)

`cargo test --workspace`, this machine, this session:

```
passed = 2771   failed = 0   ignored = 14   across 109 test binaries
```

`cargo test -p lakko-mls` → **291 passed, 0 failed, 0 ignored**.
`CLAUDE.md` claims 268 for that crate. The real figure is *higher*, so the claim
is stale rather than inflated — recorded for completeness, not as a finding.

The 14 ignored tests are all environment-gated (docker compose stacks, live
network, a live SFU) and each carries an explicit reason string. That is honest.

**Separately: 23 test bodies print `"skipping …"` and pass when a database env
var is unset** (8 in `services/auth`, 8 in `services/transparency`, 2 in
`services/discovery`, 1 in `services/witness`, e.g.
`services/auth/src/device_authorize.rs:482`). These are counted in the 2,771 as
passes. On a machine without `LAKKO_AUTH_TEST_DATABASE_URL` — which includes any
CI runner, and includes `ci.yml` itself — the DB-backed security tests for
device-grant redemption, WebAuthn action binding, the word vault and the KT
append gate report success without executing. See **LAK-RC-08**.

### 4.5 `#![forbid(unsafe_code)]` policy — the claim is inverted

`CLAUDE.md` states: *"`#![forbid(unsafe_code)]` on every crate, except
`lakko-ffi-uniffi` and `lakko-ffi-wasm` (which carry justified unsafe — document
it with a `// SAFETY:` comment)."*

Measured:

| Crate | Attribute | `unsafe {` blocks |
|---|---|---:|
| `lakko-ffi-uniffi` | `forbid(unsafe_code)` (`src/lib.rs:28`) | 0 |
| `lakko-ffi-wasm` | `forbid(unsafe_code)` (`src/lib.rs:68`) | 0 |
| `lakko-sframe-native` | `allow(unsafe_code)` (`src/lib.rs:70`) | **98** |
| `services/obfs-proxy` | no attribute | 0 |

The two named exceptions contain no unsafe. The crate containing all of the
project's unsafe is not named as an exception. 98 blocks against 38 `// SAFETY:`
comments — some comments cover several blocks in one function, so this is a ratio,
not a count of undocumented blocks; I did not audit them individually.
See **LAK-RC-04**.

### 4.6 Default-off security gates (enumerated)

Server side:

| Gate | Variable | Default |
|---|---|---|
| KT genesis-attestation append gate (ADR-0036) | `LAKKO_TRANSPARENCY_REQUIRE_GENESIS_ATTEST` | **off** (`services/transparency/src/main.rs:287`) |
| KT leaf-auth append gate (ADR-0019) | `LAKKO_TRANSPARENCY_REQUIRE_LEAF_AUTH` | on |
| Witness fan-out (ADR-0020/0039) | `LAKKO_TRANSPARENCY_WITNESS_URLS` | unset → dormant |
| Backup-head mint rate limit (ADR-0037 F5b) | `LAKKO_BACKUP_HEAD_MINT_RATE_LIMIT` | **0 = disabled** (`services/auth/src/backup_head.rs:98`) |
| Call-admission throttle (ADR-0033) | `LAKKO_..._CALL_ADMISSION_LIMIT` | 8; `0 = disabled` |
| Proof of work | `LAKKO_*_POW_EFFORT_BITS` | 8 bits (`lakko-pow-server/src/lib.rs:71`) |

Client side — **all default to off, and none is set by the CI build** (§4.2):
`VITE_LAKKO_TRANSPARENCY_URL`, `…_OPERATOR_KEY`, `…_WITNESS_KEY(S)`,
`…_WITNESS_THRESHOLD`, `VITE_LAKKO_DEVICE_ATTEST_KEY`, `VITE_LAKKO_RELEASE_KEY`,
`VITE_LAKKO_SFU_URL`.

A PoW effort of 8 bits is ~256 expected hashes. I did not benchmark the Equix
solver, so I make no claim about what that costs an attacker — but 8 bits is not
a meaningful barrier to the enumeration and spam surfaces it guards, and it is
worth a deep workstream's attention.

### 4.7 A transparency-log MISMATCH reaches `console.warn` and nothing else

`apps/web/src/contacts/client.ts:1628-1637`: when the log check returns
`"mismatch"` — the served key differs from the logged key, or the inclusion proof
failed, i.e. the exact signal the log exists to produce — the client calls
`console.warn` and proceeds. Only `"rollback"` throws
(`LogRollbackDetected`, `:1608-1610`). The comment says the fail-closed
refinement is "deferred to a `lakko-security` ruling". `apps/web/src/contacts/log-posture.test.ts:10-13`
pins this as the intended posture.

On a **first** observation the primary gate does not block either — it is TOFU and
pins what it is served. So on first contact, a key the log contradicts is accepted
with a developer-console warning. See **LAK-RC-09**.

### 4.8 Immaterial doc divergences (logged, not filed)

- `CLAUDE.md` says `lakko-crypto` provides HPKE and OPRF; it does not (the crate's
  own row in `CLAUDE.md` already corrects this — the correction is present and the
  original claim is in the crate's `Cargo.toml` description).
- `docs/security/threat-model.md` names the hard boundary as
  *"(client wasm/uniffi) ↔ (server gRPC / HTTP)"*. There is no gRPC anywhere in
  the tree; every service is axum over HTTP.
- The same document's invariant #7 omits "TPM 2.0", which `CLAUDE.md`'s copy of
  the same invariant includes. Two lists of the same invariant disagree.
- `CLAUDE.md` claims 268 tests in `lakko-mls`; measured 291 (§4.4).
- `crates/lakko-tuf` module doc describes "defense-in-depth delegation rejection";
  `CLAUDE.md` already notes the delegation walk superseded it. Moot either way —
  the crate has no consumer (§1.2).
- `services/edge` and `services/transport-rendezvous` are described in `CLAUDE.md`
  as Phase-0 stubs, accurately (28 and 29 lines respectively).

---

## 5. Findings filed

| Id | Title | Severity (finder's view; `audit-verify` calibrates) |
|---|---|---|
| LAK-RC-01 | DS journal is unauthenticated; an invite-link holder keeps read/write access forever | High |
| LAK-RC-02 | Unauthenticated channel rotation permanently destroys subscriber decryptability | High |
| LAK-RC-03 | Every transparency/witness/attestation gate is off in the deployed system | High |
| LAK-RC-04 | The project's only unsafe code is in the crate the unsafe policy does not cover, and it is not fuzzed | Medium |
| LAK-RC-05 | ~5,600 lines of Rust with no production caller, 2,819 of them shipped to browsers | Low |
| LAK-RC-06 | The web "device root key" is derived from the password and is not device-bound | Medium |
| LAK-RC-07 | No automated CI gate runs tests, lints, formatting, licensing or dependency checks | Medium |
| LAK-RC-08 | 23 database-backed security tests pass without executing when no test DB is configured | Medium |
| LAK-RC-09 | A transparency-log mismatch is reported only to the developer console | Medium |
| LAK-RC-10 | The threat model cites blinded presence tokens as an active invariant-#5 defence; none is wired | Low |

Severities are the finder's proposal. `audit-verify` calibrates, and LAK-RC-01 /
LAK-RC-02 share a root cause (*authorisation by possession of a published
pseudonym*) that a verification pass may prefer to merge.

---

## 6. Where I would look first, before any deep review runs

Ranked, from this map alone:

1. **`services/ds` and `services/channels`.** Two deployed services with no
   authorisation code. Everything rests on pseudonym secrecy, and the pseudonyms
   are published in invite links and resolvable from public handles.
2. **The gap between built and deployed.** The project's strongest claims
   (equivocation resistance, split-view resistance, censorship resistance) rest on
   three services that run nowhere and client gates that are off.
3. **`lakko-sframe-native` + `apps/shared/sframe-bridge`.** 98 unsafe blocks, a C
   ABI, no fuzzing, on the path every media frame from an untrusted SFU takes.
4. **The three-client divergence.** Every discipline is implemented three times
   from one spec. §3.3 already shows the device-key story differing materially
   across platforms; the project's own history (C-68, ADR-0044 F-A) shows the
   class is live.
5. **`services/auth` at 12,646 lines.** The largest service, holder of every
   server-side secret, and the one place where ACCESS recovery, the word vault and
   four attestation kinds meet.

---

## 7. Coverage statement — what I did NOT reach

Each of these is **unknown**, not clear.

- **I did not review any parser body.** §2.2 lists which parsers are fuzzed and
  which are not; I did not read the unfuzzed ones for bounds, indices or panics.
  I would have started with `services/ds/src/journal.rs` (unauthenticated, 1 MiB
  bodies) and the `lakko-sframe-native` C ABI.
- **I did not verify the iOS NSE / web SW / Android background-decrypt no-write
  discipline (C-59).** I read the claim; I did not trace the code paths.
- I **did** check `push-gw` authorisation after drafting §2.1, and it belongs in
  the body rather than here: `grep` for `require_session|AuthenticatedAccount`
  over `services/push-gw/src/` returns nothing outside the three outbound
  provider modules. `notify` (`notify.rs:176`) and `notify_group` (`:309`) take a
  pseudonym in the JSON body and are gated by **proof of work alone**
  (`:180`, `:313`). Unlike DS and channels this is *not* obviously exploitable,
  because the pseudonyms genuinely are not published: the device pseudonym lives
  in the keychain and the group pseudonym is the ADR-0042 `push_gid`, derived
  from a secret and algebraically independent of the DS `group_id` — which is
  precisely what ADR-0042 bought. I did **not** review the subscribe path or
  whether a former member's `push_gid` knowledge lets them wake a group
  (ADR-0043's stated residual). That is the next thing I would check.
- **I did not benchmark the PoW effort.** §4.6 states 8 bits and draws no
  conclusion about cost.
- **I did not run `cargo deny`, `cargo audit`, `clippy`, `reuse lint`, or any
  fuzz target.** No dependency-advisory or lint claim appears anywhere above.
- **I did not open `apps/ios` or `apps/android` beyond the key-storage grep** in
  §3.3, nor read any client protocol logic.
- **I did not review `lakko-mls`, `lakko-calls`, `lakko-transport` or
  `lakko-channels` internals at all** — those are the deep workstreams' surface
  and this engagement does not run them.
- **I did not verify what the live site is actually built with** (§4.2). I
  verified what the tracked configuration and the CI path produce.
- **I did not review the vendored `webauthn-rs-core` fork** beyond noting it
  exists and that `services/auth/tests/fork_delta_guard.rs` is claimed to pin it —
  I did not run that guard.
