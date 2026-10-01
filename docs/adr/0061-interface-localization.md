<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0061: Interface localization — one catalog, built-in languages, signed updates that do not reveal the language

- **Status:** Accepted (maintainer, 2026-09-28)
- **Date:** 2026-09-28
- **Number:** written as ADR-0058 on its branch; renumbered to 0061 when main was merged in (2026-10-01), since main had given 0058 to chat folders and another branch holds 0059.
- **Context:** GA readiness — every client, user-facing. UX spec:
  [`docs/ux/localization.md`](../ux/localization.md). Maintainer decisions
  of 2026-09-28: first language set `en ar es fa he hi pt-BR ru tr uk zh-Hans`;
  system language by default, English fallback; languages built into the
  app **plus** signed updates between releases; machine translation with
  human review of security-critical strings.

## Context

No client is localized. Measured on 2026-09-28 in this tree:

- **Web** (`apps/web`, 101 `.tsx` files): no i18n dependency in
  `package.json`; every label is an English literal in JSX. Some modules
  already format dates and times through `Intl` (`chat/list-time.ts`,
  `channels/relative-time.ts`, `calls/call-history.ts`, ...), with the
  browser's default locale.
- **iOS** (`apps/ios`, xcodegen `project.yml`, 303 Swift files): no
  `.xcstrings`, no `.lproj`, no `String(localized:)`.
- **Android** (`apps/android`, 301 Kotlin files): `res/values/strings.xml`
  holds `app_name` only; no `stringResource` call in `src/main`.
- Some web screens surface an error's `.message` directly
  (`routes/Channels.tsx`, `routes/Calls.tsx`), so core or transport prose
  can reach the user today.

Forces:

1. **Three clients, one product.** The same string drifting three ways is
   the failure mode `lakko-ui-rules` exists to prevent. Three hand-kept
   string tables will drift in wording, placeholders and plural handling.
2. **Live switching.** The maintainer wants the language to change the
   moment it is picked. Apple's per-app language restarts the app; Android's
   recreates the activity (acceptable); nothing on the web is built.
3. **Updates between releases.** Translations will be machine-drafted and
   corrected over time; a fix should not wait for a store review.
4. **Server-delivered UI text is an attack surface.** A server that can
   change what the interface says can rewrite the safety-number or
   recovery-phrase instructions — social engineering against the exact
   screens that protect the user. The operator is untrusted (zero-trust).
5. **The language is personal data.** It correlates with country and, for
   Persian, Arabic, Russian, Chinese and Turkish users, with the censored
   audience ADR-0004 / ADR-0026 serve. Invariants #1 and #5 say the server
   must not learn it. A per-language download reveals it.

## Decision

**One source catalog in the repository, compiled into every client; a Rust
crate `lakko-l10n` owns the formatter, the resolution rule and the signed
update; a native client may receive one signed bundle carrying every
language at once, which can never change a security-critical string.**

### 1. Catalog

- `l10n/catalog/en.json` is the source: per key, the English text, a
  translator comment, a `security_critical` flag, and the placeholder
  names it uses. `l10n/catalog/<lang>.json` holds a translation per key
  plus a `status`: `machine` or `reviewed`.
- The language table (code, own name, English name, direction, script,
  CLDR plural locale) lives in `crates/lakko-l10n/src/language.rs`, the one
  list every client reads over the FFI. Platform tags map to it by a rule
  (`match_tag`), not by a per-language list of aliases: language and
  script, where a script named or implied by the region (`zh-TW` implies
  `Hant`) that differs from Lakko's never matches.
- Message syntax is a **subset of ICU MessageFormat**: `{name}` arguments
  and `{count, plural, zero|one|two|few|many|other {...}}` with `#`.
  No `select`, no nested plurals, no formatting styles inside messages —
  dates, numbers and sizes are formatted by the platform for the locale
  and passed in as string arguments.
- **Security-critical keys ship only when reviewed.** A build takes the
  translation of a `security_critical` key only if its status is
  `reviewed`; otherwise that key renders in English. Machine-drafted text
  never appears on the safety-number, recovery-phrase, device-removal,
  account-deletion, identity-change or E2EE-indicator surfaces.

### 2. Build-time tool

`tools/l10n` (a workspace bin) validates and compiles the catalog:

- every translation uses exactly the placeholder set of its source and
  only the plural categories the language has (CLDR), else the build
  fails;
- no key in a translation is absent from the source;
- every source key carries a translator comment;
- `compile --out <dir>` writes `source.json`, one compiled `<lang>.json`
  per translation (security-critical keys without `reviewed` status left
  out, so they render in English), and `bundle-payload.json` (every
  language, security-critical keys left out) for `sign`;
