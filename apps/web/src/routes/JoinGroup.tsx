// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `/g/:slug` — group join-ticket landing (ADR-0028 Phase 3, G-3).
 *
 * The inverse of admin-adds-by-username: a non-member opens an owner-minted
 * `https://lakko.me/g/<slug>` link, decodes + verifies the
 * `GroupInviteCapability`, and requests to join. "Request to join" posts an MLS
 * external-join proposal (ADR-0027) to the group journal; an owner/admin
 * approves it from their members panel; and — since ADR-0045 S7 — whichever
 * device is the acting committer then sweeps that approval into a commit and
 * sends the Welcome to this device's own mailbox, where the mailbox receive
 * loop joins it; this page waits for the group to appear. Since ADR-0053 the
 * request travels as a knock, one anonymous token each since ADR-0054 S3
 * (the journal of a claimed group answers
 * only its members), and the knock's response is what tells the joiner the
 * epoch its proposal must target.
 *
 * That last step is why the wait is TWO-stage and the copy says so. Approval no
 * longer admits anyone by itself: it publishes an `Add` proposal, and ADR-0045
 * decision 2 gives the sweep that commits it no deadline, so a request can sit
 * approved-but-not-yet-admitted for as long as no candidate committer is
 * running. Promising immediacy here would be the optimistic-update lie the UX
 * spec forbids.
 *
 * The link conveys only the `group_id` — it authorizes nobody; the admin
 * approval gate is the security boundary. Public route (anonymous visitors land
 * here and are prompted to log in), mirroring the `/i/` contact-invite route.
 *
 * Since ADR-0028 v2 the capability also carries a sealed preview (the group's
 * name, the inviter's name, the member count at mint time) under a link key
 * that rides the URL fragment; the card's headline comes from it
 * (docs/ux/group-invite-page.md section 3), and the issuer fingerprint moves
 * to the foot. A missing or refused preview shows the same card without a
 * headline name — never a guess, never an error.
 *
 * ## What this side is NOT told (ADR-0045 S7 residual 1)
 *
 * A sweep can exclude the approval-derived `Add` — `narrow_sweep_by_exclusion`
 * drops whatever refuses to build, and the commonest cause needs no adversary
 * at all (an expired KeyPackage, reachable because a request has no deadline).
 * This page is never told when that happens, and the reason is structural
 * rather than an omission: the only place a non-member can be reached is its
 * own mailbox, whose writes are open to anyone (the cold inbox, ADR-0053
 * section 2), so any "your request was not carried" envelope would be a
 * forgeable denial primitive. Inference does not work either: since ADR-0053
 * this side does not read the group at all, and silence is byte-for-byte what
 * *no admin has approved yet* looks like, which is the ordinary state.
 *
 * So the wait copy states the truth about the wait, and the page offers the one
 * repair this side has: `Request again`, user-driven, which mints a fresh
 * KeyPackage. It never claims that is why.
 */
import { Show, createMemo, createResource, createSignal, onCleanup } from "solid-js";
import { A, useLocation, useNavigate, useParams } from "@solidjs/router";
import { useAuth } from "../auth/AuthContext";
import { useChats } from "../chat/ChatContext";
import { ContactsClient } from "../contacts/client";
import { base64UrlEncodeNoPad } from "../auth/account-id";
import { base64Encode } from "../auth/account-id";
// ADR-0045 S7 residual 1, the joiner's half. The copy and the one repair live
// in a plain module because `vitest.config.ts` cannot test a `.tsx` file.
import {
  JOIN_REQUEST_AGAIN,
  JOIN_WAIT_PRIMARY,
  JOIN_WAIT_SECONDARY,
} from "../chat/unapplied-requests";
import { paths } from "./paths";
import { PublicLayout } from "../ui/PublicLayout";
import { joinGroupAuthLinks } from "./join-group-auth";
import {
  JOIN_REQUEST_FAILED,
  JOIN_SUCCEEDED_OPENING,
  JOIN_TRY_AGAIN,
  joinFailureNotice,
  type JoinFailureNotice,
} from "./join-group-failure";
import { recordConversationKind } from "../chat/conversation-kind-store";
// ADR-0028 v2 — the sealed preview and the link key in the URL fragment
// (docs/ux/group-invite-page.md). The copy lives in a plain module for the
// same `vitest` reason as the wait copy above.
import {
  INVITE_LINK_DAMAGED,
  INVITE_LINK_RATE_LIMITED,
  INVITE_LINK_TRY_AGAIN,
  isInviteLinkDamaged,
} from "../contacts/invite-link";
import { reachSession } from "../reach/reach-service";
import { journalHex } from "../ds/access";
import { isDiscoveryRateLimited } from "../discovery/client";
import {
  inviteHeadline,
  invitedByLine,
  resolveInvitePreview,
  type GroupInvitePreviewData,
} from "./join-group-preview";

type JoinPhase = "idle" | "requesting" | "waiting" | "joined" | "error";

interface DecodedTicket {
  /** The plaintext capability wire the sealed link opened to (the blob
   *  fetched from the ledger for a pointer, the slug itself when inline). */
  wire: Uint8Array;
  /** Non-null only for the server-backed pointer form; drives the consume gate
   *  (ADR-0028 OQ1). */
  blobId: Uint8Array | null;
  fingerprint: string;
  valid: boolean;
  /** The sealed preview, opened with the link secret from the URL fragment,
   *  or `null` when the link was minted without one. A preview that does not
   *  open never reaches here: the link is refused as damaged. */
  preview: GroupInvitePreviewData | null;
}

/** Why the page refuses to offer a join without the link being invalid. */
type LinkRefusal = "rate-limited" | "damaged";

export default function JoinGroup() {
  const params = useParams<{ slug: string }>();
  const location = useLocation();
  const auth = useAuth();
  const chats = useChats();
  const navigate = useNavigate();

  // Canonical identity provider — the same wasm Identity every Group on this
  // device uses (mirrors Invite.tsx).
  const client = new ContactsClient(undefined, undefined, {
    identityProvider: () => chats.identity(),
    // The join request mints a KeyPackage whose private key must survive
    // until the admin's Welcome arrives; without a persist hook a reload of
    // this page lost it and the Welcome could never be opened.
    onPersist: async () => {
      const s = auth.state();
      if (s.kind === "session") await chats.persistSnapshotDurable(s.account.accountId);
    },
  });

  // Resolve the slug (pointer => ledger fetch, inline => passthrough), then
  // decode + verify the capability in a memo-driven resource so the panel
  // renders the right state without imperative juggling. Re-runs on navigation
  // — of the slug or of the fragment, since the fragment holds the link key.
  const source = createMemo(() => ({ slug: params.slug, hash: location.hash }));
  const [decoded, { refetch }] = createResource(
    source,
    async ({ slug, hash }): Promise<DecodedTicket | LinkRefusal | null> => {
      try {
        // Sealed-link amendment of ADR-0028 (2026-09-28): the capability is
        // encrypted under the fragment's secret. A missing or malformed
        // fragment is damaged before any request; a blob that does not open
        // under it (a wrong secret, a tampered or substituted blob) is
        // damaged too. An old-width or malformed slug throws into the
        // invalid-or-expired state below and never reaches the network.
        const opened = await client.openGroupInviteLink(slug, hash);
        const cap = opened.capability;
        try {
          // A present preview that does not open under the same secret is
          // damaged as well: no preview-less join is offered.
          const outcome = resolveInvitePreview(cap, opened.linkSecret);
          if (outcome.kind === "damaged") return "damaged";
          return {
            wire: opened.capabilityWire,
            blobId: opened.blobId,
            fingerprint: cap.issuerFingerprint(),
            valid: cap.isValidNow(BigInt(Date.now())),
            preview: outcome.kind === "preview" ? outcome.preview : null,
          };
        } finally {
          cap.free();
        }
      } catch (err) {
        // A 429 says nothing about the link: "try again later", never
        // "invalid" (ADR-0028 short-link amendment).
        if (isDiscoveryRateLimited(err)) return "rate-limited";
        if (isInviteLinkDamaged(err)) return "damaged";
        // eslint-disable-next-line no-console
        console.warn("group-join: decode/verify failed", err);
        return null;
      }
    },
  );
  /** The decoded ticket, or `null` for every refusal. */
  const ticket = (): DecodedTicket | null => {
    const d = decoded();
    return d === undefined || d === null || typeof d === "string" ? null : d;
  };

  const [phase, setPhase] = createSignal<JoinPhase>("idle");
  /** The failure sentence and whether `Try again` is offered — never the raw
   *  error, which is logged instead. */
  const [error, setError] = createSignal<JoinFailureNotice | null>(null);
  /** In flight, so `Request again` cannot be double-tapped into two journal
   *  rows. Separate from `phase`, which stays `waiting` across a re-request —
   *  going back through `requesting` would hide the wait copy and read as a
   *  restart of something that never stopped. */
  const [requesting, setRequesting] = createSignal(false);
  let pollTimer: number | undefined;
  onCleanup(() => {
    if (pollTimer !== undefined) clearInterval(pollTimer);
  });

  async function onRequest(ticket: DecodedTicket): Promise<void> {
    if (requesting()) return;
    setError(null);
    setRequesting(true);
    // A re-request keeps the `waiting` copy on screen; the first request and
    // a `Try again` after a failure pass through `requesting`.
    if (phase() === "idle" || phase() === "error") setPhase("requesting");
    // A re-request replaces the poll rather than adding a second one.
    if (pollTimer !== undefined) clearInterval(pollTimer);
    try {
      const { groupId } = await client.requestToJoinFromLink(
        auth.identitySecrets(),
        ticket.wire,
        Date.now(),
        {
          blobId: ticket.blobId,
          joinerAccountId: auth.identitySecrets()?.accountId,
        },
      );
      setPhase("waiting");
      // ADR-0053 contract point 6 — wait for the group to APPEAR. The Welcome
      // arrives in this device's own mailbox (the committer's sweep delivers
      // it there like any other added member's), and the mailbox receive loop
      // (`MailboxContext`) joins it and registers the group. A claimed journal
      // answers only its members, so there is nothing to read on the group
      // side. ADR-0045 S7: an admin's approve publishes a proposal and the
      // Welcome exists only once a committer sweeps it, so this wait spans
      // both stages and keeps running until the group lands or the page
      // unmounts.
      const groupIdB64 = base64Encode(groupId);
      const finish = (active: { groupIdB64: string }): void => {
        if (pollTimer !== undefined) clearInterval(pollTimer);
        // docs/ux/client-redesign.md section 3 — a conversation joined
        // through an invite link is a group whatever its size.
        {
          const s = auth.state();
          if (s.kind === "session") recordConversationKind(s.account.accountId, active.groupIdB64, "group");
        }
        setPhase("joined");
        // ADR-0060 section 6: the Welcome landed, so the single-use reply key
        // registered for this request goes.
        void reachSession()?.closeRepliesTo(journalHex(groupId));
        navigate(paths.chat(base64UrlEncodeNoPad(groupId)));
      };
      pollTimer = window.setInterval(() => {
        const landed = chats.get(groupIdB64);
        if (landed === undefined) return;
        // ADR-0048 §2 — the mailbox loop already announced this device's
        // profile into the group it joined; recording the kind and opening
        // the chat is what is left.
        finish(landed);
      }, 1000);
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("group-join: request failed", err);
      setError(joinFailureNotice(err));
      setPhase("error");
    } finally {
      setRequesting(false);
    }
  }

  // The card's headline (spec section 3): the inviter as the subject, the
  // group's name or `a group`, the count at mint time; without a preview,
  // `You are invited to a group` and nothing more.
  const headline = createMemo(() => inviteHeadline(ticket()?.preview ?? null));
  const authLinks = () => joinGroupAuthLinks(params.slug, location.hash);

  return (
    <PublicLayout>
      <Show when={decoded.loading}>
        <h1>Join a group</h1>
        <p>Loading invite…</p>
      </Show>
      <Show when={!decoded.loading && (decoded() === null || ticket()?.valid === false)}>
        <h1>Join a group</h1>
        <p>This invite link is invalid or has expired.</p>
        <A href={paths.home}>Open Lakko</A>
      </Show>
      <Show when={!decoded.loading && decoded() === "damaged"}>
        <h1>Join a group</h1>
        <p>{INVITE_LINK_DAMAGED}</p>
        <A href={paths.home}>Open Lakko</A>
      </Show>
      <Show when={!decoded.loading && decoded() === "rate-limited"}>
        <h1>Join a group</h1>
        <p>{INVITE_LINK_RATE_LIMITED}</p>
        <button type="button" class="primary" onClick={() => void refetch()}>
          {INVITE_LINK_TRY_AGAIN}
        </button>
        <A href={paths.home}>Open Lakko</A>
      </Show>
      <Show when={!decoded.loading && ticket()?.valid === true}>
        <Show when={headline().title} fallback={<h1>{headline().lead}</h1>}>
          {(title) => (
            <>
              <p style={{ margin: "0 0 4px" }}>{headline().lead}</p>
              <h1 style={{ "margin-top": "0" }}>{title()}</h1>
            </>
          )}
        </Show>
        <Show when={headline().members}>{(members) => <p>{members()}</p>}</Show>
        <Show
          when={auth.state().kind === "session" && auth.identitySecrets() !== null}
          fallback={
            <>
              <p>You need an account to ask to join. Create one, or log in.</p>
              <div class="lk-actions">
                <A href={authLinks().signup}>
                  <button class="primary" type="button">
                    Sign up
                  </button>
                </A>
                <A href={authLinks().login}>
                  <button type="button">Log in</button>
                </A>
              </div>
              <p class="lk-meta">{invitedByLine(ticket()?.fingerprint ?? "")}</p>
            </>
          }
        >
          <Show when={phase() === "idle"}>
            <button type="button" class="primary" onClick={() => void onRequest(ticket()!)}>
              Request to join
            </button>
          </Show>
          <Show when={phase() === "requesting"}>
            <p>Sending request…</p>
          </Show>
          <Show when={phase() === "waiting"}>
            <p>{JOIN_WAIT_PRIMARY}</p>
            <p style={{ color: "var(--surface-muted)", "font-size": "0.85rem" }}>
              {JOIN_WAIT_SECONDARY}
            </p>
            <button
              type="button"
              disabled={requesting()}
              onClick={() => void onRequest(ticket()!)}
            >
              {JOIN_REQUEST_AGAIN}
            </button>
          </Show>
          <Show when={phase() === "joined"}>
            <p>{JOIN_SUCCEEDED_OPENING}</p>
          </Show>
          <Show when={phase() === "error"}>
            <p style={{ color: "var(--error)" }}>{error()?.message ?? JOIN_REQUEST_FAILED}</p>
            <Show when={error()?.offerRetry !== false}>
              <button
                type="button"
                class="primary"
                disabled={requesting()}
                onClick={() => void onRequest(ticket()!)}
              >
                {JOIN_TRY_AGAIN}
              </button>
            </Show>
          </Show>
          {/* The one thing on the page the inviter cannot fake stays, at the
              foot and in the muted style, for a person who was told the
              digits out of band. */}
          <p class="lk-meta">{invitedByLine(ticket()?.fingerprint ?? "")}</p>
        </Show>
      </Show>
    </PublicLayout>
  );
}
