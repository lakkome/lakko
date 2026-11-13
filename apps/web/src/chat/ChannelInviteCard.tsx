// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * A channel invitation in a one-to-one (UX
 * `docs/ux/group-and-channel-privacy.md` sections 14.1 and 15.2; ADR-0087
 * section 14): the message is the channel's `/g/` link (the envelope may also
 * carry the sealed card), and the bubble draws it as a card — the channel's
 * name, what it is, and `Subscribe`, which becomes `Open` once subscribed. A
 * publisher invitation reads "... invites you to be a publisher" with
 * `Accept`, which sends the request from the card itself (UX 15.2: no
 * further step); a sent request waits on the card until the owner's device
 * approves it.
 *
 * A message that carries the card is drawn from it, opened on this device
 * with no network (`channel-invite-preview.ts` `openStoredCard`), so it
 * stays an invitation after its link is used up. A message with the link
 * alone opens it through the ledger the first time the bubble is drawn; a
 * link that is not a channel's (a group invitation) leaves the bubble as text.
 */
import { createResource, createSignal, Match, onCleanup, Show, Switch, type JSX } from "solid-js";
import { A, useNavigate } from "@solidjs/router";

import { useAuth } from "../auth/AuthContext";
import { channelRoleOf, subscribeToChannel } from "../channels/channel-access";
import { acceptPublisherInvite } from "../channels/channel-publishers";
import {
  loadPendingPublisherRequests,
  markPendingPublisherLinkDead,
  pendingPublisherRequestsVersion,
} from "../channels/pending-publisher-requests";
import { useChats } from "./ChatContext";
import {
  askAgainFailure,
  publisherCardControl,
  type AskAgainOutcome,
  type PendingRequestView,
  type PublisherCardControl,
} from "./publisher-invite-card";
import { channelErrorCode, channelErrorText } from "../channels/channel-error-copy";
import { channelHeaderLine } from "../channels/channel-facts";
import { openChannelLink, type OpenedChannelInvite } from "../channels/channel-invites";
import { cardTitle, openOnce } from "./channel-invite-preview";
import { t } from "../l10n";
import { paths } from "../routes/paths";
import { Avatar } from "../ui/Avatar";
import { channelAvatarKey } from "../channels/channel-avatar";

export { groupLinkOf } from "./group-link";
export { channelInviteRowPreview } from "./channel-invite-preview";

/** UX 15.2 — "{name} invites you to be a publisher of {channel}." when the
 *  link names its owner, else the sentence without a name. */
export function publisherInviteLine(o: Pick<OpenedChannelInvite, "name" | "inviterName">): string {
  return o.inviterName === undefined
    ? t("channels.publishers.invite_page", { channel: o.name })
    : t("channels.publishers.invite_page_from", { name: o.inviterName, channel: o.name });
}

