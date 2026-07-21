<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0038: Signed "update available" notice + store deep-link

- **Status:** Accepted (maintainer approved 2026-07-21; freshness DISABLED in v1 —
  see the resolved Open question below; implementation via the S1–S6 decomposition)
- **Date:** 2026-07-21
- **Context:** GA — client release / update operations. The store-app slice of
  auto-update: what a client that cannot self-swap its binary can still do to
  tell the user a newer version exists.

## Context

Lakko already has a binary auto-update design on paper: `crates/lakko-tuf` is a
complete, offline TUF v1.1 metadata verifier (`Root` / `Snapshot` / `Targets` /
`Timestamp` chain-of-trust walk, delegated-targets resolution, `verify_target_bytes`
length + SHA-256 gate, mirror failover). It is built for the classic
fetch-metadata → verify → **atomically swap the installed binary** flow. That flow
is inapplicable to a client delivered through an app store: an App Store / Play
build **cannot self-swap its binary** — the store owns the update channel, and the
only actionable thing a store app can do about a new version is send the user to
its store listing. `lakko-tuf` is also currently wired to **nothing** (it is a
dependency of no client or service; see the CLAUDE.md module map), so nothing today
tells a store-delivered client that it is out of date at all.

There is a smaller, universally-applicable slice of "auto-update" that **does**
apply to store apps: a client can periodically fetch a small **signed release
manifest**, verify it against a **build-time-pinned** key, compare its own
build-time release number to the manifest's, and — if it is behind — show a
dismissible "update available" banner that, on mobile, deep-links to the store.
This is the applicable slice, and this ADR designs it.

**The version numbers are currently inconsistent placeholders**, which shapes the
design. The web + Rust workspace report `0.0.0`
(`apps/web/package.json`, root `Cargo.toml`); iOS carries a placeholder
`CFBundleShortVersionString` (`apps/ios/Lakko/Info.plist`); Android carries
`versionName = "0.1.0-dev"` / `versionCode = 1`
(`apps/android/app/build.gradle.kts`). The server `/version` endpoint reports the
**server crate** version, which is useless as a *client*-version source. So there is
no single, coherent, cross-platform version string a client could compare today —
and even once the placeholders are fixed, the three stores use divergent version
schemes (App Store semantic version vs. Play `versionName` string vs. the monotone
`versionCode` integer). Any design that compares the cosmetic display version is
unworkable across platforms.

Every primitive this needs already exists and is audited. The `lakko-attest`
fixed-layout-Ed25519 template (`crates/lakko-attest/src/backup_head.rs`:
`BackupHead` = `format_version(1) ‖ … ‖ signed_at(8 BE i64) ‖ Ed25519_sig(64)`,
domain `b"lakko/backup-head/v1"`, `sign` / `open` / `verify_*`, bounded fuzzable
parser) is a proven shape to mirror. The build-time pin pattern
(`apps/ios/Lakko/Transport/BootstrapPin.swift`,
`apps/android/.../transport/BootstrapPin.kt`,
`apps/web/src/auth/device-attest-key.ts`) is the template for pinning the
manifest-verifying key + its fetch host, and it is **default-empty → dormant** so
an unset pin can never arm insecurely. For retrieval, the native clients reuse their
existing tunnel-aware HTTP stack (the ADR-0026 anti-censorship transport when armed)
and the web client a plain same-origin `fetch` — the Ed25519 signature, not the
transport, authenticates the manifest (`DohFetcher::fetch_pinned_bytes`,
`crates/lakko-transport/src/doh_fetch.rs:365`, remained available but was not needed).
The static manifest is served behind the existing `lakko.me` nginx vhost
(`infra/nginx/sites-available/lakko.conf`) from a dedicated root — zero new
infrastructure. And the
dismissible top-level banner already has a visual template on all three clients
(`IdentitySecurityBanner` — web `apps/web/src/auth/guards.tsx`, iOS
`apps/ios/Lakko/UI/Auth/IdentitySecurityBanner.swift`, Android
`.../ui/auth/IdentitySecurityBanner.kt`), and web already runs a separate
service-worker-driven `SwUpdateBanner` (`apps/web/src/sw/SwUpdateBanner.tsx`) for
its own update path.

## The load-bearing design decision

Three things must be stated precisely up front, because getting them wrong would
either over-build the feature or mis-sell its security.

1. **This is NOT full binary auto-update — it is a signed *notice* + a store
   link.** The client never fetches, verifies, or installs a binary. It fetches a
   signed integer, compares, and (if behind) shows a banner that opens the store.
   `lakko-tuf` remains the right tool for the *sideloaded / desktop* binary
   auto-update (a genuinely different concern that owns metadata + target-hash
   verification + atomic swap); it stays out of scope here and un-wired. For a
   store app the only actionable output is "go to the store", which needs only a
   signed version number — not a metadata chain.

