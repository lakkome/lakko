// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Lakko web client root component.
 *
 * 1.16a was a single page that ran a "Hello core" probe inline.
 * 1.16b moves to a real shell:
 *
 *   - `<AuthProvider>` exposes the session state to every route.
 *   - `<Router>` from @solidjs/router serves the routes defined in
 *     `routes/routes.tsx`. Public pages (`/login`, `/signup`) and
 *     protected pages (`/`, `/contacts`, `/chat/:id`, `/dev`) are
 *     gated by `<RequireAnonymous>` / `<RequireSession>` guards,
 *     respectively.
 *   - The original "core probe" lives at `/dev` (DevProbe.tsx) and
 *     stays as a smoke surface — real users never land there.
 *
 * ## Provider order
 *
 * `<ChatProvider>` MUST wrap `<AuthProvider>`, not the other way
 * round. `AuthProvider`'s body synchronously calls `useChats()`
 * (it owns the canonical `WasmIdentity` and clears MLS state on
 * logout / forgetDevice — see 1.16e5 / 1.16e6c1), so the chat
 * context has to be in scope when AuthProvider mounts.
 *
 * No business logic in this file on purpose: every screen lives in
 * its own module so swapping a route is a single import change.
 */
import type { Component } from "solid-js";
import { Router, type RouteSectionProps } from "@solidjs/router";
import { AuthProvider } from "./auth/AuthContext";
import { ChatProvider } from "./chat/ChatContext";
import { BackgroundReceiveProvider } from "./chat/BackgroundReceiveContext";
import { CallHostProvider, CallHostView } from "./calls/CallHostContext";
import { ChannelsHydration } from "./channels/ChannelsHydration";
import { DeviceGroupSyncProvider } from "./devices/sync/DeviceGroupSyncContext";
import {
  MailboxJoinFailureBanner,
  MailboxProvider,
} from "./contacts/MailboxContext";
import { PowActivityIndicator } from "./ui/PowActivityIndicator";
import { ProfileProvider } from "./profile/ProfileContext";
import { GroupNameProvider } from "./chat/GroupNameContext";
import { WakeNavigator } from "./push/WakeNavigator";
import { routes } from "./routes/routes";
import { SwUpdateBanner } from "./sw/SwUpdateBanner";
import { ConfirmDialog } from "./ui/ConfirmDialog";

/**
 * Router-level layout (G-36). Renders the active route unchanged and
 * mounts the push notification-tap reader alongside it.
 *
 * The reader has to live INSIDE `<Router>` (it calls `useNavigate`)
 * but OUTSIDE any individual route, so that a tap captured on a cold
 * `/?wake=…` open survives the `/` -> `/login` -> `/` bounce a
 * password re-entry performs. `<Router root>` is exactly that slot.
 *
 * The call UI (`CallHostView`) lives here for the same reason: inside the
 * router (its banners link to routes) and outside every route, including the
 * public `/c/` call-link landing, so no route change hides a ringing call or
 * unmounts a live one of any kind. It renders nothing without a call.
 */
const RootLayout: Component<RouteSectionProps> = (props) => (
  <>
    <WakeNavigator />
    {props.children}
    <CallHostView />
  </>
);

/**
 * Provider order rationale:
 *
 *   <ChatProvider> — AuthProvider's body synchronously calls
 *     `useChats()` (chat state is cleared on logout / forgetDevice).
 *     Has to be in scope before AuthProvider mounts.
 *
 *   <AuthProvider> — exposes session state. Owns the canonical
 *     export-key-derived Identity.
 *
 *   <DeviceGroupSyncProvider> — Phase 2a3e2b. Reads
 *     `useAuth().state()` to know when to bootstrap a per-session
 *     DeviceGroup handle + poll loop. MUST be inside AuthProvider
 *     so the auth-state subscription resolves.
 *
 *   <ProfileProvider> — ADR-0048 profile names: the own profile, the
 *     received-profile store, username verification, publishing into groups
 *     and the one display resolver. Inside Auth + Chat (it reads the session
 *     and publishes through the canonical identity) and ABOVE the mailbox,
 *     whose Welcome joins and contact requests feed it.
 *
 *   <GroupNameProvider> — ADR-0051 shared group names: the per-group store,
 *     the role-gated receive and the growth re-send. Inside Auth + Chat for
 *     the same reasons, and ABOVE the mailbox, whose Welcome joins title
 *     from it.
 *
 *   <MailboxProvider> — the identity-mailbox receive loop (contact
 *     requests, acks, Welcomes) for the whole signed-in session, and
 *     the pending-request list the Contacts route renders. Inside
 *     AuthProvider for the same reason, and above the router so the
 *     loop does not stop when the user leaves the Contacts route.
 *
 *   <CallHostProvider> — the one call slot (a 1x1, group or call-link call)
 *     and the one call-signaling queue for every chat, so a call rings on
 *     any screen and survives a route change. Its UI (`CallHostView`) is
 *     rendered by the router's root layout, inside the router (and so inside
 *     the background receive, which its group-call banner reads); its state
 *     lives here, above it.
 *
 *   <BackgroundReceiveProvider> — receives every joined group whose chat
 *     view is not open, feeding the call host and the message log. Inside
 *     the call host, which it hands call signaling to.
 */
const App: Component = () => (
  <ChatProvider>
    <AuthProvider>
      <DeviceGroupSyncProvider>
        <ProfileProvider>
        <GroupNameProvider>
        <MailboxProvider>
        <CallHostProvider>
        <BackgroundReceiveProvider>
          <Router root={RootLayout}>{routes}</Router>
          {/*
           * Per-session hydration of the channel list / subscriber seeds /
           * publisher keys from the encrypted channel log, and the restore of
           * the PublisherGroup provider it carries. Whole-session for the
           * same reason as the receivers above: the list must exist before
           * the Channels route mounts, and the restore must precede any
           * PublisherGroup open.
           */}
          <ChannelsHydration />
          {/*
           * A Welcome this device could not open. Top level for the same
           * reason as the update banner below: it is received for the whole
           * session, and the user is rarely on Contacts when it arrives.
           */}
          <MailboxJoinFailureBanner />
          {/*
           * Proof-of-work runs in a Web Worker and can take a long time at the
           * production effort; this says the app is working rather than hung.
           */}
          <PowActivityIndicator />
          {/*
           * 1.18f — "new version available" toast. Mounted at
           * the top level so it overlays every route (auth shell
           * + main app shell + invite landing). No route knows
           * it exists; it self-subscribes to the SW_UPDATE_EVENT
           * dispatched by `sw/register.ts`.
           */}
          <SwUpdateBanner />
          {/*
           * ADR-0045 UX §14 — the in-page confirmation that replaced
           * `window.confirm`. Mounted here, beside the update banner and for
           * the same reason: it must overlay every route (auth shell + main
           * app shell + invite landing) and no route knows it exists. Callers
           * reach it through `requestConfirm`; with no host mounted every
           * request resolves `false`, so this mount is what makes the
           * destructive controls live at all.
           */}
          <ConfirmDialog />
        </BackgroundReceiveProvider>
        </CallHostProvider>
        </MailboxProvider>
        </GroupNameProvider>
        </ProfileProvider>
      </DeviceGroupSyncProvider>
    </AuthProvider>
  </ChatProvider>
);

export default App;