export function ChannelInviteCard(props: {
  /** The message's link, opened through the ledger when there is no card. */
  readonly link?: { readonly href: string; readonly url: string };
  /** The invitation the message's own card opened to, on this device. */
  readonly opened?: OpenedChannelInvite;
  /** Called with `true` once the link proves to be a channel's. */
  readonly onResolved?: (isChannel: boolean) => void;
}): JSX.Element {
  const navigate = useNavigate();
  const auth = useAuth();
  const chats = useChats();
  const [fromLink] = createResource(
    () => (props.opened === undefined ? props.link?.url : undefined),
    async (url) => {
      const o = await openOnce(url);
      props.onResolved?.(o !== null);
      return o;
    },
  );
  const invite = (): OpenedChannelInvite | null | undefined => props.opened ?? fromLink();
  /** The link Accept and Ask again go through: the card's own, else the
   *  message's. */
  const linkUrl = (): string | undefined => props.opened?.link ?? props.link?.url;
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);

  async function subscribe(o: OpenedChannelInvite): Promise<void> {
    setBusy(true);
    setError(null);
    try {
      await subscribeToChannel(o.channelIdHex, o.access, {
        name: o.name,
        description: o.description,
        avatar: o.avatar,
      });
      navigate(paths.channel(o.channelIdHex));
    } catch (err) {
      setError(channelErrorText(err, () => t("channels.subscribe.error.generic")));
    } finally {
      setBusy(false);
    }
  }

  const accountIdB64 = (): string | null => {
    const st = auth.state();
    return st.kind === "session" ? st.account.accountId : null;
  };
  /** The recorded publisher request for this channel, if any. Reactive. */
  const pendingFor = (channelIdHex: string): PendingRequestView | null => {
    pendingPublisherRequestsVersion();
    const account = accountIdB64();
    if (account === null) return null;
    return loadPendingPublisherRequests(account).find((r) => r.channelIdHex === channelIdHex) ?? null;
  };
  // The day is crossed while the card is on screen too.
  const [now, setNow] = createSignal(Date.now());
  const clock = setInterval(() => setNow(Date.now()), 60_000);
  onCleanup(() => clearInterval(clock));
  const control = (o: OpenedChannelInvite): PublisherCardControl =>
    publisherCardControl({
      role: channelRoleOf(o.channelIdHex),
      busy: busy(),
      pending: pendingFor(o.channelIdHex),
      nowMs: now(),
    });

  /** UX 15.2.2 — `Ask again`: the same link, fetched again; when it still
   *  works, the request goes out once more exactly as the first `Accept`
   *  (the record is replaced and the day starts again). A dead link keeps
   *  the earlier record (a late approval still lands) and the card keeps the
   *  invalid-link line; no network keeps `Ask again`. */
  async function askAgain(): Promise<void> {
    const identity = chats.conversationAccountIdentity();
    const account = accountIdB64();
    const url = linkUrl();
    if (identity === null || account === null || url === undefined || busy()) return;
    setBusy(true);
    setError(null);
    let outcome: AskAgainOutcome;
    try {
      const fresh = await openChannelLink(url);
      if (fresh === null || !fresh.publisherInvite) {
        outcome = { kind: "dead" };
      } else {
        const sent = await acceptPublisherInvite(identity, fresh, account);
        outcome = { kind: sent === "joined" ? "joined" : "sent" };
        if (sent === "joined") navigate(paths.channel(fresh.channelIdHex));
      }
    } catch (err) {
      outcome = askAgainFailure(channelErrorCode(err));
    } finally {
      setBusy(false);
    }
    const o = invite();
    if (outcome.kind === "dead" && o) markPendingPublisherLinkDead(account, o.channelIdHex);
    if (outcome.kind === "offline") setError(t("common.error.offline"));
    if (outcome.kind === "failed") setError(t("channels.subscribe.error.generic"));
  }

  /** UX 15.2 — `Accept` sends the request here: the owner named this person,
   *  so the owner's device approves it with no further step. */
  async function accept(o: OpenedChannelInvite): Promise<void> {
    const identity = chats.conversationAccountIdentity();
    const account = accountIdB64();
    if (identity === null || account === null || busy()) return;
    setBusy(true);
    setError(null);
    try {
      const outcome = await acceptPublisherInvite(identity, o, account);
      if (outcome === "joined") navigate(paths.channel(o.channelIdHex));
      // "waiting": the request is recorded; the card shows the waiting line
      // and the mailbox loop lands it when the owner's device approves.
    } catch (err) {
      setError(channelErrorText(err, () => t("channels.subscribe.error.generic")));
    } finally {
      setBusy(false);
    }
  }

  return (
    <Show when={invite()}>
      {(o) => (
        <div class="lk-bubble-channel-invite">
          {/* The channel's own avatar treatment (its photo when this device
              holds it, else its initials from the best name known), as on
              its Chats row: never a "?" from an empty preview name. */}
          <Avatar
            label={cardTitle(o())}
            id={o().channelIdHex}
            size="md"
            conversationKey={channelAvatarKey(o().channelIdHex)}
          />
          <div class="lk-bubble-channel-invite-body">
            <span class="lk-bubble-channel-invite-title">{cardTitle(o())}</span>
            <span class="lk-meta">
              {o().publisherInvite
                ? publisherInviteLine(o())
                : channelHeaderLine(o().visibility, null)}
            </span>
            <Show
              when={o().publisherInvite}
              fallback={
                <Show
                  when={channelRoleOf(o().channelIdHex) === null}
                  fallback={
                    <A class="lk-button-link primary" href={paths.channel(o().channelIdHex)}>
                      {t("search.conversation.open")}
                    </A>
                  }
                >
                  <button type="button" class="primary small" disabled={busy()} onClick={() => void subscribe(o())}>
                    {busy() ? t("channels.subscribe.status.subscribing") : t("channels.subscribe.action")}
                  </button>
                </Show>
              }
            >
              <Switch>
                <Match when={control(o()) === "open"}>
                  <A class="lk-button-link primary" href={paths.channel(o().channelIdHex)}>
                    {t("search.conversation.open")}
                  </A>
                </Match>
                <Match when={control(o()) === "busy"}>
                  <button type="button" class="primary small" disabled>
                    {t("channels.join.waiting")}
                  </button>
                </Match>
                <Match when={control(o()) === "waiting"}>
                  {/* Sent and recorded: it lands by itself when the owner's
                      device approves. A request is not withdrawn. */}
                  <span class="lk-meta" role="status">
                    {t("channels.join.waiting")}
                  </span>
                </Match>
                <Match when={control(o()) === "no-answer"}>
                  {/* Never a refusal: "No answer yet." is true either way. */}
                  <span class="lk-meta" role="status">
                    {t("requests_sent.no_answer")}
                  </span>
                  <button type="button" class="primary small" onClick={() => void askAgain()}>
                    {t("requests_sent.ask_again")}
                  </button>
                </Match>
                <Match when={control(o()) === "dead"}>
                  <span class="lk-meta" role="status">
                    {t("invite.link.invalid")}
                  </span>
                </Match>
                <Match when={true}>
                  <button type="button" class="primary small" onClick={() => void accept(o())}>
                    {t("channels.publishers.accept")}
                  </button>
                </Match>
              </Switch>
            </Show>
            <Show when={error()}>
              {(m) => (
                <span class="form-error" role="alert">
                  {m()}
                </span>
              )}
            </Show>
          </div>
        </div>
      )}
    </Show>
  );
}
