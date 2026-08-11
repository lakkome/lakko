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
import { DeviceGroupSyncProvider } from "./devices/sync/DeviceGroupSyncContext";
import { WakeNavigator } from "./push/WakeNavigator";
import { routes } from "./routes/routes";
import { SwUpdateBanner } from "./sw/SwUpdateBanner";

/**
 * Router-level layout (G-36). Renders the active route unchanged and
 * mounts the push notification-tap reader alongside it.
 *
 * The reader has to live INSIDE `<Router>` (it calls `useNavigate`)
 * but OUTSIDE any individual route, so that a tap captured on a cold
 * `/?wake=…` open survives the `/` -> `/login` -> `/` bounce a
 * password re-entry performs. `<Router root>` is exactly that slot.
 */
const RootLayout: Component<RouteSectionProps> = (props) => (
  <>
    <WakeNavigator />
    {props.children}
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
 */
const App: Component = () => (
  <ChatProvider>
    <AuthProvider>
      <DeviceGroupSyncProvider>
        <Router root={RootLayout}>{routes}</Router>
        {/*
         * 1.18f — "new version available" toast. Mounted at
         * the top level so it overlays every route (auth shell
         * + main app shell + invite landing). No route knows
         * it exists; it self-subscribes to the SW_UPDATE_EVENT
         * dispatched by `sw/register.ts`.
         */}
        <SwUpdateBanner />
      </DeviceGroupSyncProvider>
    </AuthProvider>
  </ChatProvider>
);

export default App;
