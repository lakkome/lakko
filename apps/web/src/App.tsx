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
 * No business logic in this file on purpose: every screen lives in
 * its own module so swapping a route is a single import change.
 */
import type { Component } from "solid-js";
import { Router } from "@solidjs/router";
import { AuthProvider } from "./auth/AuthContext";
import { ChatProvider } from "./chat/ChatContext";
import { routes } from "./routes/routes";

const App: Component = () => (
  <AuthProvider>
    <ChatProvider>
      <Router>{routes}</Router>
    </ChatProvider>
  </AuthProvider>
);

export default App;