2. **A single monotone integer `release`, decoupled from the cosmetic display
   version.** The manifest and each client carry a plain `u64` release counter
   (`CURRENT_RELEASE`, starting at `1`), compared as integers. This is unambiguous
   and identical across platforms, and it sidesteps the placeholder-version
   mismatch (web/Rust `0.0.0`, iOS placeholder, Android `0.1.0-dev`) and the
   per-store scheme divergence (semver vs. `versionName` vs. `versionCode`)
   entirely. The store *display* version stays each platform's own scheme (what the
   store listing shows); the `release` integer is the sole thing this feature
   compares, and it is bumped in lock-step at each release cut. Comparing the
   display version was considered and rejected (Alternatives).

3. **The security property is anti-forgery, and the check is fail-safe-toward-
   silence.** The signature stops a malicious server or a MITM from **forging** a
   nudge ("update now" → a phishing push toward a hostile store link or a panic) or
   from **forging** a `min_supported` floor. It does **not** meaningfully stop
   *suppression* — an attacker who strips or replays an old manifest gets "no
   banner" or "an older asserted latest", which is exactly the LOW-impact outcome a
   suppressor wants, and the client's own fail action on an unverifiable manifest is
   also "show no banner". That is acceptable: suppressing the notice keeps the user
   on a version they **already run** (no confidentiality or integrity break; the
   store link the button opens is itself safe), so the suppress/replay residual is
   **LOW** and accepted. This is an **availability / anti-phishing** control, not an
   E2EE control — so where it is "fail-safe", that means *fail toward showing no
   nag*, which is a different thing from invariant #4's *fail-closed-refuse-the-
   connection*. The two must not be conflated.

**The manifest is global, not account-bound.** This is the one structural deviation
from every other `lakko-attest` blob (`ActionAttestation`, `GenesisAttestation`,
`BackupHead` all bind `account_id`). A release notice is the same for every user,
so it carries **no `account_id` and no per-user field**. That deviation is what
makes this the *simplest* attest primitive in the codebase: no per-account state,
no auth endpoint, no database, no monotonic counter — the publisher signs one blob
per release and nginx serves it statically. State this explicitly so a reviewer does
not look for the (absent) account binding.

The maintainer must accept this framing — "a signed monotone-integer notice + a
store link, anti-forgery not anti-suppression, global not account-bound, fail-safe-
toward-silence when unpinned/unverifiable" — before implementation.

## Decision

Add a **signed release manifest** the client fetches, verifies against a
build-time-pinned dedicated release key, and turns into a dismissible "update
available" banner (with a store deep-link on mobile). It reuses the `lakko-attest`
fixed-layout-Ed25519 template; the deviations from ADR-0037 are (a) the blob is
**global** (no `account_id`), (b) the signing key is **dedicated** (not the shared
attest key), and (c) there is **no server endpoint, no state, no migration** — the
manifest is signed offline and served statically.

### 1. `ReleaseManifest` primitive (extend `lakko-attest`)

A fixed-layout, domain-separated, Ed25519-signed blob, mirroring `BackupHead`
(`crates/lakko-attest/src/backup_head.rs`):

```text
wire          = format_version(1) ‖ latest_release(8, big-endian u64)
                ‖ min_supported_release(8, big-endian u64)
                ‖ signed_at_unix_millis(8, big-endian i64)
                ‖ Ed25519_sig(64)                                   = 89 bytes
signing input = DOMAIN ‖ format_version ‖ latest_release
                ‖ min_supported_release ‖ signed_at
DOMAIN        = b"lakko/release-manifest/v1"   (NEW — distinct from backup-head/v1,
                                                genesis-attest/v1, device-attest/v1)
```

- `format_version` is the blob-layout version (`= 1`), distinct from the payload
  `latest_release` / `min_supported_release` counters (do not conflate the field
  names).
- `latest_release` is the newest release the publisher has shipped;
  `min_supported_release` is the oldest release the publisher still supports
  (`min_supported_release <= latest_release`, a publisher invariant). Both are
  monotone `u64` counters, compared as integers against the client's build-time
  `CURRENT_RELEASE`.
- `signed_at_unix_millis` is the **mint timestamp** (not an expiry), carried in Unix
  milliseconds (the clients work in milliseconds natively) — the same choice, and
  the same rationale, as `BackupHead`: the staleness window is a *client* policy, so
  the manifest carries the raw mint time and the client applies its own window.
