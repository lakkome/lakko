// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * A channel invitation in a one-to-one (UX
 * `docs/ux/group-and-channel-privacy.md` sections 14.1 and 15.2; ADR-0087
 * section 14): the message is the channel's `/g/` link (the envelope may also
 * carry the sealed card), and the bubble draws it as a card — the channel's
 * name, what it is, and `Subscribe`, which becomes `Open` once subscribed. A
 * publisher invitation reads "... invites you to be a publisher" with
 * `Accept`, which opens the link page.
 *
 * The link is opened on this device the first time the bubble is drawn; a
 * link that is not a channel's (a group invitation) leaves the bubble as text.
 */
import { createResource, createSignal, Show, type JSX } from "solid-js";
import { A, useNavigate } from "@solidjs/router";

import { channelRoleOf, subscribeToChannel } from "../channels/channel-access";
import { channelErrorText } from "../channels/channel-error-copy";
import { channelHeaderLine } from "../channels/channel-facts";
import { type OpenedChannelInvite } from "../channels/channel-invites";
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
  readonly link: { readonly href: string; readonly url: string };
  /** Called with `true` once the link proves to be a channel's. */
  readonly onResolved: (isChannel: boolean) => void;
}): JSX.Element {
  const navigate = useNavigate();
  const [invite] = createResource(
    () => props.link.url,
    async (url) => {
      const o = await openOnce(url);
      props.onResolved(o !== null);
      return o;
    },
  );
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
              when={channelRoleOf(o().channelIdHex) === null || (o().publisherInvite && channelRoleOf(o().channelIdHex) === "subscriber")}
              fallback={
                <A class="lk-button-link primary" href={paths.channel(o().channelIdHex)}>
                  {t("search.conversation.open")}
                </A>
              }
            >
              <Show
                when={!o().publisherInvite}
                fallback={
                  <A class="lk-button-link primary" href={props.link.href}>
                    {t("channels.publishers.accept")}
                  </A>
                }
              >
                <button type="button" class="primary small" disabled={busy()} onClick={() => void subscribe(o())}>
                  {busy() ? t("channels.subscribe.status.subscribing") : t("channels.subscribe.action")}
                </button>
              </Show>
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
