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
 * (docs/ux/group-invite-page.md section 3), and the inviter's name stands at
 * the foot. A missing or refused preview shows the same card without a
 * headline name and without that line — never a guess, never an error, and
 * never the issuer's key as digits.
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
 * So the wait copy states the truth about the wait ("Request sent. An admin
 * will see it when they next open Lakko.", no control), and after a day the
 * page offers the one repair this side has: `Ask again`, user-driven, which
 * mints a fresh KeyPackage. It never claims that is why. A device asks once per
 * group per day (`docs/ux/group-and-channel-privacy.md` 6.2, amended
 * 2026-11-12): the request is recorded under `Requests sent`, and the page
 * opened again within the day shows the waiting line, not `Request to join`.
 */
import { Show, createEffect, createMemo, createResource, createSignal, onCleanup } from "solid-js";
import { A, useLocation, useNavigate, useParams } from "@solidjs/router";
import { useAuth } from "../auth/AuthContext";
import { useChats } from "../chat/ChatContext";
import { ContactsClient } from "../contacts/client";
import { base64UrlEncodeNoPad } from "../auth/account-id";
import { base64Decode, base64Encode } from "../auth/account-id";
// ADR-0045 S7 residual 1, the joiner's half. The copy and the page's states
// live in plain modules because `vitest.config.ts` cannot test a `.tsx` file.
import { JOIN_WAIT_PRIMARY } from "../chat/unapplied-requests";
import { recordRequestSent, sentRequestFor, type SentRequest } from "../chat/requests-sent";
import { sendRequestAgain } from "../chat/ask-again";
import { joinPageView, waitForGroup, type JoinPhase } from "./join-group-wait";
import { t } from "../l10n";
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
// ADR-0028 v2 — the sealed preview and the link key in the URL fragment
// (docs/ux/group-invite-page.md). The copy lives in a plain module for the
// same `vitest` reason as the wait copy above.
import {
  INVITE_LINK_DAMAGED,
  INVITE_LINK_OPEN_FAILED,
  INVITE_LINK_RATE_LIMITED,
  INVITE_LINK_TRY_AGAIN,
  isInviteLinkDamaged,
  isInviteLinkUnreachable,
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
import { durableSnapshotHook, sessionAccountOf } from "../chat/persist-hooks";
import { openChannelLink, type OpenedChannelInvite } from "../channels/channel-invites";
import { groupInviteRouteLink } from "../contacts/group-invite";
import { channelErrorCode } from "../channels/channel-error-copy";
import { ChannelLinkPage } from "./ChannelLinkPage";

interface DecodedTicket {
  /** The plaintext capability wire the sealed link opened to (the blob
   *  fetched from the ledger). */
  wire: Uint8Array;
  /** The ledger id; drives the consume gate (ADR-0028 OQ1). */
  blobId: Uint8Array;
  /** The group the link names, base64 (the `Requests sent` key). */
  groupIdB64: string;
  /** The group's knock key, base64, and its version; empty and 0 when the
   *  link carries none. */
  knockKeyB64: string;
  knockVersion: number;
  valid: boolean;
  /** The ledger says no uses are left. Read only with no session: a signed-in
   *  person may be the one who used it, and the consume tells them apart. */
  usedUp: boolean;
  /** The sealed preview, opened with the link secret from the URL fragment,
   *  or `null` when the link was minted without one. A preview that does not
   *  open never reaches here: the link is refused as damaged. */
  preview: GroupInvitePreviewData | null;
}

/** Why the page refuses to offer a join without the link being invalid. */
type LinkRefusal = "rate-limited" | "damaged" | "open-failed";

function JoinGroupFlow() {
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
    onPersist: durableSnapshotHook(chats, () => sessionAccountOf(auth.state())),
  });

  // Resolve the slug (a ledger fetch; an inline slug is malformed), then
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
          const knockKey = cap.knockPublicKey;
          const knockVersion = cap.knockVersion;
          return {
            wire: opened.capabilityWire,
            blobId: opened.blobId,
            groupIdB64: base64Encode(cap.groupId),
            knockKeyB64: knockKey !== undefined && knockVersion !== undefined ? base64Encode(knockKey) : "",
            knockVersion: knockKey !== undefined && knockVersion !== undefined ? knockVersion : 0,
            valid: cap.isValidNow(BigInt(Date.now())),
            usedUp: opened.usedUp,
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
        // The ledger could not be asked (the network, a server error): the
        // link may be fine, so it is offered again, never called invalid.
        if (isInviteLinkUnreachable(err)) return "open-failed";
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
  /** Whether the link can be offered: its own expiry, and with no session the
   *  ledger's used-up flag too. */
  const linkWorks = (): boolean => {
    const d = ticket();
    if (d === null || !d.valid) return false;
    return auth.state().kind === "session" || !d.usedUp;
  };

  const [phase, setPhase] = createSignal<JoinPhase>("idle");
  /** The failure sentence and whether `Try again` is offered — never the raw
   *  error, which is logged instead. */
  const [error, setError] = createSignal<JoinFailureNotice | null>(null);
  /** A failed `Ask again`: the line under the button, which stays. */
  const [askError, setAskError] = createSignal<JoinFailureNotice | null>(null);
  /** In flight, so `Request to join` or `Ask again` cannot be double-tapped
   *  into two requests. */
  const [requesting, setRequesting] = createSignal(false);
  // The day is crossed while the page is on screen too.
  const [now, setNow] = createSignal(Date.now());
  const clock = window.setInterval(() => setNow(Date.now()), 60_000);
  let stopWait: (() => void) | undefined;
  let unmounted = false;
  onCleanup(() => {
    unmounted = true;
    clearInterval(clock);
    stopWait?.();
  });

  /** This device's recorded request to the link's group, if any. Reactive. */
  const sent = (): SentRequest | undefined => {
    const d = ticket();
    return d === null ? undefined : sentRequestFor(d.groupIdB64);
  };
  const view = () => joinPageView({ phase: phase(), sentAtMs: sent()?.sentAtMs ?? null, nowMs: now() });

  /** Wait for the group to appear, then open its chat straight away. The
   *  Welcome arrives in this device's own mailbox (the committer's sweep
   *  delivers it there like any other added member's), and the mailbox
   *  receive loop (`MailboxContext`) joins it and registers the group.
   *  ADR-0045 S7: an admin's approve publishes a proposal and the Welcome
   *  exists only once a committer sweeps it, so this wait spans both stages
   *  and keeps running until the group lands or the page unmounts. */
  function startWait(groupIdB64: string): void {
    // The page was left while the request was on its way. `onCleanup` has
    // already run, so a timer armed now would never be cleared: it would tick
    // until the group landed and then open the chat over whatever the person
    // is doing. The request stands, and the mailbox loop joins the group when
    // the Welcome arrives; the chat appears in the list.
    if (unmounted) return;
    stopWait?.();
    stopWait = waitForGroup({
      landed: () => chats.get(groupIdB64) !== undefined,
      onLanded: () => {
        setPhase("joined");
        const groupId = base64Decode(groupIdB64);
        // ADR-0060 section 6: the Welcome landed, so the single-use reply key
        // registered for this request goes.
        void reachSession()?.closeRepliesTo(journalHex(groupId));
        navigate(paths.chat(base64UrlEncodeNoPad(groupId)));
      },
      timers: {
        setInterval: (fn, ms) => window.setInterval(fn, ms),
        clearInterval: (id) => window.clearInterval(id),
      },
    });
  }

  // Opened again for a group this device already asked to join: the waiting
  // line (or, after a day, `Ask again`) stands in for `Request to join`, and
  // the page still opens the chat the moment the join lands.
  createEffect(() => {
    const d = ticket();
    if (d === null || stopWait !== undefined) return;
    if (sent() !== undefined) startWait(d.groupIdB64);
  });

  async function onRequest(ticket: DecodedTicket): Promise<void> {
    if (requesting()) return;
    setError(null);
    setRequesting(true);
    setPhase("requesting");
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
      const groupIdB64 = base64Encode(groupId);
      // Section 6.2 — the request shows under `Requests sent` until the group
      // appears, and the day before `Ask again` starts now. What an earlier
      // request from search knew of the group (its `@name`, its avatar) stays.
      const earlier = sentRequestFor(groupIdB64);
      const sentAtMs = Date.now();
      recordRequestSent({
        idB64: groupIdB64,
        name: earlier?.name !== undefined && earlier.name.length > 0 ? earlier.name : (ticket.preview?.groupName ?? ""),
        handle: earlier?.handle ?? "",
        avatar: earlier?.avatar ?? null,
        sentAtMs,
        knockKeyB64: ticket.knockKeyB64,
        knockVersion: ticket.knockVersion,
        capabilityWireB64: base64Encode(ticket.wire),
        blobIdB64: base64Encode(ticket.blobId),
      });
      setNow(sentAtMs);
      setPhase("waiting");
      startWait(groupIdB64);
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("group-join: request failed", err);
      setError(joinFailureNotice(err));
      setPhase("error");
    } finally {
      setRequesting(false);
    }
  }

  /** `Ask again`, a day or more after the last request: the same request the
   *  `Requests sent` list's `Ask again` sends (`chat/ask-again.ts`), and the
   *  day starts again. A failure keeps the button. */
  async function onAskAgain(): Promise<void> {
    const r = sent();
    if (r === undefined || requesting()) return;
    setAskError(null);
    setRequesting(true);
    try {
      await sendRequestAgain(client, auth.identitySecrets(), r, Date.now(), auth.identitySecrets()?.accountId);
      const sentAtMs = Date.now();
      recordRequestSent({ ...r, sentAtMs });
      setNow(sentAtMs);
      startWait(r.idB64);
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("group-join: ask again failed", err);
      setAskError(joinFailureNotice(err));
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
        <h1>{t("group.join.title")}</h1>
        <p>{t("invite.link.loading")}</p>
      </Show>
      <Show when={!decoded.loading && (decoded() === null || (ticket() !== null && !linkWorks()))}>
        <h1>{t("group.join.title")}</h1>
        <p>{t("invite.link.invalid")}</p>
        <A href={paths.home}>{t("common.open_lakko")}</A>
      </Show>
      <Show when={!decoded.loading && decoded() === "damaged"}>
        <h1>{t("group.join.title")}</h1>
        <p>{INVITE_LINK_DAMAGED()}</p>
        <A href={paths.home}>{t("common.open_lakko")}</A>
      </Show>
      <Show when={!decoded.loading && decoded() === "rate-limited"}>
        <h1>{t("group.join.title")}</h1>
        <p>{INVITE_LINK_RATE_LIMITED()}</p>
        <button type="button" class="primary" onClick={() => void refetch()}>
          {INVITE_LINK_TRY_AGAIN()}
        </button>
        <A href={paths.home}>{t("common.open_lakko")}</A>
      </Show>
      <Show when={!decoded.loading && decoded() === "open-failed"}>
        <h1>{t("group.join.title")}</h1>
        <p>{INVITE_LINK_OPEN_FAILED()}</p>
        <button type="button" class="primary" onClick={() => void refetch()}>
          {INVITE_LINK_TRY_AGAIN()}
        </button>
        <A href={paths.home}>{t("common.open_lakko")}</A>
      </Show>
      <Show when={!decoded.loading && linkWorks()}>
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
              <p>{t("group.join.needs_account")}</p>
              <div class="lk-actions">
                <A href={authLinks().signup}>
                  <button class="primary" type="button">
                    {t("invite.sign_up")}
                  </button>
                </A>
                <A href={authLinks().login}>
                  <button type="button">{t("auth.login.submit")}</button>
                </A>
              </div>
              <Show when={invitedByLine(ticket()?.preview ?? null)}>
                {(line) => <p class="lk-meta">{line()}</p>}
              </Show>
            </>
          }
        >
          <Show when={view() === "request"}>
            <button
              type="button"
              class="primary"
              disabled={requesting()}
              onClick={() => void onRequest(ticket()!)}
            >
              {t("group.join.request")}
            </button>
          </Show>
          <Show when={view() === "sending"}>
            <p>{t("group.join.sending")}</p>
          </Show>
          <Show when={view() === "waiting"}>
            <p>{JOIN_WAIT_PRIMARY()}</p>
          </Show>
          <Show when={view() === "no-answer"}>
            <p>{t("requests_sent.no_answer")}</p>
            <button type="button" class="primary" disabled={requesting()} onClick={() => void onAskAgain()}>
              {t("requests_sent.ask_again")}
            </button>
            <Show when={askError()}>
              {(e) => <p style={{ color: "var(--error)" }}>{e().render()}</p>}
            </Show>
          </Show>
          <Show when={view() === "joined"}>
            <p>{JOIN_SUCCEEDED_OPENING()}</p>
          </Show>
          <Show when={view() === "error"}>
            <p style={{ color: "var(--error)" }}>{error()?.render() ?? JOIN_REQUEST_FAILED()}</p>
            <Show when={error()?.offerRetry !== false}>
              <button
                type="button"
                class="primary"
                disabled={requesting()}
                onClick={() => void onRequest(ticket()!)}
              >
                {JOIN_TRY_AGAIN()}
              </button>
            </Show>
          </Show>
          {/* The one thing on the page the inviter cannot fake stays, at the
              foot and in the muted style, for a person who was told the
              digits out of band. */}
          <Show when={invitedByLine(ticket()?.preview ?? null)}>
            {(line) => <p class="lk-meta">{line()}</p>}
          </Show>
        </Show>
      </Show>
    </PublicLayout>
  );
}

