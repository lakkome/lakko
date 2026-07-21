<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# UX spec — "Update available" notice

**Status:** design only (no app code yet).
**Owner surface:** cross-platform (web SolidJS / iOS SwiftUI / Android Compose).
**Pattern parent:** `IdentitySecurityBanner` (placement + mount) and `SwUpdateBanner` (tone + a11y).

## 1. User goal + entry points

The user is running a client that periodically learns from a signed manifest whether a
newer app version was published. The goal is a **quiet, honest, dismissible** heads-up so
they can update at their convenience — never a blocker in v1, never a surprise modal.

Entry point: passive. The notice appears on its own when the resolved update state is
`UpdateAvailable` (or `UpdateRequired`, treated identically in v1). There is no user action
that "opens" it. On mobile the only user action inside it is **Update**, which deep-links
to the App Store / Play Store.

## 2. States (the mechanism, and what the UI renders)

The manifest resolves one of three states. **The UI consumes the state only after the
manifest signature is verified** (see §8). An unverified or unfetched manifest resolves to
`UpToDate` — fail toward *not* nagging and *not* blocking, so a spoofed manifest cannot
brick the app.

| State | v1 UI |
|---|---|
| `UpToDate` | Nothing rendered. No positive-confirmation banner (see §5). |
| `UpdateAvailable` | The dismissible top banner (§3). |
| `UpdateRequired` | **v1: identical to `UpdateAvailable`** — same dismissible banner, same copy. The blocking screen is a future design, sketched in §7, NOT built. |

## 3. Placement + form

A **top-of-content, dismissible, non-blocking banner** that clones
`IdentitySecurityBanner`'s placement and structure — a horizontal row: leading icon +
message (fills width) + trailing controls (`Update`, then a dismiss affordance).

- **Native mount points (clone the security banner exactly):**
  - **iOS** — `LakkoApp.swift`, inside the `.session` `VStack(spacing: 0)`, rendered
    **below** `IdentitySecurityBanner` and above `SessionRoot`.
  - **Android** — `MainActivity.kt`, inside the `Session` `Column`, **below**
    `IdentitySecurityBanner` and above `SessionRoot`.
  - Ordering rule when both banners are present: **security alert first (higher
    severity), update notice second.** The update banner never displaces the security one.
- **It does NOT block the app.** Dismissible; the app is fully usable behind it.
- **Visual tone: informational, NOT danger.** This is the one deliberate divergence from
  `IdentitySecurityBanner`: that banner is red because it signals possible compromise. The
  update notice must read as *neutral/helpful* so it does not trip the same alarm response
  (and so stacking the two is legible). It uses the neutral-surface + accent-button
  treatment already established by `SwUpdateBanner`, not the error palette. See §9 for the
  per-platform token map.

### Web reconciliation — no second banner

Web self-updates via the service worker; there is no store to deep-link to. The existing
**`SwUpdateBanner`** (bottom-center toast, "A new version of Lakko is ready." → **Reload**)
already *is* the web "update available" surface, wired at the `App.tsx` top level.

**Decision: web keeps `SwUpdateBanner` unchanged. Do NOT add a competing top banner on
web.** The manifest-driven top banner is a **mobile-only** surface. The shared mental model
is "a dismissible new-version notice with one primary action"; web fulfils it with Reload,
mobile with Update-in-store. Keeping them as two surfaces avoids a double notice on web and
respects each platform's real update mechanism (honesty corollary — the button must do what
it says: web reloads, mobile opens the store).

## 4. Copy (exact strings — English only)

- **Primary line (required):** `A new version of Lakko is available.`
- **Optional version detail (render only if the manifest exposes a human version
  string):** append a second, muted line — `Lakko {version} is ready to install.`
  (e.g. "Lakko 1.4 is ready to install."). Omit entirely if no version string is present;
  never invent one.
- **Primary button:** `Update`
- **Dismiss affordance:** matches the parent banner. iOS/Android use a text `Dismiss`
  button (as `IdentitySecurityBanner` does); web (if ever surfaced) would too. A trailing
  `×` icon-button is an acceptable platform-idiomatic substitute **only** with an
  accessible label "Dismiss".

There is no distinct `UpdateRequired` copy in v1 (it reuses the above). The future blocking
copy lives in §7.

## 5. The store button + empty-store-URL behavior

The store URL is a **build-time config that is DEFAULT-EMPTY** (the app is `me.lakko.dev`,
unpublished). Empty is the common case until launch.

- iOS reads it from an `Info.plist` key (e.g. `LakkoAppStoreUrl`), default empty — same
  pattern as `LakkoSfuUrl` / `LakkoBootstrap*`.
- Android reads a `BuildConfig` field (e.g. `LAKKO_APP_STORE_URL`), default empty — same
  pattern as `LAKKO_SFU_URL` / `LAKKO_BOOTSTRAP_*`.

