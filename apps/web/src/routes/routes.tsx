// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Route table for `apps/web`.
 *
 * The shape mirrors `routes/paths.ts` 1:1 — when this file changes,
 * `paths.ts` changes in the same commit.
 *
 * Two route groups:
 *
 *   1. **Public** (`/login`, `/signup`) — wrapped in
 *      `<RequireAnonymous>` so a signed-in user gets bounced home
 *      instead of landing on a login form they cannot use.
 *
 *   2. **Protected** (`/`, `/chat/:id`, `/groups`, `/contacts`, `/channels`,
 *      `/settings`, `/devices`, `/dev`) — wrapped in `<RequireSession>` and rendered inside the
 *      `<AppShell>` layout. The wrapper short-circuits to `/login`
 *      when the user is anonymous.
 *
 * Per-route lazy loading is intentionally NOT used in 1.16b: every
 * route is tiny, lazy chunks add complexity, and Vite's chunking
 * behaves better when the wasm chunk is the only big payload. We
 * revisit lazy loading in 1.16f together with the bundle pass.
 */
import type { JSX } from "solid-js";
import { Navigate, useSearchParams, type RouteDefinition } from "@solidjs/router";
import { AppShell } from "../ui/AppShell";
import { RequireAnonymous, RequireSession } from "../auth/guards";
import Login from "./Login";
import Recover from "./Recover";
import Signup from "./Signup";
import Home from "./Home";
import ChatsLayout from "./ChatsLayout";
import GroupsLayout, { GroupsIndex } from "./GroupsLayout";
import NewGroup from "./NewGroup";
import Contacts from "./Contacts";
import Settings from "./Settings";
import Call from "./Call";
import { ChannelSubscribe, ChannelView, JoinCoPublisher, NewChannel } from "./Channels";
import ChannelsLayout, { ChannelsIndex } from "./ChannelsLayout";
import Chat from "./Chat";
import DevProbe from "./DevProbe";
import Devices from "./Devices";
import Invite from "./Invite";
import JoinGroup from "./JoinGroup";
import JoinCall from "./JoinCall";
import NotFound from "./NotFound";
import {
  callJoinRoutePattern,
  callRoutePattern,
  chatRoutePattern,
  groupChatRoutePattern,
  groupJoinRoutePattern,
  inviteRoutePattern,
  paths,
} from "./paths";

/** The standalone design route's Call, opened at the kind the URL asked for. */
function StandaloneCall(): JSX.Element {
  const [searchParams] = useSearchParams<{ call?: string }>();
  return <Call startKind={searchParams.call === "video" ? "video" : "audio"} />;
}

/** All app routes. Consumed by `<Router>` in `App.tsx`. */
export const routes: RouteDefinition[] = [
  // Public auth pages — bare layout, no shell.
  {
    path: paths.login,
    component: () => (
      <RequireAnonymous>
        <Login />
      </RequireAnonymous>
    ),
  },
  {
    path: paths.signup,
    component: () => (
      <RequireAnonymous>
        <Signup />
      </RequireAnonymous>
    ),
  },
  {
    path: paths.recover,
    component: () => (
      <RequireAnonymous>
        <Recover />
      </RequireAnonymous>
    ),
  },

  // Invite-link landing — Phase 1.18c. Public route: anyone
  // who clicks an invite URL must be able to land here, and
  // the component itself drives the "log in / sign up" prompt
  // when the visitor is anonymous. NOT wrapped in
  // <RequireAnonymous> (auth users open invites too) nor
  // <RequireSession> (anon users CAN open it; the component
  // handles the gating).
  { path: inviteRoutePattern, component: Invite },

  // Group join-ticket landing (ADR-0028, G-3) — same public posture as the
  // contact-invite route: a non-member opens an owner's `/g/<slug>` link, the
  // component decodes + verifies the capability and drives the request-to-join
  // flow, prompting anonymous visitors to log in first.
  { path: groupJoinRoutePattern, component: JoinGroup },

  // Multi-joiner call-invite landing (ADR-0032, G-21/G-22) — same public
  // posture: whoever opens a `/c/<slug>` call link lands here; the component
  // decodes + verifies the CallInviteCapability and drives the join (or, when
  // the visitor is the owner, the host) flow, prompting anon visitors to log
  // in first.
  { path: callJoinRoutePattern, component: JoinCall },

  // Protected pages — every child renders inside <AppShell />.
  {
    path: "/",
    component: (props) => (
      <RequireSession>
        <AppShell>{props.children}</AppShell>
      </RequireSession>
    ),
    children: [
      // Chats: the list pane stays mounted across `/` and `/chat/:id`.
      {
        path: "/",
        component: ChatsLayout,
        children: [
          { path: paths.home, component: Home },
          { path: chatRoutePattern, component: () => <Chat backTo={paths.home} /> },
        ],
      },
      // Groups: the same chat view, beside the group list; Back returns to it.
      {
        path: paths.groups,
        component: GroupsLayout,
        children: [
          { path: "/", component: GroupsIndex },
          // `/new` MUST precede `/:id` so the id pattern does not swallow it.
          { path: "/new", component: NewGroup },
          { path: groupChatRoutePattern, component: () => <Chat backTo={paths.groups} /> },
        ],
      },
      { path: paths.contacts, component: Contacts },
      // The former Calls destination: call links live in the Contacts add menu.
      { path: paths.calls, component: () => <Navigate href={paths.contacts} /> },
      { path: "/settings/:section?", component: Settings },
      // The old profile URL lands on its Settings section.
      { path: "/profile", component: () => <Navigate href={paths.settingsSection("profile")} /> },
      // Standalone design route — render Call with no live-call props (its
      // embedded/onExit props are only used when the call host presents it
      // over a live call). Wrapping decouples Call's prop type from the
      // router's RouteSectionProps signature.
      //
      // `?call=video` opens the pre-join screen at the video default, the same
      // parameter the contact detail's Call / Video buttons already use to
      // reach a chat (docs/ux/client-redesign.md section 5.3). Anything else,
      // the parameter absent included, is a voice call: docs/ux/calls.md
      // section 3 — the camera is never turned on by a screen that was not
      // asked to.
      { path: callRoutePattern, component: () => <StandaloneCall /> },
      // Channels: the list pane stays mounted across every /channels route.
      {
        path: paths.channels,
        component: ChannelsLayout,
        children: [
          { path: "/", component: ChannelsIndex },
          { path: "/new", component: NewChannel },
          { path: "/join", component: JoinCoPublisher },
          // Subscribe route MUST be matched BEFORE the generic `/:id`
          // pattern so `:id` does not swallow the trailing `/subscribe`.
          { path: "/:id/subscribe", component: ChannelSubscribe },
          { path: "/:id", component: ChannelView },
        ],
      },
      { path: paths.dev, component: DevProbe },
      { path: paths.devices, component: Devices },
    ],
  },

  // Catch-all 404.
  { path: "*", component: NotFound },
];
