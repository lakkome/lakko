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
 *   2. **Protected** (`/`, `/contacts`, `/chat/:id`, `/dev`) —
 *      wrapped in `<RequireSession>` and rendered inside the
 *      `<AppShell>` layout. The wrapper short-circuits to `/login`
 *      when the user is anonymous.
 *
 * Per-route lazy loading is intentionally NOT used in 1.16b: every
 * route is tiny, lazy chunks add complexity, and Vite's chunking
 * behaves better when the wasm chunk is the only big payload. We
 * revisit lazy loading in 1.16f together with the bundle pass.
 */
import type { RouteDefinition } from "@solidjs/router";
import { AppShell } from "../ui/AppShell";
import { RequireAnonymous, RequireSession } from "../auth/guards";
import Login from "./Login";
import Recover from "./Recover";
import Signup from "./Signup";
import Home from "./Home";
import Contacts from "./Contacts";
import Call from "./Call";
import { ChannelsList, ChannelView, NewChannel } from "./Channels";
import Chat from "./Chat";
import DevProbe from "./DevProbe";
import Devices from "./Devices";
import Invite from "./Invite";
import NotFound from "./NotFound";
import {
  callRoutePattern,
  channelRoutePattern,
  chatRoutePattern,
  inviteRoutePattern,
  paths,
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

  // Protected pages — every child renders inside <AppShell />.
  {
    path: "/",
    component: (props) => (
      <RequireSession>
        <AppShell>{props.children}</AppShell>
      </RequireSession>
    ),
    children: [
      { path: paths.home, component: Home },
      { path: paths.contacts, component: Contacts },
      { path: chatRoutePattern, component: Chat },
      { path: callRoutePattern, component: Call },
      { path: paths.channels, component: ChannelsList },
      { path: paths.channelNew, component: NewChannel },
      { path: channelRoutePattern, component: ChannelView },
      { path: paths.dev, component: DevProbe },
      { path: paths.devices, component: Devices },
    ],
  },

  // Catch-all 404.
  { path: "*", component: NotFound },
];
