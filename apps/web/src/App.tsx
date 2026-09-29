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
import { CallActionListener } from "./calls/CallActionListener";
import { CallHostProvider, CallHostView } from "./calls/CallHostContext";
import { ChannelsHydration } from "./channels/ChannelsHydration";
import { DeviceGroupSyncProvider } from "./devices/sync/DeviceGroupSyncContext";
import {
  MailboxJoinFailureBanner,
  MailboxProvider,
} from "./contacts/MailboxContext";
import { OneToOneContactAdoption } from "./contacts/OneToOneContactAdoption";
import { ProfileProvider } from "./profile/ProfileContext";
import { ReachProvider } from "./reach/ReachProvider";
import { ScopedDiscoveryRuntime } from "./reach/ScopedDiscoveryRuntime";
import { GroupNameProvider } from "./chat/GroupNameContext";
import { JournalOwnershipProvider } from "./ds/JournalOwnershipContext";
import { PinProvider } from "./chat/PinContext";
import { ReadWatermarkProvider } from "./chat/ReadWatermarkContext";
import { CallLogProvider } from "./calls/CallLogContext";
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
    {/*
     * Accept / Decline pressed on a ringing background notification. Here for
     * the same reason as the reader above — it must survive the cold-open
     * login bounce, because on that path the user pressed Accept before the
     * app existed and the call has not reached the slot yet.
     */}
    <CallActionListener />
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
 *   <ReachProvider> — ADR-0058 scoped discovery: the session's reach keys
 *     and the "who can find me" setting, synced to the account mailbox.
 *     Above ProfileProvider, which reads the grants a profile carries.
 *   <ProfileProvider> — ADR-0048 profile names: the own profile, the
 *     received-profile store, username verification, publishing into groups
 *     and the one display resolver. Inside Auth + Chat (it reads the session
 *     and publishes through the canonical identity) and ABOVE the mailbox,
 *     whose Welcome joins and contact requests feed it.
 *
 *   <JournalOwnershipProvider> — ADR-0057 journal ownership: which journals
 *     this account owns (root), the registrar seeds other owners handed it,
 *     and the owner's appointment of registrars. Inside the profile provider
 *     (it reads the account ids profiles carry) and ABOVE every receive loop,
 *     which hands it registrar-seed envelopes.
 *
 *   <GroupNameProvider> — ADR-0051 shared group names: the per-group store,
 *     the role-gated receive and the growth re-send. Inside Auth + Chat for
 *     the same reasons, and ABOVE the mailbox, whose Welcome joins title
 *     from it.
 *
 *   <PinProvider> — ADR-0052 decision 3 pinned messages: the per-conversation
 *     store of shared and personal pins, the role-gated receive and the
 *     growth re-send. Beside the group-name provider, for the same reasons.
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
 *   <ReadWatermarkProvider> — ADR-0024 read receipts: the durable
 *     per-group watermark store both receive paths feed, so "Read" survives
 *     a reload and a receipt applied while the chat was closed is kept.
 *
 *   <CallLogProvider> — docs/ux/calls.md section 6: the durable per-group
 *     record of the calls this device was in, so a finished call leaves a
 *     row in the conversation. ABOVE the call host, which writes to it: a
 *     call can end on any screen, and the row must be recorded there rather
 *     than only where the chat happens to be open.
 *
 *   <BackgroundReceiveProvider> — receives every joined group whose chat
 *     view is not open, feeding the call host and the message log. Inside
 *     the call host, which it hands call signaling to.
 */
const App: Component = () => (
  <ChatProvider>
    <AuthProvider>
      <DeviceGroupSyncProvider>
        <ReachProvider>
        <ProfileProvider>
        <JournalOwnershipProvider>
        <GroupNameProvider>
        <PinProvider>
        <ReadWatermarkProvider>
        <CallLogProvider>
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
           * ADR-0058 — what this account publishes to be found by (the
           * Everyone publication, the contacts-of-contacts record and the
           * relayed bundle). Whole-session, like the receivers above.
           */}
          <ScopedDiscoveryRuntime />
          {/*
           * A Welcome this device could not open. Top level for the same
           * reason as the update banner below: it is received for the whole
           * session, and the user is rarely on Contacts when it arrives.
           */}
          <MailboxJoinFailureBanner />
          {/*
           * A person this account has a one-to-one conversation with IS a
           * contact. Whole-session: the conversation arrives on any route (a
           * Welcome, a second device's admission) and the Contacts list must
           * hold the person before it is first opened.
           */}
          <OneToOneContactAdoption />
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
        </CallLogProvider>
        </ReadWatermarkProvider>
        </PinProvider>
        </GroupNameProvider>
        </JournalOwnershipProvider>
        </ProfileProvider>
        </ReachProvider>
      </DeviceGroupSyncProvider>
    </AuthProvider>
  </ChatProvider>
);

export default App;
