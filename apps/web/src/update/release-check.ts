// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Shared "update available" check core (ADR-0038 S5 — web).
 *
 * Fetches the signed release manifest, verifies it against a **build-time
 * pinned** publisher release key in the audited Rust core (`verifyReleaseManifest`),
 * and classifies the client's build-time `CURRENT_RELEASE` against the manifest's
 * `latest` / `min_supported` counters (`releaseUpdateStatus`).
 *
 * ## Web intentionally renders NO banner
 *
 * The web client already self-updates via its service worker and shows the
 * existing {@link ../sw/SwUpdateBanner.SwUpdateBanner} ("A new version of Lakko
 * is ready." -> Reload) on a real SW update. Web has **no store to deep-link
 * to**, so per ADR-0038 § "Web note" and the UX spec § "Web reconciliation" it
 * does NOT render a second, competing manifest-driven banner. This module is the
 * shared verify-core landing (one codepath + testability + a dormant future hook
 * — e.g. a future About screen row), NOT a UI surface. The mobile clients
 * (ADR-0038 S4) are the ones that show a banner + a store link.
 *
 * ## Fail toward "up to date" (the UX honesty rule)
 *
 * Every failure path — an unpinned/invalid release key, a fetch error, a non-2xx
 * response, an over-large body, or ANY verify failure (malformed / wrong-length /
 * wrong-version / bad-signature / stale) — resolves to `{ status: "up-to-date" }`.
 * This is an availability / anti-phishing control, NOT an E2EE control (ADR-0038
 * § "Invariants touched" #4): its "fail-safe" means *fail toward showing no nag*,
 * so a spoofed, stripped, or replayed manifest can never drive a false update
 * signal. It is deliberately distinct from invariant #4's fail-closed-refuse.
 *
 * ## Freshness DISABLED in v1
 *
 * Per ADR-0038's resolved Open question, the client passes `i64::MAX` as the
 * staleness window, which short-circuits the freshness gate — a stale manifest
 * can only *suppress* a notice, never forge one (a LOW-threat residual). Re-enabling
 * a generous window later is a one-line change here.
 */

import { loadWasm } from "../wasm";
import { base64UrlDecodeNoPad } from "../auth/account-id";

/**
 * The resolved update verdict. Fails toward `"up-to-date"` on any error (see the
 * module doc). `latestRelease` is populated only on a successful verify, purely
 * for logging / a future notice — never load-bearing.
 */
export interface UpdateCheckResult {
  readonly status: "up-to-date" | "update-available" | "update-required";
  readonly latestRelease?: number;
}

/**
 * Default same-origin path for the signed release manifest. The web SPA is
 * served from `lakko.me`, where S3 (`tools/release-manifest` +
 * `infra/nginx/sites-available/lakko.conf`) serves the raw 89-byte Ed25519 blob
 * as a static file with `Cache-Control: no-store`. This constant MUST match that
 * pinned serve path (`https://lakko.me/.well-known/lakko-release`).
 */
const DEFAULT_MANIFEST_URL = "/.well-known/lakko-release";

/** The build's release counter when `VITE_LAKKO_CURRENT_RELEASE` is unset. */
const DEFAULT_CURRENT_RELEASE = 1;

/**
 * Freshness gate DISABLED (ADR-0038 v1) — `i64::MAX` short-circuits the staleness
 * check in `verifyReleaseManifest`, so a manifest is never rejected as stale.
 */
const STALENESS_DISABLED_MILLIS = 9_223_372_036_854_775_807n; // i64::MAX

/**
 * A defensive upper bound on the fetched manifest size. The manifest is a fixed
 * 89 bytes; a body far larger than that is a hostile / misconfigured origin, and
 * we refuse it (fail toward up-to-date) rather than hand megabytes to the parser.
 * `verifyReleaseManifest` would reject any non-89-byte input anyway; this just
 * caps the work before that.
 */
const MAX_MANIFEST_BYTES = 4096;

/** The always-safe verdict every failure path returns. */
const UP_TO_DATE: UpdateCheckResult = { status: "up-to-date" };

function env(): Record<string, string | undefined> {
  return import.meta.env as Record<string, string | undefined>;
}

/**
 * The build-time pinned 32-byte Ed25519 publisher release verifying key
 * (`VITE_LAKKO_RELEASE_KEY`, base64url no-pad), or `null` when unset / blank /
 * malformed / not 32 bytes.
 *
 * Mirrors {@link ../auth/device-attest-key.pinnedDeviceAttestKey} — but this key
 * is **pin-only** (ADR-0038 § 2): unlike the device-attest key there is NO
 * serving-tier / JWKS fallback, so an unpinned build simply has the check
 * dormant. There is no insecure default.
 */
export function pinnedReleaseKey(): Uint8Array | null {
  const raw = env()["VITE_LAKKO_RELEASE_KEY"];
  if (typeof raw !== "string" || raw.trim().length === 0) return null;
  try {
    const key = base64UrlDecodeNoPad(raw.trim());
    return key.length === 32 ? key : null;
  } catch {
    return null;
  }
}

/** The same-origin manifest URL (`VITE_LAKKO_RELEASE_MANIFEST_URL`, or the pinned default). */
export function manifestUrl(): string {
  const raw = env()["VITE_LAKKO_RELEASE_MANIFEST_URL"];
  if (typeof raw === "string" && raw.trim().length > 0) return raw.trim();
  return DEFAULT_MANIFEST_URL;
}

/**
 * The build's monotone `CURRENT_RELEASE` integer (`VITE_LAKKO_CURRENT_RELEASE`,
 * default `1`). A non-parseable / non-finite / negative value falls back to the
 * default (fail toward the safest — a lower current release only ever risks a
 * benign "update available", never a false "up to date" that hides a required
 * update).
 */
export function currentRelease(): number {
  const raw = env()["VITE_LAKKO_CURRENT_RELEASE"];
  if (typeof raw !== "string" || raw.trim().length === 0) {
    return DEFAULT_CURRENT_RELEASE;
  }
  const n = Number.parseInt(raw.trim(), 10);
  return Number.isSafeInteger(n) && n >= 0 ? n : DEFAULT_CURRENT_RELEASE;
}

/**
 * Map the wasm `UpdateStatus` number-enum (`UpToDate=0`, `UpdateAvailable=1`,
 * `UpdateRequired=2`) to the string status. A pure total function — an
 * unrecognised code is treated as up-to-date (fail toward no-nag).
 */
export function mapUpdateStatus(code: number): UpdateCheckResult["status"] {
  switch (code) {
    case 1:
      return "update-available";
    case 2:
      return "update-required";
    default:
      return "up-to-date";
  }
}

/**
 * Fetch + verify the signed release manifest and classify this build against it.
 *
 * Control flow (every failure resolves to `{ status: "up-to-date" }`):
 *
 *   1. No / invalid pinned release key -> dormant, up-to-date (fail-safe).
 *   2. `fetch(manifestUrl())` -> non-2xx / over-large / network error -> up-to-date.
 *   3. `verifyReleaseManifest(bytes, key, now, i64::MAX)` -> ANY throw -> up-to-date.
 *   4. `releaseUpdateStatus(current, latest, minSupported)` -> the string verdict.
 *
 * Never throws — the caller can `void checkForUpdate()` at boot.
 */
export async function checkForUpdate(): Promise<UpdateCheckResult> {
  // (1) Dormant when the release key is not pinned (ADR-0038 pin-only fail-safe).
  const pinned = pinnedReleaseKey();
  if (pinned === null) return UP_TO_DATE;

  // (2) Fetch the raw manifest bytes. Any network error / non-2xx / over-large
  //     body fails toward up-to-date (a suppressor gets "no nag" either way).
  let bytes: Uint8Array;
  try {
    const response = await fetch(manifestUrl());
    if (!response.ok) return UP_TO_DATE;
    const buf = await response.arrayBuffer();
    if (buf.byteLength === 0 || buf.byteLength > MAX_MANIFEST_BYTES) {
      return UP_TO_DATE;
    }
    bytes = new Uint8Array(buf);
  } catch {
    return UP_TO_DATE;
  }

  // (3) Verify signature + freshness in the audited core. Freshness DISABLED
  //     (i64::MAX). ANY failure (malformed / wrong-key / wrong-version / stale)
  //     throws a `release:`-coded JsError -> treat the app as up-to-date.
  try {
    const wasm = await loadWasm();
    const verified = wasm.verifyReleaseManifest(
      bytes,
      pinned,
      BigInt(Date.now()),
      STALENESS_DISABLED_MILLIS,
    );
    // (4) Pure classification against the build's current release.
    const code = wasm.releaseUpdateStatus(
      BigInt(currentRelease()),
      verified.latestRelease,
      verified.minSupportedRelease,
    );
    return {
      status: mapUpdateStatus(code as number),
      latestRelease: Number(verified.latestRelease),
    };
  } catch {
    return UP_TO_DATE;
  }
}

/**
 * Boot-time fire-and-forget hook: run {@link checkForUpdate} and, when not up to
 * date, emit a single informational `console.info` line.
 *
 * The web UI intentionally defers to {@link ../sw/SwUpdateBanner.SwUpdateBanner}
 * (the service worker is the web's update path) and renders NO manifest-driven
 * banner (ADR-0038 § "Web note"). This is the shared-core landing + a dormant
 * future hook (e.g. a future About screen row). Never throws / rejects — it
 * swallows its own errors so a boot path can `void runBootUpdateCheck()`.
 */
export async function runBootUpdateCheck(): Promise<void> {
  try {
    const result = await checkForUpdate();
    if (result.status !== "up-to-date") {
      // Dormant signal only — no banner on web (SwUpdateBanner owns the UI).
      // ASCII-only, no emoji (repo log-style rule).
      console.info(
        `[update] a newer version is available (release ${result.latestRelease ?? "?"}, ${result.status})`,
      );
    }
  } catch {
    // Fail silent — an update check must never affect the app boot.
  }
}
