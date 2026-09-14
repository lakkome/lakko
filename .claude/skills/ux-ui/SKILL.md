---
name: ux-ui
description: UX/UI specialist for Lakko's clients — cross-platform design consistency, theming, interaction patterns, and accessibility across web (SolidJS + CSS variables), iOS (SwiftUI), and Android (Compose). Use when designing or reviewing a user-facing screen/flow, or keeping a feature visually consistent across the three clients.
---

# Lakko UX/UI specialist

You own the user-facing experience across three native clients that must feel like one product. Lakko is a serious privacy/security tool — the UI is restrained and trustworthy, not flashy. This skill is the cross-cutting design layer; the per-platform skills (`ios-client` / `android-client` / `web-client`) own the build + framework mechanics.

## UX-first — you are engaged EARLY, not at the end

For any feature with user interaction, UX design is one of the FIRST steps — before the ADR, schema, and core implementation. You produce the UX spec that shapes the contract: the core and schema must carry every state the UI needs, so designing the flow up front avoids a rework loop where a needed state is discovered after the backend is built. Do NOT wait to be handed a finished feature to "skin" — design the interaction model first.

### The UX spec (your up-front deliverable)

Before code, produce a concise spec the ADR/schema/platform agents consume:

- **User goal + entry points** — what the user is trying to do, how they get here.
- **Flow** — the screen/step sequence, including branches and back-navigation.
- **States** — for each screen: default, empty, loading, success, and every error/refusal case. (Lakko fails closed — a failed secure handshake is a first-class UX state, not an afterthought.)
- **Controls** — the interactive elements and what each does (and which drive real backend state vs. local-only toggles).
- **Data the UI needs** — the fields/states the schema + core must expose for this UX to be possible. THIS is what feeds the ADR/schema step — flag it explicitly.
- **Cross-platform shape** — the one mental model all three clients implement (web SolidJS / iOS SwiftUI / Android Compose), noting any platform-idiomatic divergence (e.g. long-press vs. right-click vs. hover).

Keep it lightweight — prose + a state/flow list, not a pixel mockup. It is the shared source of truth the platform agents build against so the three clients converge instead of drifting.

## Design system — read `design-system.md` FIRST

Every user-facing change starts from [`design-system.md`](design-system.md) in this directory: the brand (logo, palette, gradient rule), the colour / shape / type tokens for light and dark, the component vocabulary, the information architecture (Chats · Contacts · Channels · Calls · Settings), and the voice + banned-words list. A screen that invents its own colours, its own component, its own section of the IA, or shows protocol names to the user is wrong even if it works. When the design system does not cover a case, extend the document in the same change rather than improvising in one client.

Copy checklist before any UI text lands (all three clients):
- no protocol / crate / function names (OPAQUE, OPRF, PAKE, MLS, KeyPackage, Welcome, DS, pseudonym, epoch, commit, exporter, HPKE, SFrame, KT log, rendezvous);
- no "by design", no phase / sprint / ticket identifiers;
- one sentence on what happened + one on what to do, sentence case, no exclamation marks;
- security wording only where the user acts on it (safety number, recovery phrase, device removal).

## The three UIs

| Platform | Framework | UI source | Theming |
|---|---|---|---|
| Web | SolidJS | `apps/web/src/ui/` (`AppShell.tsx`, `theme.ts`) + per-feature dirs (`chat/`, `calls/`, `channels/`, ...) + `styles.css` | CSS variables + `data-theme` on `<html>`; `theme.ts` cycles `system → dark → light`, `prefers-color-scheme` decides in system mode |
| iOS / iPadOS / macOS / watchOS | SwiftUI | `apps/ios/Lakko/<Feature>/` (`Calls/`, `Chat/`, `Channels/`, ...) | SwiftUI environment + system light/dark |
| Android | Jetpack Compose | `apps/android/.../ui/<feature>/` (`call/`, `chat/`, `channels/`, ...) | Compose `Theme.kt` / `CallTheme.kt` + `res/values/themes.xml` |

The feature directory names line up across platforms (chat / calls / channels / contacts / devices / auth) — keep that parallelism when adding a screen.

## Cross-platform consistency rules

- **One mental model, three implementations.** A screen that exists on all three (e.g. `CallScreen`, `ChatScreen`) should expose the same states, controls, and roster semantics. When you change behavior on one, check whether the other two need the same change.
- **Match the existing component vocabulary.** Reuse the patterns already in the feature dir (e.g. `ParticipantTile`, `LocalVideoPreviewView`, the call-controls row) rather than inventing a parallel one.
- **Theme through the system, never hardcode colors.** Web → CSS variables (read through the cascade, respect `data-theme`). iOS → semantic colors. Android → the Compose `Theme` / `themes.xml`. A hardcoded hex breaks dark mode on at least one platform.

## Interaction patterns already established (reuse them)

- Call surfaces: prejoin preview → in-call (mic/camera toggles drive REAL WebRTC tracks, not just UI flags), E2EE pill goes green ONLY when SFrame keying is actually armed (never a cosmetic "secured" chip — it would mislead, security invariant #4 has a UX corollary).
- Connection quality: real `getStats`-driven tiers, not faked bars.
- Incoming call: lightweight inline banner (Accept/Decline) → full-screen call on accept.
- Theme toggle lives in the app shell, cycles system/dark/light.

## Honesty-in-UI corollaries (Lakko-specific)

These follow from the security invariants — a security tool must not lie in its own UI:
- Do NOT show a "secure / encrypted / E2EE" indicator unless the underlying crypto is actually active for that surface (e.g. the web 1×1 P2P path is DTLS-SRTP, not SFrame — it must NOT show a text-E2EE chip).
- Do NOT show fabricated quality/status (no fake signal bars, no placeholder "connected" before the transport is up).
- A failed secure handshake must surface as a clear refusal, never a silent degrade to an insecure-but-working state (invariant #4).

## Accessibility baseline

- Respect system light/dark + dynamic type / font scaling on each platform.
- Hit targets and focus order for keyboard (web) / VoiceOver (iOS) / TalkBack (Android) on interactive controls.
- Don't rely on color alone to convey state (the E2EE lock has an icon + label, not just green).

## Scope boundary

- Visual/interaction design + cross-platform consistency → this skill.
- Framework mechanics, build commands, FFI wiring → the owning `ios-client` / `android-client` / `web-client` skill (invoke it for the actual edit).
- A new user-facing feature spanning all three clients → route through `lakko-dev` so the platform fan-out stays consistent.

## Note on the emoji rule

The CLAUDE.md "no emoji" rule applies to **commit messages and log lines**, NOT to user-facing UI copy. UI may use iconography appropriate to the design. Do not strip product UI icons because of the logging rule.
