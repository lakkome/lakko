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
 * Introduced in 1.16b as a routing scaffold; the screens it points
 * at are now implemented — 1.16c (auth), 1.16d (contacts) and 1.16e
 * (chat) filled them in. The authenticated landing (`home`) remains
 * a placeholder shell pending the inbox / conversation-list view.
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
  /** Group conversations — the second primary destination. */
  groups: "/groups",
  /** New group: name it, pick the first members from your contacts. */
  newGroup: "/groups/new",
  /** Contact list and add-contact entry point. */
  contacts: "/contacts",
  /**
   * One contact's own page - the Contacts route with that person selected
   * (`docs/ux/conversation-info.md` section 3, "Except a person"). This is
   * what a one-to-one conversation's header opens, so a person has ONE card
   * rather than a second one rendered as a layer over the chat. `keyB64Url`
   * is the `base64UrlEncodeNoPad` of the person's Ed25519 identity key -
   * the key the contact list addresses them by, made safe for a path
   * segment (the stored form is standard base64, which carries `/`).
   */
  contact(keyB64Url: string): string {
    return `/contacts/${keyB64Url}`;
  },
  /** Settings — Profile · Privacy · Security · Chat folders · Appearance · Language ·
   *  Media · About · Account. */
  settings: "/settings",
  /** Settings sub-sections; `settings` itself lands on Profile. `recovery`
   *  is reached from Security (authenticator, email or phone), not from the
   *  section list; `privacy-groups` (the chosen groups and channels) from
   *  Privacy. */
  settingsSection(
    section:
      | "profile"
      | "privacy"
      | "privacy-groups"
      | "security"
      | "folders"
      | "appearance"
      | "language"
      | "media"
      | "about"
      | "account"
      | "recovery",
  ): string {
    return `/settings/${section}`;
  },
  /** The viewer's own profile name (ADR-0048) — the first Settings section. */
  profile: "/settings/profile",
  /** Calls — the call history and one action, `New call`
   *  (`docs/ux/call-links.md` section 2). Also where a call started from that
   *  button returns to while it is still connecting. */
  /** `docs/ux/feed.md` - the ranked feed of group messages and channel posts. */
  feed: "/feed",
  /** A shared feed opened from its link or file (`docs/ux/feed.md` 4.1); the
   *  rule rides in the fragment. `feed/feed-draft.ts` `shareLink` builds it. */
  feedImport: "/feed/import",
  calls: "/calls",
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
   * The same conversation opened from the Groups list. Both URLs render the
   * one chat view; the prefix decides which list pane stays beside it and
   * where Back returns to.
   */
  groupChat(groupIdB64Url: string): string {
    return `/groups/${groupIdB64Url}`;
  },
  /**
   * Build the path of an invite link for a slug (1.18c): the
   * 8-character ledger id of the sealed capability, or the sealed
   * blob itself. The link secret is NOT part of the path: it rides
   * in the fragment (`contacts/invite.ts::composePointerInviteUrl`
   * builds the whole URL). The route handler at `/i/:slug` opens the
   * link with the fragment's secret and drives the joiner-side
   * ContactRequest send.
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
   * Multi-joiner call-invite landing (ADR-0032, G-21/G-22). `slug` is the
   * base64url-no-pad of a `CallInviteCapability` blob_id (pointer form) or
   * the inline capability bytes — the same `/c/<slug>` link
   * `calls/invite.ts::composeCallInviteUrl` mints. The landing screen
   * decodes + verifies the capability and drives the joiner (or, when the
   * visitor is the owner, the host) flow.
   */
  joinCall(slug: string): string {
    return `/c/${slug}`;
  },
  /**
   * Channel list — Phase 4 G4-2. Shows the user's
   * registered channels with a + entry point that goes to
   * `paths.channelNew()`.
   */
  channels: "/channels",
  /** New-channel form — Phase 4 G4-2. */
  channelNew: "/channels/new",
  /** Join an existing channel as a co-publisher from an owner's link
   *  (ADR-0028 OQ3); the second entry of the Channels compose menu. */
  channelJoin: "/channels/join",
  /**
   * Channel view — Phase 4 G4-2. `channelIdHex` is the
   * 32-char lowercase hex form of the 16-byte channel
   * pseudonym (ADR-0010 §"Wire surfaces"); the
   * `services/channels` REST routes accept the same form.
   */
  channel(channelIdHex: string): string {
    return `/channels/${channelIdHex}`;
  },
  /**
   * Subscribe-to-channel view — Phase 4 G4-2 (subscriber
   * side). Generates a local X25519 keypair on first
   * visit, registers it via
   * `POST /v1/channels/:id/subscribers`, polls for a
   * seed envelope, opens it, and renders decoded posts.
   */
  channelSubscribe(channelIdHex: string): string {
    return `/channels/${channelIdHex}/subscribe`;
  },
} as const;

/**
 * The matchable pattern for the dynamic chat route — must stay in
 * sync with `paths.chat(...)`. Used in the route table.
 */
export const chatRoutePattern = "/chat/:id" as const;

/**
 * The matchable pattern for the Contacts route, with or without a selected
 * person - must stay in sync with `paths.contacts` and `paths.contact(...)`.
 */
export const contactRoutePattern = "/contacts/:key?" as const;

/**
 * The matchable pattern for a conversation opened from the Groups list —
 * must stay in sync with `paths.groupChat(...)`. Relative to the Groups
 * layout (`paths.groups`).
 */
export const groupChatRoutePattern = "/:id" as const;

/**
 * The matchable pattern for the dynamic invite-link route — must
 * stay in sync with `paths.invite(...)`. Used in the route table.
 */
export const inviteRoutePattern = "/i/:slug" as const;

/**
 * The matchable pattern for the group join-ticket route (ADR-0028, G-3) — the
 * `/g/<slug>#<secret>` link an owner mints for a non-member to request to
 * join. `slug` is the 8-character ledger id of the sealed capability, or the
 * sealed blob itself (inline); the fragment is the link secret it opens
 * under (2026-09-28 security pass).
 */
export const groupJoinRoutePattern = "/g/:slug" as const;

/**
 * The matchable pattern for the dynamic call route — must
 * stay in sync with `paths.call(...)`. Used in the route
 * table.
 */
export const callRoutePattern = "/call/:id" as const;

/**
 * The matchable pattern for the multi-joiner call-invite landing route
 * (ADR-0032, G-21/G-22) — must stay in sync with `paths.joinCall(...)`.
 * `slug` is the 8-character ledger id of a sealed `CallInviteCapability`, or
 * the sealed blob itself (inline); the fragment is the link secret.
 */
export const callJoinRoutePattern = "/c/:slug" as const;

/**
 * The matchable pattern for the dynamic channel view route
 * — must stay in sync with `paths.channel(...)`.
 */
export const channelRoutePattern = "/channels/:id" as const;

/**
 * The matchable pattern for the dynamic subscribe-to-
 * channel route — must stay in sync with
 * `paths.channelSubscribe(...)`.
 */
export const channelSubscribeRoutePattern =
  "/channels/:id/subscribe" as const;