- `keygen` / `sign` for the translation-publisher key and the bundle; `sign`
  runs the client acceptance path on its own output before writing it.

The few OS-owned files (iOS `InfoPlist.xcstrings` for permission prompts
and the display name, Android `res/xml/locales_config.xml` and
`values-<lang>/strings.xml` for `app_name`) are generated by the tool too.
Built so far: `lakko-l10n android-locales --out <path>` writes
`locales_config.xml` from the supported-language table, and
`just l10n-compile` / `just l10n-check` write and check it beside the
catalogs. `InfoPlist.xcstrings` and `values-<lang>/strings.xml` are not
built yet.

`just l10n-compile` writes the compiled `source.json` and `<lang>.json`
files into each client's resources (`apps/web/src/l10n/catalog`,
`apps/ios/Lakko/Resources/L10n`, `apps/android/app/src/main/assets/l10n`).
The outputs are committed, so Vite, Xcode and Gradle need no Rust step;
`just l10n-check` validates the catalog and fails when a committed output
differs from a fresh compile.

### 3. `lakko-l10n` crate (Rust, FFI-exposed)

Same rationale as `lakko-ui-rules`: a rule three clients must agree on is
written once.

- `resolve_language(selected: Option<&str>, system_preferred: &[String],
  supported) -> LanguageCode` — selected, else the first system preference
  matching by language + script, then language alone, else `en`.
- `Catalog::load(built_in_bytes, update_bytes: Option<&[u8]>, lang)` —
  lookup order: verified update for `lang` -> built-in `lang` -> built-in
  `en`. A key is never missing at runtime.
- `format(key, args)` — the subset grammar above; plural category from
  CLDR data via `icu_plurals` 2 (ICU4X, Unicode-3.0 licence, allowed by
  `deny.toml`). Adding it moved the ICU4X crates already in the tree
  through `idna` from 2.2 to 2.3. Text arguments are wrapped in Unicode
  isolates (FSI ... PDI), so a name in another direction cannot reorder
  the sentence; a count's display form (native digits, grouping) is
  formatted by the platform and passed alongside the value.
- Direction per language, for the client to set `dir` /
  `layoutDirection`.

The client keeps the loaded catalog in memory and asks for plain strings
once per language switch; per-render FFI calls are only for messages with
arguments. The cost of that on each platform is measured when each client
is wired (S2-S4), not assumed.

### 4. Live switching — per client, no restart