- `DOMAIN` MUST be new and distinct from the three existing attest domains so a
  release manifest can never be replayed as (or forged from) a device / genesis /
  backup-head attestation under a shared key — the codebase forbids domain reuse,
  and the `lakko-attest` suite's three-way `domains_do_not_cross` test is extended
  to a four-way check.

Verify + status surface:

- `verify_release_manifest(bytes, verifying, now_unix_millis, max_staleness_millis)
  -> ReleaseManifest` — signature + freshness, returning the parsed manifest so the
  caller reads `latest_release` / `min_supported_release`. **No account or hash
  binding** (there is none). Freshness bounds a replay of a genuine-but-old manifest
  asserting stale counters (its integrity value is modest — see § Freshness — but it
  is carried for parity and to give the maintainer a dial).
- `release_status(current_release, manifest) -> UpdateStatus` — a **pure** classifier:

  ```text
  UpdateRequired   if current_release < manifest.min_supported_release
  UpdateAvailable  if current_release < manifest.latest_release   (and not Required)
  UpToDate         otherwise
  ```

  `UpdateStatus { UpToDate | UpdateAvailable | UpdateRequired }`. `Required` is
  checked first (it strictly implies behind), then `Available`, else `UpToDate`.

`#![forbid(unsafe_code)]`, bounded fail-closed parser, unit/property tests mirroring
the `backup_head.rs` suite (round-trip, tamper-any-field-breaks-sig, wrong-key,
stale, the four-way domain-crossing test, `release_status` boundary cases including
`current == latest`, `current == min_supported`, and `min == latest`), plus a
`release_manifest_open` fuzz target in `crates/lakko-attest/fuzz` mirroring
`backup_head_open` (the bytes are attacker-controlled — a static file a hostile
serving tier / MITM can craft — reaching the bounded parser before any state).

### 2. Pinned key + fetch host — a dedicated release-signing key, pin-only

A **dedicated** release-signing Ed25519 keypair (signing-role isolation, § 3), whose
32-byte verifying key + fetch host are pinned at build time on each client via a new
`LakkoRelease{Key,Domain,Path}` slot, mirroring `BootstrapPin` exactly:

| Native platform | Pin source |
|---|---|
| iOS | `Info.plist` `LakkoReleaseKey` (+ `LakkoReleaseManifestUrl`, default `https://lakko.me/.well-known/lakko-release`) |
| Android | `BuildConfig.LAKKO_RELEASE_KEY` (+ `LAKKO_RELEASE_MANIFEST_URL`, gradle properties) |
| Web | `VITE_LAKKO_RELEASE_KEY` (+ `VITE_LAKKO_RELEASE_MANIFEST_URL`, default the same-origin `/.well-known/lakko-release`) |

> **Shipped note.** The fetch host is carried as a single full **manifest URL**
> (default `https://lakko.me/.well-known/lakko-release`), not a separate
> domain/path pair — the client does a plain HTTP GET of that URL (§ 4), so one URL
> is all it needs.

All **default-empty**. When the key is unset / blank / malformed, the check is
**dormant** — the client never fetches, never verifies, never shows a banner. There
is no insecure default and, unlike `DeviceAttestKey` (which falls back to a JWKS
endpoint when its pin is unset, trusting the auth origin), the release key is
**pin-only**: there is no serving-tier fallback, so an unpinned build simply has the
feature off. This is *stronger* than the `DeviceAttestKey` posture and is the right
call here because there is no per-request key rotation to discover — the release key
rotates on a release-engineering cadence, not at runtime.

Fetch. The manifest is the **RAW 89 signed bytes** — no JSON envelope, no base64
wrapper — served as a static `application/octet-stream` file; the client hands the
fetched bytes straight to `verify_release_manifest`, and the Ed25519 signature (not
the transport) is the trust anchor either way. A tight response-size cap bounds a
hostile origin (the manifest is a fixed 89 bytes).

- **Native** (iOS + Android): the platform HTTP client, routed through the
  anti-censorship tunnel when it is armed (ADR-0026) — iOS via
  `AntiCensorshipTransport.perform`, Android via the `AntiCensorshipEngine` — so a
  censored network cannot block the check. (An earlier draft routed this through
  `DohFetcher::fetch_pinned_bytes`; the shipped clients reuse their existing
  tunnel-aware HTTP stack instead, since the signature — not IP pinning —
  authenticates the manifest.)
- **Web**: a plain same-origin `fetch("/.well-known/lakko-release")`. The web client
  is same-origin with the manifest host, so no IP pinning applies; the Ed25519
  signature is the trust anchor either way.

