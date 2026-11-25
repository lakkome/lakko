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
 *   2. **Protected** (`/`, `/chat/:id`, `/channel/:id`, `/groups`, `/contacts`,
 *      `/settings`, `/devices`, `/dev`) — wrapped in `<RequireSession>` and rendered inside the
 *      `<AppShell>` layout. The wrapper short-circuits to `/login`
 *      when the user is anonymous.
 *
 * Per-route lazy loading is intentionally NOT used in 1.16b: every
 * route is tiny, lazy chunks add complexity, and Vite's chunking
 * behaves better when the wasm chunk is the only big payload. We
 * revisit lazy loading in 1.16f together with the bundle pass.
 */
import { Navigate, type RouteDefinition } from "@solidjs/router";
import { AppShell } from "../ui/AppShell";
import { RequireAnonymous, RequireSession } from "../auth/guards";
import Login from "./Login";
import Recover from "./Recover";
import Signup from "./Signup";
import Home from "./Home";
import ChatsLayout from "./ChatsLayout";
import { GroupsIndex } from "./GroupsLayout";
import NewGroup from "./NewGroup";
import Saved from "./Saved";
import Contacts from "./Contacts";
import Settings from "./Settings";
import Call from "./Call";
import Calls from "./Calls";
import Feed from "./Feed";
import FeedImport from "./FeedImport";
import ChannelConversation from "./ChannelConversation";
import NewChannel from "./NewChannel";
import Chat from "./Chat";
import PendingChat from "./PendingChat";
import DevProbe from "./DevProbe";
import Devices from "./Devices";
import Invite from "./Invite";
import JoinGroup from "./JoinGroup";
import JoinCall from "./JoinCall";
import NotFound from "./NotFound";
import Donations from "./Donations";
import {
  callJoinRoutePattern,
  callRoutePattern,
  channelRoutePattern,
  chatRoutePattern,
  contactRoutePattern,
  groupChatRoutePattern,
  groupJoinRoutePattern,
  inviteRoutePattern,
  paths,
  pendingChatRoutePattern,
} from "./paths";

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

  // docs/ux/donations.md: the ways to donate. Public, signed in or not.
  { path: paths.donations, component: Donations },

  // Protected pages — every child renders inside <AppShell />.
  {
    path: "/",
    component: (props) => (
      <RequireSession>
        <AppShell>{props.children}</AppShell>
      </RequireSession>
    ),
    children: [
      // Chats: ONE list pane for every conversation, kept mounted while the
      // person moves between them. Groups open at `/groups/:id` (Back returns
      // to `/groups`), but under this same layout: as a sibling layout of its
      // own, every move between a chat and a group unmounted the list and
      // drew it again from empty (seen on the desktop app, 2026-11-25).
      {
        path: "/",
        component: ChatsLayout,
        children: [
          { path: paths.home, component: Home },
          { path: chatRoutePattern, component: () => <Chat backTo={paths.home} /> },
          // ADR-0079 — the chat with an inviter whose app has not answered.
          { path: pendingChatRoutePattern, component: PendingChat },
          // ADR-0087 section 12 — a channel opens beside the same list; the
          // create form too. `/channel/new` precedes the id pattern.
          { path: paths.channelNew, component: NewChannel },
          { path: channelRoutePattern, component: ChannelConversation },
          { path: paths.groups, component: GroupsIndex },
          // `/groups/new` MUST precede `/groups/:id` so the id pattern does
          // not swallow it.
          { path: paths.newGroup, component: NewGroup },
          {
            path: `${paths.groups}${groupChatRoutePattern}`,
            component: () => <Chat backTo={paths.groups} />,
          },
        ],
      },
      // `/contacts` and `/contacts/:key` are one route: the second is the
      // list with a person selected, which is what a one-to-one's header
      // opens (`docs/ux/conversation-info.md` section 3).
      { path: contactRoutePattern, component: Contacts },
      // `docs/ux/navigation.md` section 5 - Calls is a destination again, and
      // this time it has the history the redesign removed it for lacking.
      // `docs/ux/feed.md` section 2 - Feed is the second destination.
      { path: paths.feed, component: Feed },
      { path: paths.feedImport, component: FeedImport },
      { path: paths.calls, component: Calls },
      // ADR-0065 — saved messages, from the Chats screen's menu.
      { path: paths.saved, component: Saved },
      { path: "/settings/:section?", component: Settings },
      // The old profile URL lands on its Settings section.
      { path: "/profile", component: () => <Navigate href={paths.settingsSection("profile")} /> },
      // Standalone design route — render Call with no live-call props (its
      // embedded/onExit props are only used when the call host presents it
      // over a live call). Wrapping decouples Call's prop type from the
      // router's RouteSectionProps signature.
      //
      // It used to take `?call=video` to open the pre-join screen at the video
      // default. There is no pre-join screen (docs/ux/calls.md section 3: a
      // voice call dials immediately, a video call gets the camera check in
      // section 3.2, and neither is a route), so the design route seeds its own
      // mock call and the parameter is gone.
      { path: callRoutePattern, component: () => <Call /> },
      { path: paths.dev, component: DevProbe },
      { path: paths.devices, component: Devices },
    ],
  },

  // Catch-all 404.
  { path: "*", component: NotFound },
];
