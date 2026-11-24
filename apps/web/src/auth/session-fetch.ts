// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `fetch` for every bearer request of the app (ADR-0073 section 3.3).
 *
 * A drop-in for `globalThis.fetch`: a request whose `Authorization` header
 * carries a token the session provider manages is refreshed ahead of expiry
 * and retried once after a refresh on a `401`; every other request passes
 * through. Clients import this instead of calling `fetch` on their bearer
 * routes, so the one rule lives in `session-token-provider.ts`.
 */
import { sessionTokens } from "./session-token-provider";
import { observeRequest } from "../net/connection-state";

export function sessionFetch(input: RequestInfo | URL, init?: RequestInit): Promise<Response> {
  // docs/ux/connection-state.md: every bearer request says whether this
  // device can reach the servers.
  return observeRequest(sessionTokens.fetch(input, init), init?.signal);
}