### 3. Signing key — dedicated, NOT the shared attest key (the deliberate inverse of ADR-0037 §3)

ADR-0036 §3 and ADR-0037 §3 **reuse** the auth device-attest signing key for their
mint endpoints. This ADR deliberately does the **opposite**: a **dedicated**
release-signing key, held by release engineering, not the auth service. The reason
is that the two roles genuinely differ here (whereas genesis + backup-head were both
auth-service *runtime* signing over an OPAQUE-authenticated request):

- The attest key is an **auth-service runtime** key — online, per-request, in the
  auth service's trust + rotation domain.
- The release key is a **build / publisher** key — used offline at each release cut
  to sign one blob, in the release-engineering trust domain, with its own lifecycle
  and blast radius.

Reusing the attest key would couple release signing to the auth service's key
management and put a build-time signing role inside a runtime service's key. A
dedicated key keeps the blast radii separate: a compromised auth attest key must not
let an attacker forge "update now" phishing nudges, and vice versa. The dedicated
key has no JWKS endpoint (§ 2, pin-only) — it is distributed solely as the
build-time pin.

### 4. Manifest hosting — static, zero new infrastructure

The signed manifest is served statically behind the existing `lakko.me` nginx vhost
(`infra/nginx/sites-available/lakko.conf`) at the well-known path
**`/.well-known/lakko-release`**, as a raw `application/octet-stream` blob with
`Cache-Control: no-store` (so an edge cache cannot pin a stale manifest, and a new
manifest propagates on the next fetch — the signature is what is trusted, not cache
freshness, and the client applies its own staleness window). It is a dedicated
`location = /.well-known/lakko-release` block, mirroring the existing dedicated
`location = /sw.js`. No new service, no new vhost, no database.

The one deploy subtlety, resolved: the blob is served from a **separate** on-disk
root `/var/www/lakko-release/` via an nginx `alias` (NOT the web SPA root
`/var/www/lakko-web/`), because `just deploy-web` rsyncs the SPA with `--delete` and
would otherwise wipe the manifest on the next web deploy. The publisher `scp`s the
minted blob to that alias target (`/var/www/lakko-release/lakko-release.bin`) and it
is served immediately — no service restart, no nginx reload. See § 7 and the
`infra/README.md` runbook.

### 5. Client UX contract (high level — the ux-ui pass details it)

Per-status behaviour, cloning the `IdentitySecurityBanner` visual template
(dismissible, top-level, theme-variable-driven):

| `UpdateStatus` | Behaviour |
|---|---|
| `UpToDate` | Nothing. No banner. |
| `UpdateAvailable` | A **dismissible** top-level banner: "A new version of Lakko is available." On **mobile**, an **Update** button that opens the store (§ 6). On desktop/web there is no store button (§ web note). |
| `UpdateRequired` | The manifest carries `min_supported_release` and the core reports `UpdateRequired`, but **no blocking UI is built in v1** (per "do not bake in future features"). v1 renders the **same dismissible banner** as `UpdateAvailable`. A blocking / non-dismissible "you must update to continue" screen is a clean future addition — the field is already carried, so it needs no wire change. |

**Web note.** The web client auto-updates via its service worker and already shows
`SwUpdateBanner` ("A new version of Lakko is ready — Reload") on a real SW update.
Web has **no store to deep-link to**, so it keeps `SwUpdateBanner` as its update
mechanism and does **not** render a second store-linking banner (a double banner
would be confusing). The shared verify core (`verify_release_manifest` +
`release_status`) still lands in **wasm** so the primitive is available and testable
on every platform and there is one codepath; whether the web additionally surfaces
any manifest-driven notice is a UX call (default: no — the SW is the web's update
path). See § Open questions.

The concrete banner copy, the poll cadence + trigger (boot / foreground / periodic),
dismissal persistence, and the `UpdateRequired`-vs-`UpdateAvailable` visual weight are
a **ux-ui pass** — this ADR fixes only the contract (states + the store-link
behaviour), per the UX-first rule.

### 6. Store deep-link — build-time config, default-empty (honest rollout condition)

The outbound store URLs are build-time config, **default-empty**, because **no
production store identity exists yet** — every id is the placeholder `me.lakko.dev`
(`apps/android/app/build.gradle.kts` `applicationId`, `apps/ios/project.yml`
`PRODUCT_BUNDLE_IDENTIFIER`), there is **no App Store numeric id** and **no Play
listing URL** anywhere in the tree.

- **iOS**: a full outbound App Store URL from `Info.plist` `LakkoAppStoreUrl`
  (e.g. `https://apps.apple.com/app/id…`), default-empty.