/**
 * `/g/:slug` — a link to a group, or to a channel (ADR-0087 sections 7 and 15:
 * a subscriber invitation, preview kind 3, or a publisher invitation, kind 2).
 * The channel machinery opens the link first; a link it reads as an ordinary
 * group link goes to the group flow above.
 */
export default function JoinGroup() {
  const params = useParams<{ slug: string }>();
  const location = useLocation();
  const source = createMemo(() => ({ slug: params.slug, hash: location.hash }));
  const [channel] = createResource(source, async (s): Promise<OpenedChannelInvite | "group" | "invalid" | "damaged"> => {
    try {
      // The link from the router, never `window.location`: on an in-app
      // navigation (Accept on a publisher card) this runs inside the
      // router's transition, before the address bar changes, so
      // `window.location` still names the chat the person came from.
      return (await openChannelLink(groupInviteRouteLink(s.slug, s.hash))) ?? "group";
    } catch (err) {
      const code = channelErrorCode(err);
      if (code === "link-damaged") return "damaged";
      if (code === "link-invalid") return "invalid";
      // Unreachable or unknown: the group flow below says what it says for
      // the same link, with its own retry.
      return "group";
    }
  });
  const opened = (): OpenedChannelInvite | null => {
    const c = channel();
    return c === undefined || typeof c === "string" ? null : c;
  };
  return (
    <Show
      when={!channel.loading}
      fallback={
        <PublicLayout>
          <h1>{t("group.join.title")}</h1>
          <p>{t("invite.link.loading")}</p>
        </PublicLayout>
      }
    >
      <Show
        when={opened()}
        fallback={
          <Show
            when={channel() === "invalid" || channel() === "damaged"}
            fallback={<JoinGroupFlow />}
          >
            <PublicLayout>
              <p>{channel() === "damaged" ? INVITE_LINK_DAMAGED() : t("invite.link.invalid")}</p>
              <A href={paths.home}>{t("common.open_lakko")}</A>
            </PublicLayout>
          </Show>
        }
      >
        {(o) => (
          <PublicLayout>
            <ChannelLinkPage slug={params.slug} opened={o()} />
          </PublicLayout>
        )}
      </Show>
    </Show>
  );
}