**Recommendation: option (a) — hide the `Update` button; render the banner text-only when
the store URL is empty.** The banner stays coherent as a purely informational notice
("A new version of Lakko is available.") with no action control.

- **Reject option (b) — a lakko.me fallback link.** For an unpublished app, lakko.me offers
  no installable download, so the link is a dead end. A control that promises an action it
  can't deliver is exactly the kind of dishonest UI a security product must avoid (the
  honesty-in-UI corollary). A dead link erodes trust more than a missing button.
- **Do not disable-and-show a greyed-out `Update` button** either — a visible-but-dead
  control is the same broken promise. Hide it outright.
- **Optional hardening (recommended, not blocking):** because a text-only "update
  available" with no way to act is itself a little awkward, gate the whole mobile banner
  behind "store URL present OR an internal-build override flag." In a shipped store build
  the URL will be set, so the button always shows; in unpublished/dogfood builds the notice
  stays dormant unless a tester opts in. This mirrors Lakko's established "default-empty
  config ⇒ dormant feature, no awkward/insecure default" convention (SFU, anti-censorship).
  If the team prefers the notice visible during pre-launch testing, the text-only banner
  from option (a) is the fallback — paired with the §6 cadence so it appears at most once
  per new version.

## 6. Dismiss + re-nag cadence

The banner is dismissible and must not nag. Recommended rule:

- **Within a session:** dismiss removes the banner for the rest of the current app run. It
  does not reappear until a later cold launch.
- **On a later cold launch:** the banner reappears only if **either**
  1. at least **24 hours** have passed since the dismiss, **or**
  2. the manifest now offers a **strictly newer** version than the one that was dismissed.
- This caps nagging to roughly **once per day** while the user stays out of date, and
  **immediately re-surfaces a still-newer release**.

**Newer-release rule (answers the sub-question — yes):** the dismiss stores the
**dismissed target version**. The banner shows whenever
`latest_offered_version > dismissed_version` (and not already dismissed this session),
**bypassing the 24h window**. So dismissing release N does not suppress release N+1 — N+1 is
a new target and re-nags at once.