- **Android**: a full outbound Play URL from `BuildConfig.LAKKO_PLAY_STORE_URL`
  (e.g. `https://play.google.com/store/apps/details?id=…`), default-empty.

The client stores **one complete store URL per platform** rather than assembling a
`market://` / `itms-apps://` scheme from an id — simpler, and the URL is opaque to
the notice logic.

When the store URL does not exist, the **Update button is hidden** (the banner still
shows the "a new version is available" text — it is truthful and useful, and a user
can update through the store manually). The feature **ships and shows the banner
now**; the button **arms when the store URLs are configured**. This is an
**accepted, documented rollout condition**, mirroring the default-empty pin pattern
(a capability present in code and dormant until its production identity exists) — not
a blocker. A build-time dev override (`LakkoReleaseCheckDevOverride` /
`LAKKO_RELEASE_CHECK_DEV_OVERRIDE`, default off) forces the banner to show even
without a store URL, for pre-launch dogfood testing.

### 7. Publisher side — a mint-and-drop tool / `just` recipe

Shipped as `tools/release-manifest` (the `lakko-release-manifest` bin, workspace
member `lakko-release-manifest-tool`) + two `just` recipes. `just gen-release-key`
generates the dedicated release **signing** keypair once (a `0600` seed the
maintainer keeps offline / never commits, and the base64url public key to paste into
the client build pins). Then at each release cut, `just mint-release-manifest
<latest> <min_supported>`: reads the offline seed, builds
`ReleaseManifest::new(latest, min_supported, now)`, `sign`s it, self-verifies the
result, and writes the **raw 89 bytes** to a file (`release-manifest.bin`). The
maintainer `scp`s that raw blob to the static host's alias target
`/var/www/lakko-release/lakko-release.bin` (§ 4) — served immediately, no restart,
no reload. The signing-key material and its rotation policy are a release-engineering
/ infra concern, out of scope for this ADR beyond "dedicated, offline, pinned on
clients"; the `infra/README.md` runbook has the full flow.

## Freshness model + residual

The staleness window is a **client-side** knob (`LAKKO_RELEASE_STALENESS_MILLIS` or
the per-platform equivalent), the same shape as `BackupHead`'s. Its integrity value
here is **modest and should not be oversold**:

- The signature is what prevents forging a *higher* `latest_release` or `min_supported`
  (a phishing nudge). Freshness only bounds how long a **replayed genuine old**
  manifest can assert **stale** counters.
- The client's fail action on an unverifiable (including too-stale) manifest is "show
  no banner". So a suppressor who serves a stale manifest gets the same "no nag"
  outcome as one who strips it entirely — freshness does not deny them that. Its
  real benefit is narrow: it stops an attacker from *pinning* the notice to an old
  asserted `latest` indefinitely (after the window, that old manifest fails and the
  client stops trusting it), and it flags a serving tier that has silently stopped
  re-publishing.
- **Operational coupling to surface honestly.** Because the manifest is served
  statically and its `signed_at` ages, a freshness window shorter than the release
  cadence means the publisher must **re-sign and re-publish the manifest on a cron
  within the window even when there is no new release**, or the check silently goes
  dormant everywhere (every client sees `Stale` → no banner). That is a real
  operational burden. Two defensible resolutions, the maintainer's call (Open
  questions): (a) a **generous** window (or a cron re-sign inside a tighter window),
  or (b) **disable freshness** (`max_staleness = i64::MAX`) and accept the LOW
  replay residual — given the fail-action is silence and the threat is LOW, (b) is a
  reasonable default and keeps hosting a pure drop-a-file operation.

**The residual, stated plainly.** An attacker who controls the serving tier / a MITM
can **suppress** the notice (strip the file → no banner) or **replay** a genuine old
manifest (→ an older asserted `latest`). Both are **LOW**: they keep the user on a
version they already run, break no confidentiality or integrity, and the store link
the button opens is unaffected. They cannot **forge** a nudge or a `min_supported`
floor (that needs the release signing key). This is an availability/anti-phishing
posture, accepted.

## Alternatives

- **Wire up the full `lakko-tuf` chain for store apps.** Rejected — over-built and
  half-inapplicable. TUF's value is verifying a **binary artifact** (metadata chain
  → target length + SHA-256 → atomic on-disk swap); a store app **cannot swap its
  binary**, so TUF's entire install half (`verify_target_bytes` + the atomic-swap
  glue) is dead weight for it, and its metadata half (a four-role signed chain +
  delegation walk + mirror failover) is far more machinery than a signed integer
  needs. TUF remains the right tool for the **sideloaded / desktop** binary
  auto-update (a separate, genuinely different concern this ADR does not touch and
  does not un-scope). For a store app the only actionable output is "go to the
  store", and that needs one signed number, not a TUF repository.