- **Web:** the dictionary is a Solid signal; switching loads the language
  chunk (lazy `import()` from the app's own origin), then swaps the signal
  and sets `lang` / `dir` on `<html>`. Only the active language is held in
  memory.
- **iOS:** a root `L10n` observable; the root view reads it and sets
  `.environment(\.locale)` and `.environment(\.layoutDirection)`. System
  `Bundle` localization is not used for UI text, which is why in-app
  switching needs no restart. The choice is stored in the App Group so the
  notification extension renders the same language.
- **Android:** `AppCompatDelegate.setApplicationLocales` (the system
  per-app language, also visible in system settings on Android 13+); the
  activity is recreated and Compose re-renders with the `lakko-l10n`
  catalog for the new locale.

The choice is stored only on the device and is never sent to a server.

### 5. Signed translation updates (native clients only)

- **Artifact:** `TranslationBundle` = header `format_version(1) ‖
  bundle_version(8 BE u64) ‖ min_release(8 BE u64) ‖
  signed_at_unix_millis(8 BE i64) ‖ payload_len(4 BE) ‖ payload ‖ sig(64)`,
  Ed25519 over the domain `lakko/translation-bundle/v1` plus everything
  before the signature. The payload is the compiled catalog for **every**
  supported language.
- **Signer:** a dedicated offline translation-publisher key, not the
  ADR-0038 release key and not the auth attest key (translation publishing
  may be delegated to a smaller circle than release signing). The
  verifying key is pinned at build time; default-empty = no fetch.
- **Serving:** a static file at `/.well-known/lakko-translations`, the
  ADR-0038 hosting pattern (separate root, `no-store`). Fetched on the
  ADR-0038 cadence, through the ADR-0026 tunnel when armed.
- **Language privacy:** every client downloads the same bytes regardless
  of its language, so the request reveals nothing about it.
- **Client acceptance rules** (all in `lakko-l10n`, fail toward the
  built-in catalog on any failure, silently):
  1. signature valid under the pinned key;
  2. `bundle_version` strictly greater than the built-in catalog's version
     and than the last accepted bundle (no rollback);
  3. `min_release <= CURRENT_RELEASE` (keys match this build);
  4. per string: the key exists in the built-in source catalog, the
     placeholder set matches the built-in source, and the key is **not**
     `security_critical` **in the built-in catalog** (the bundle cannot
     declare the flag away) — otherwise that one string is ignored;
  5. size bounded before parsing.
- **Web gets no bundle:** the web client's code already comes from the
  server, so a separate signed channel would add nothing; a translation
  fix ships with the next web deploy.

### 6. Core prose never reaches the UI

Errors and refusals crossing the FFI are rendered from their code
(`RefusalReason`, `AnonTokenError::code()`, HTTP error codes), never from
`.message`. A sweep of the three clients replaces every direct
`.message` display with a keyed string; an unknown code renders a generic
keyed sentence.

## Alternatives

- **Platform-native tables only (xcstrings / strings.xml / a JS i18n
  library), three generated outputs, no Rust.** Rejected as the base:
  plural handling and formatting would be three implementations; iOS
  in-app switching still needs a custom lookup; and updates between
  releases would have to be merged into three different runtime stores.
  Kept for the OS-owned strings (permission prompts, app name), which the
  OS reads before any Lakko code runs.
- **Downloadable per-language packs (the Telegram model).** Rejected: the
  download reveals the language (invariants #1, #5), is blockable in the
  censored networks Lakko targets, and unsigned it hands the operator the
  interface text.
- **Built-in only, no updates.** Simplest and safest; rejected by the
  maintainer because translation fixes would wait for store releases.
  The signed bundle keeps the safety of this option for every
  security-critical string.
- **Reuse the ADR-0038 release key.** Rejected: signing roles stay
  separate, as ADR-0038 itself separated the release key from the attest
  key.

## Consequences

- **Positive:** one catalog and one formatter; the server can neither
  learn the language nor change a security-critical sentence; languages
  are added without touching screens; translation fixes reach native users
  without a store release.
- **Negative:** every UI string in the three clients moves to a key — the
  bulk of the work, mechanical, and a regression risk in every screen; a
  new Rust dependency for CLDR plural data; every native client downloads
  every language on each bundle update (text only, size to be measured
  once the catalog exists); RTL mirroring touches most layouts.
- **Neutral:** a fourth `lakko-attest`-style signed artifact, verify-only
  on the client, with its own offline tool (`tools/l10n` `sign`).

Invariants: #1 and #5 hold by construction (the choice is local, the
bundle fetch is language-blind). #2, #3, #4 untouched — no key material,
no MLS change, no E2EE path. A `lakko-security` pass is optional (a static
signed artifact and a pinned verifier, as ADR-0038).

## Plan

- **S1** `lakko-l10n` (resolution, catalog load, formatter, bundle
  verify + acceptance rules, property tests, fuzz target for the bundle
  and catalog parsers), `tools/l10n` validate + compile, FFI on UniFFI and
  wasm, the English catalog seeded.
- **S2** web: extraction of every literal, Settings > Language, the auth
  language button, CSS logical properties and `dir`, bidi isolation of
  user text.
- **S3** iOS and **S4** Android: the same, in parallel with S2.
- **S5** machine translation of the first set; security-critical keys
  queued for native-speaker review.
- **S6** signed bundle: `tools/l10n sign`, hosting, native fetch + apply,
  Settings > About line.

S1-S6 built by 2026-10-01 (S6: `just gen-translation-key` / `just
sign-translations`, nginx `/.well-known/lakko-translations`, iOS and Android
fetch on the release-notice cadence with any 2xx starting the 24-hour window,
re-verify at launch, Settings > About line). Not walked live; dormant until a
publisher key is pinned.

## Open questions

1. ~~The CLDR plural crate.~~ Resolved in S1: `icu_plurals`. Still open:
   its size inside the iOS notification extension, which renders
   notification text and has a tight memory budget (ADR-0041), is not
   measured yet; measure it when the extension is wired in S3.
2. Digits in Arabic and Persian: follow the locale formatter (native
   digits) everywhere except the safety number and recovery-phrase
   positions, which are Latin by the UX spec. Confirm with native
   speakers during review.
3. Whether a bundle may add a **new** language that the installed build
   does not ship. Current answer: no — a language needs layout review on
   the build that shows it.

## References

- [`docs/ux/localization.md`](../ux/localization.md)
- [ADR-0038](0038-update-available-notice.md) — signed static artifact,
  pinned key, hosting pattern
- [ADR-0026](0026-anti-censorship-wiring.md) — tunnelled fetches when armed
- [ADR-0041](0041-ios-silent-push-nse-decrypt.md) — notification extension
  budget
- `crates/lakko-ui-rules` — precedent for rules shared in Rust
- Unicode CLDR plural rules; ICU MessageFormat