- **Persistence:** the dismissed version + timestamp go in the lightweight per-platform
  prefs store the app already uses for non-secret UI state — web `localStorage`, iOS
  `UserDefaults`, Android `SharedPreferences` (the theme-preference / anti-censorship-toggle
  tier). This is a version integer + a timestamp: **no PII, no key material**, so no
  encrypted store is required (invariant #1 is not engaged).
- **Empty-store-URL cadence:** when the notice is text-only (§5), show it **at most once per
  new version** — there is no update action to "resolve" it, so the 24h re-nag is dropped in
  favour of once-per-new-version to avoid nagging a tester who cannot act.

## 7. `UpToDate` — no confirmation, and the (future) version row

- **v1: nothing is shown when up to date.** No green "you're current" banner — a persistent
  positive confirmation is clutter and risks going stale between manifest fetches.
- **No Settings/About screen exists on any client today** (recon confirmed). Therefore
  **v1 ships banner-only — it does NOT add a new Settings/About screen.**
- **Future add:** when a Settings/About screen lands, add a passive row —
  `Lakko {version} — up to date` when current, or `Lakko {version} — update available` with
  an inline `Update` affordance when not. That is the natural home for a positive
  confirmation (a user who navigates to About is asking); it does not belong in a banner.
  Flagged as future, not built here.

## 8. `UpdateRequired` — future blocking screen (sketch, NOT built)

v1 renders `UpdateRequired` as the same dismissible banner. When a future release needs a
hard floor, the design is a **full-screen, non-dismissible gate** shown over the whole app:

- **Form:** full-screen cover (iOS `fullScreenCover`, Android full-screen `Dialog`/route,
  web full-viewport route) — no dismiss, no "Later", no way past it except updating.
- **Copy:**
  - Title: `Update required`
  - Body: `This version of Lakko is no longer supported. Update to continue.`
  - Primary button: `Update` (opens the store, same deep-link as the banner).
- **Empty store URL (post-launch this should not happen):** show the message and a muted
  `Get the latest version from lakko.me` line **without** a dead button — same honesty rule
  as §5.
- **Two open questions to resolve when it is actually built (not now):**
  1. **Offline / unverified-manifest behavior** — a hard block must be driven only by a
     *signature-verified* `UpdateRequired`. If the client cannot fetch or verify the
     manifest, it must **not** hard-block on a stale cached "required" verdict without care,
     or a network outage bricks the app. Lean toward letting the user in unless a verified
     required-floor is known.
  2. **Grace treatment** — whether a first `UpdateRequired` sighting gets a short "update
     within N days" soft phase before the hard gate. Deferred.
- Same theming + accessibility rules as the banner (§9, §10) apply to the blocking screen.

## 9. Cross-platform token map (clone the banner, informational tone)

The one mental model — "dismissible new-version notice, one primary action, informational
(not danger) tone" — maps to each platform through its theme system. **Never hardcode a
hex.**

| | Surface / background | Text | Primary `Update` button | Corner / spacing |
|---|---|---|---|---|
| **Web** (if surfaced) | `--surface-elevated` on `--surface-border`; already covered by `SwUpdateBanner` (bottom toast, `role="status"`) | `--surface-fg`, muted detail `--surface-muted` | existing `.primary` class → `--accent` / `--accent-fg` | `--radius-md`, `--shadow-1` |
| **iOS** | subtle accent tint — `Color.accentColor.opacity(0.12)` with a thin accent stroke (reads informational, not the security banner's `Color.red.opacity(0.15)`) | `.subheadline`/`.semibold` title, `.footnote` detail (semantic colors) | `.borderedProminent` `Button("Update")` (accent) | `cornerRadius: 8`, `.padding(12)` inner / `.padding(8)` outer — exactly the security banner's metrics |
| **Android** | `MaterialTheme.colorScheme.secondaryContainer` / `onSecondaryContainer` (informational — NOT `errorContainer`, which is reserved for the security banner) | `titleSmall` + `bodySmall` | `FilledTonalButton`/`Button("Update")` (theme accent) | `RoundedCornerShape(8.dp)`, `.padding(12.dp)` inner / `.padding(8.dp)` outer |

Suggested file placement (parallel feature dirs, matching the codebase convention):
`apps/ios/Lakko/Updates/UpdateAvailableBanner.swift`,
`apps/android/.../ui/updates/UpdateAvailableBanner.kt`. Web needs no new file (it reuses
`SwUpdateBanner`).

**Data the UI needs (this feeds the ADR / schema / core step):**

- `updateState: UpToDate | UpdateAvailable | UpdateRequired` — **post-signature-verify**.
- `latestVersion` — the offered version string, used to key the dismiss marker and, if
  human-readable, the optional detail line.
- `storeUrl` — build-time config (`Info.plist` / `BuildConfig`), may be empty.
- Client-local (per-platform prefs): `dismissedVersion`, `dismissedAt`.

## 10. Accessibility

- **Don't rely on color alone.** Pair the text with a leading glyph (e.g. a download /
  up-arrow-in-circle icon) so the meaning survives for color-blind users and greyscale — the
  same principle as the E2EE lock carrying an icon + label, not just green.
- **Contrast:** all colors come from theme tokens, which already target WCAG-AA in
  `styles.css` and the native theme systems; body text must hit ≥4.5:1 on the banner
  surface. The informational secondary-container / accent-tint choices in §9 must be checked
  against AA in both light and dark.
- **Tap targets:** `Update` and the dismiss control ≥ **44×44 pt** (iOS) / **48×48 dp**
  (Android) / an adequately padded hit area (web).
- **Screen reader:**
  - Web (SwUpdateBanner, existing): `role="status"` + `aria-live="polite"` — informational,
    non-interrupting. Note this is deliberately **weaker** than the security banner's
    `role="alert"` (assertive); an available update is not an interrupt-worthy alert.
  - iOS: expose the banner as an accessibility element reading the primary line; the
    `Update` button carries its own label plus a hint ("Opens the App Store").
  - Android: the text is exposed to TalkBack; the `Update` button gets a semantics /
    content description ("Update, opens the Play Store").
- **Reduced motion:** **recommend no entrance animation** — an instant conditional render,
  exactly like `IdentitySecurityBanner` today. If any fade/slide is ever added, it must
  respect `prefers-reduced-motion` (web) / `UIAccessibility.isReduceMotionEnabled` (iOS) /
  the animator-duration-scale / reduce-motion setting (Android) and fall back to instant.
- **Dynamic type / font scaling:** use the semantic text styles above so the banner scales;
  it must be allowed to grow vertically and wrap (the existing banners already do) — never
  clip at large text sizes.

## 11. UX risks (flagged)

- **Banner fatigue / stacking.** With the security banner also possible, two stacked
  top-of-content banners can overwhelm. Mitigated by: severity ordering (security first),
  the update banner's softer informational tone (visually distinct from the red alert), and
  the once-per-day cadence. If a third top banner is ever proposed, revisit — three is too
  many.
- **Empty-store-button awkwardness.** A "new version available" notice with no way to act
  (pre-launch, empty store URL) is inherently a little odd. Mitigated by the §5
  recommendation (hide the button / optionally suppress the whole banner on unpublished
  builds) and the §6 once-per-new-version cadence. This is the main pre-launch rough edge —
  flagged so the team consciously picks "text-only" vs "dormant."
- **Nagging with no resolution path.** If the text-only banner shows but the user cannot
  update (no store), repeated appearances have no payoff. The once-per-new-version cap
  addresses it; still worth watching in dogfood feedback.
- **Honesty pitfalls (must not regress):** (1) never let an *unverified* manifest drive a
  block or even a nag — verify first, fail toward `UpToDate`; (2) never show a stale "up to
  date" confirmation (hence none in v1); (3) the `Update` button must actually reach a
  store — hide it when it can't.