- **An unsigned `/version`-style endpoint the client polls.** Rejected — spoofable.
  A malicious server or a MITM could **fake** an "update now" notice (a phishing
  nudge toward a hostile store link, or manufactured urgency), or **suppress** a
  genuine security nag, with no cryptographic cost. Signing is cheap defence against
  the forge case. (The existing server `/version` also reports the *server* crate
  version, which is not even the right datum — it says nothing about the client
  release.) An Ed25519 signature over an 89-byte blob is a negligible addition and
  closes the forgery vector that is the whole point of the control.

- **Reuse the `DeviceAttestKey` pin / the shared auth attest signing key.** Rejected
  for signing-role isolation — the deliberate inverse of ADR-0037 §3 (§ 3). The
  attest key is an auth-service *runtime* key; the release key is a *build /
  publisher* key with a different lifecycle, trust domain, and blast radius. Reusing
  would couple release signing to the auth service's key management and let a
  compromise of one role implicate the other (a stolen auth key forging "update now"
  phishing, or vice versa). A dedicated key keeps the blast radii separate; the
  domain separator would make reuse *cryptographically* safe, but the *operational*
  coupling is the reason to reject it here.

- **Compare the store display version instead of a monotone integer.** Rejected
  (§ 2). The display versions are inconsistent placeholders today (web/Rust `0.0.0`,
  iOS placeholder, Android `0.1.0-dev`) and, even fixed, diverge per store (semver
  vs. `versionName` vs. `versionCode`), so a cross-platform display-version compare
  is unworkable and ambiguous. A single monotone `u64` `release`, bumped in
  lock-step and compared as integers, is unambiguous and identical on every
  platform, while the store *display* version stays each platform's own scheme.

## Consequences

- **Positive.** Store-delivered clients gain a **signed**, forge-resistant "you are
  behind" signal + a store deep-link — the applicable slice of auto-update that TUF
  cannot serve for a store app. Reuses the audited `lakko-attest` fixed-layout-
  Ed25519 template, the `BootstrapPin` default-empty pin pattern, and each client's
  existing HTTP path (the tunnel-aware native transport / same-origin web `fetch`),
  so the new surface is small. **No server
  endpoint, no state, no migration, no new infrastructure** — the manifest is signed
  offline and served as a static file (the manifest being *global, not
  account-bound* is what buys this simplicity). The `min_supported_release` field
  future-proofs a blocking-update screen at no wire cost.

- **Negative / new risks.** A new dedicated release **signing key** to hold offline,
  distribute as a build-time pin on three clients, and rotate (a release-engineering
  key-management responsibility — but isolated from the auth key by design). A fourth
  attest domain constant that must not collide with the existing three (mitigated by
  the distinct `lakko/release-manifest/v1` separator + the extended four-way
  domain-crossing test). If freshness is enabled with a tight window, an operational
  duty to re-sign the static manifest on a cron or the check silently goes dormant
  (§ Freshness — resolvable by a generous window or disabling freshness). The
  suppress/replay residual is real and permanent under this design, but LOW (no C/I
  break; the store link is safe). The notice is only as strong as the pinned release
  verifying key: a compromised release **signing** key lets an attacker forge nudges
  — the store-link phishing risk this control is meant to prevent — so the key's
  offline custody is load-bearing. Honest scope, stated.

- **Neutral.** Additive and non-breaking: unpinned builds have the feature dormant
  (fail-safe-toward-silence), so there is no flag-day and no forced rollout. The
  store button ships hidden and arms only when a production store identity exists
  (§ 6) — a documented rollout condition, not a blocker. Web keeps its existing SW
  update path unchanged.

### Invariants touched

- **#1 (no plaintext PII on the server) — holds trivially.** The manifest is
  **global**: it carries no `account_id`, no username, no per-user field at all —
  three monotone integers and a signature. There is nothing per-user to leak.
- **#2 (server sees only ciphertext + public artefacts) — holds.** The manifest is a
  public, Ed25519-signed artifact carrying no key material; nginx serves it as opaque
  static bytes and parses nothing. No new server secret is introduced (the signing
  key is offline / release-engineering, never on the serving tier).
- **#4 (no silent downgrade / fail-closed) — adjacent, not an E2EE control.** This is
  an **availability / anti-phishing** control. Its "fail-safe" is *fail toward showing
  no nag* (an unpinned or unverifiable manifest → no banner), which is distinct from
  invariant #4's *fail-closed-refuse-the-connection*. It touches no E2EE path and can
  never downgrade one — surfaced here for completeness, not because a message-path
  invariant is at stake.
