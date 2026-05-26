// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Typed URL path helpers for `apps/web`.
 *
 * Every navigation in the app goes through one of the constants /
 * builders here, which gives us:
 *
 *   1. A single source of truth for the URL surface — when a route
 *      moves, callers do not silently keep working against a 404.
 *   2. Type-checked params on builders (`chat(":id")` won't compile
 *      with the wrong arity).
 *   3. A grep target. `paths.login` and `paths.chat(...)` are
 *      trivially searchable.
 *
 * The shape mirrors the route table in `routes/routes.tsx` 1:1. If
 * a route is added or renamed, update both files in the same
 * commit.
 *
 * Phase 1.16b — pure routing scaffold; the screens are stubs that
 * 1.16c (auth), 1.16d (contacts) and 1.16e (chat) fill in.
 */

/** Static-path constants. */
export const paths = {
  /** Public landing for unauthenticated users — login form. */
  login: "/login",
  /** Public landing for unauthenticated users — signup form. */
  signup: "/signup",
  /** Recover-from-mnemonic flow — paste 24 words + new password
   *  to rotate the password envelope and re-establish a session. */
  recover: "/recover",
  /** Authenticated home / inbox. */
  home: "/",
  /** Contact list and add-contact entry point. */
  contacts: "/contacts",
  /**
   * Developer probe — runs the "Hello core" round-trip from 1.16a.
   * Kept off the main shell intentionally; users should never land
   * here in normal flows.
   */
  dev: "/dev",
  /** Multi-device management — pair / scan / list devices (2a3b2-ui). */
  devices: "/devices",
  /**
   * Build a chat URL for a specific MLS group. `groupIdB64Url`
   * MUST be a URL-safe-base64-no-pad encoding of the group's
   * `groupId` bytes (use `base64UrlEncodeNoPad` from
   * `auth/account-id.ts` to compute it). The router segment is
   * `:id` — the route handler decodes it and looks up the live
   * group reference in the `<ChatProvider>` registry.
   */
  chat(groupIdB64Url: string): string {
    return `/chat/${groupIdB64Url}`;
  },
  /**
   * Build an invite-link URL for a capability slug
   * (1.18c). `slug` is the base64url-no-pad encoding of the
   * `InviteCapability.compose(...)` wire bytes — see
   * `contacts/invite.ts::composeInviteUrl` for the canonical
   * builder that prepends the origin. The route handler at
   * `/i/:slug` decodes + verifies the capability and drives
   * the joiner-side ContactRequest send.
   */
  invite(slug: string): string {
    return `/i/${slug}`;
  },
  /**
   * In-call screen for an active call (Phase 3c6-web).
   * `id` is a call-scoped identifier — typically the
   * `base64UrlEncodeNoPad`-encoded 16-byte `call_id` from the
   * MLS exporter context. The route component reads `useParams`
   * and looks the call up in the in-memory call registry; if
   * the call is gone (e.g. the page was opened directly via
   * URL), the screen redirects back to `paths.home` after
   * showing the "Call ended" overlay.
   */
  call(callIdB64Url: string): string {
    return `/call/${callIdB64Url}`;
  },
  /**
   * Channel list — Phase 4 G4-2. Shows the user's
   * registered channels with a + entry point that goes to
   * `paths.channelNew()`.
   */
  channels: "/channels",
  /** New-channel form — Phase 4 G4-2. */
  channelNew: "/channels/new",
  /**
   * Channel view — Phase 4 G4-2. `channelIdHex` is the
   * 32-char lowercase hex form of the 16-byte channel
   * pseudonym (ADR-0010 §"Wire surfaces"); the
   * `services/channels` REST routes accept the same form.
   */
  channel(channelIdHex: string): string {
    return `/channels/${channelIdHex}`;
  },
} as const;

/**
 * The matchable pattern for the dynamic chat route — must stay in
 * sync with `paths.chat(...)`. Used in the route table.
 */
export const chatRoutePattern = "/chat/:id" as const;

/**
 * The matchable pattern for the dynamic invite-link route — must
 * stay in sync with `paths.invite(...)`. Used in the route table.
 */
export const inviteRoutePattern = "/i/:slug" as const;

/**
 * The matchable pattern for the dynamic call route — must
 * stay in sync with `paths.call(...)`. Used in the route
 * table.
 */
export const callRoutePattern = "/call/:id" as const;

/**
 * The matchable pattern for the dynamic channel view route
 * — must stay in sync with `paths.channel(...)`.
 */
export const channelRoutePattern = "/channels/:id" as const;