- **Invariant-index touchpoints (S6, reconciled 2026-07-21):** this is an
  **availability / anti-phishing** control (a signed public artifact, client-verify,
  fail-toward-silence), **not** an E2EE / MLS / call invariant — so it takes **no new
  indexed C-number**, and the invariant index stays at 8 I + 58 C + 15 T
  (`just audit-index-check` clean). The accepted LOW suppress/replay residual is
  recorded in `docs/security/known-gaps.md` (G-32), which the index tooling does not
  count. The CLAUDE.md ADR-table row + module-map (`lakko-attest` gains
  `ReleaseManifest`; both FFI rows gain the release verify surface; `tools/` gains
  the publisher CLI) + fuzz-list (`release_manifest_open`) entries landed in the same
  reconcile.

This ADR is low on the crypto/MLS/transport/auth risk axis (a static signed integer
+ a pinned verifier, no message-path or key-material change), but it **does add an
Ed25519 verify path + a fourth attest domain + a new signing-key role**, so a
`lakko-security` pass on the primitive + the pin wiring before merge is worthwhile
(maintainer-invoked) — mainly to confirm the domain separation, the pin-only
fail-safe, and that the store-link cannot be redirected by an unsigned field.

## Open questions

- **Freshness — RESOLVED (2026-07-21): DISABLED in v1.** The maintainer chose to not
  make the client picky about manifest staleness, given the re-sign-cadence
  operational coupling (a static manifest whose `signed_at` ages would silently make
  the check go dormant everywhere unless the publisher re-signs on a cron even with no
  new release) and the LOW threat (a suppressor serving a stale manifest only hides an
  unobtrusive nag — no confidentiality/integrity break; § Freshness). Concretely: the
  core keeps the `max_staleness_millis` parameter (flexible), and the clients pass
  `i64::MAX`, which short-circuits the staleness gate so a manifest is never rejected
  as `Stale`. Re-enabling a generous window later is a one-line client change.
- **Poll cadence + trigger.** When the client checks (boot / app-foreground /
  periodic background) and how it persists banner dismissal so it does not re-nag
  every launch — a UX/product tuning decision for the ux-ui pass.
- **Web participation.** Whether the web client surfaces any manifest-driven notice
  or relies solely on its existing `SwUpdateBanner` (default: SW only, to avoid a
  double banner) — the wasm verify core lands regardless, for one codepath and
  testability.
- **Store production identity.** The App Store numeric id + the Play listing do not
  exist yet (everything is `me.lakko.dev`); the Update button arms when they are
  minted (§ 6). A rollout condition, tracked, not a design gap.
- **`UpdateRequired` blocking UI.** A non-dismissible "must update" screen is deferred
  (do not bake in future features); the field is carried so it is a clean future
  addition with no wire change. Whether/when to build it is a product call.
- **A persisted highest-`latest_release`-ever-seen floor.** A client could remember
  the highest `latest_release` it has verified and refuse a manifest asserting a
  *lower* one, bounding a downgrade of the notice itself. Not built in v1 (the
  manifest is not account-bound, so there is no natural per-account store, and the
  threat is LOW). A clean future tightening if the suppress/replay residual is ever
  judged worth closing.
- **Dedicated release JWKS vs. pin-only.** v1 is pin-only (§ 2, stronger than the
  `DeviceAttestKey` JWKS-fallback posture). If a future need arises to rotate the
  release key without a client rebuild, a signed-config-delivered key (ADR-0026-style
  bootstrap) is the clean path — deferred.

## Sprint decomposition

Each step names the ADR-0037 / ADR-0036 file it mirrors.

- **S1 — `ReleaseManifest` primitive + fuzz.** New
  `crates/lakko-attest/src/release_manifest.rs` (mirror `backup_head.rs`): the
  89-byte fixed layout, domain `lakko/release-manifest/v1`, `sign` / `open` /
  `verify_release_manifest` (signature + freshness) + the pure
  `release_status(current, manifest) -> UpdateStatus`; re-export from `lib.rs`
  alongside `backup_head` / `genesis`. Extend the `domains_do_not_cross` test to a
  **four-way** check (release ↔ backup-head ↔ genesis ↔ device). Fuzz target
  `release_manifest_open` in `crates/lakko-attest/fuzz` mirroring `backup_head_open`.
- **S2 — FFI verify + status.** Expose `verify_release_manifest` + `release_status`
  (+ the `UpdateStatus` enum) on UniFFI (Swift/Kotlin) + wasm (web), mirroring the
  `verify_backup_head` FFI exposure in `crates/lakko-ffi-uniffi/src/lib.rs` +
  `crates/lakko-ffi-wasm/src/backup.rs`. **No sign on the FFI** — signing is the
  publisher tool's job (S3), never a client's.
- **S3 — publisher mint tool + static hosting.** The `lakko-release-manifest` bin
  (`tools/release-manifest`, workspace member `lakko-release-manifest-tool`) —
  `keygen` (an offline `0600` seed + the base64url pubkey) + `mint` (the RAW 89-byte
  blob, self-verified) — plus `just gen-release-key` / `mint-release-manifest`.
  Served as the raw blob at `/.well-known/lakko-release` from a SEPARATE root
  `/var/www/lakko-release/` via an nginx `alias` with `Cache-Control: no-store`
  (§ 4), so a web deploy's `rsync --delete` cannot wipe it. Key custody / rotation is
  a release-engineering concern, out of scope beyond "dedicated, offline, pinned".
- **S4 — native wiring (iOS + Android).** Add the build-time pins (`LakkoReleaseKey`
  + `LakkoReleaseManifestUrl` / `LAKKO_RELEASE_KEY` + `LAKKO_RELEASE_MANIFEST_URL`)
  mirroring `BootstrapPin.swift` / `BootstrapPin.kt` (default-empty → dormant); fetch
  via the platform HTTP client, routed through the ADR-0026 anti-censorship tunnel
  when armed (iOS `AntiCensorshipTransport.perform`, Android `AntiCensorshipEngine`);
  `verify_release_manifest` + `release_status`; a dismissible banner cloning
  `IdentitySecurityBanner`; the store deep-link — a single default-empty full store
  URL (`LakkoAppStoreUrl` / `LAKKO_PLAY_STORE_URL`), the Update button hidden until it
  is set (a `LakkoReleaseCheckDevOverride` / `LAKKO_RELEASE_CHECK_DEV_OVERRIDE` forces
  the banner for dogfood). Add the build-time `CURRENT_RELEASE` per client (default 1)
  + a pure `UpdateBannerDecision` cadence policy. **Design the UX via the ux-ui skill
  first.**
- **S5 — web wiring.** Add the `VITE_LAKKO_RELEASE_KEY` pin + a same-origin
  `fetch("/.well-known/lakko-release")` + the shared wasm verify core
  (`apps/web/src/update/release-check.ts`), so the primitive and one codepath exist
  on web; **keep `SwUpdateBanner`** as the web update path and do **not** add a second
  store-linking banner (web has no store — the module stays DORMANT).
- **S6 — docs reconcile.** Flip this ADR's Status to Accepted; add the CLAUDE.md ADR
  table row + module map update (`lakko-attest` gains `ReleaseManifest`; both FFI
  rows gain the release verify surface; `tools/` gains the publisher CLI; the fuzz
  list gains `release_manifest_open`). Record the accepted LOW suppress/replay
  residual in `docs/security/known-gaps.md` (G-32); **do not** add a new indexed
  invariant to `docs/security/invariant-index.md` — this is an availability /
  anti-phishing control, not an E2EE / MLS / call invariant, so the index stays at
  8 I + 58 C + 15 T.

## References

- ADR-0037 — auth-signed backup-head rollback commitment; the `lakko-attest`
  fixed-layout-Ed25519 primitive (`BackupHead`), the `signed_at_unix_millis`
  freshness-as-client-policy pattern, and the fuzz/domain-crossing test discipline
  mirrored throughout. Deviations here: global (no `account_id`), dedicated signing
  key, no server endpoint / state / migration.
- ADR-0036 — auth-attested KT-log genesis; the `lakko-attest` template + the
  build-time-pinned-verifier pattern. Its §3 **reuses** the shared attest key; this
  ADR deliberately does the **inverse** (§ 3) because the release role is a
  build/publisher key, not an auth-service runtime key.
- ADR-0026 — anti-censorship wiring; the `BootstrapPin` default-empty build-time pin
  pattern and the tunnel-aware native HTTP transport (iOS `AntiCensorshipTransport`,
  Android `AntiCensorshipEngine`) the release fetch rides when armed.
- ADR-0015 — device-removal authorization; the original `lakko-attest` template and
  the `DeviceAttestKey` pin the release pin mirrors.
- `crates/lakko-tuf` — the full binary-artifact auto-update verifier this notice does
  **not** replace and does **not** use (its install half is inapplicable to store
  apps; it stays the right tool for sideloaded/desktop binary updates).
- `apps/web/src/sw/SwUpdateBanner.tsx` — the web's existing service-worker update
  path, retained unchanged.
- RFC 8032 — Ed25519 signatures.
