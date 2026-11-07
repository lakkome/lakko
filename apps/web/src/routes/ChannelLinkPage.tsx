// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The page a channel's `/g/` link opens (UX
 * `docs/ux/group-and-channel-privacy.md` sections 7.2 and 15.2; ADR-0087
 * sections 7 and 14): the channel's name, what it is, its description, and
 * `Subscribe`. A publisher invitation (the co-publisher link) shows `Accept`
 * instead. Rendered by `JoinGroup.tsx` when the link opens to a channel.
 */
import { publisherInviteLine } from "../chat/ChannelInviteCard";
import { createSignal, Match, Show, Switch, type JSX } from "solid-js";
import { A, useLocation, useNavigate } from "@solidjs/router";

import { useAuth } from "../auth/AuthContext";
import { useChats } from "../chat/ChatContext";
import { channelRoleOf, subscribeToChannel } from "../channels/channel-access";
import { channelErrorText } from "../channels/channel-error-copy";
import { channelHeaderLine } from "../channels/channel-facts";
import type { OpenedChannelInvite } from "../channels/channel-invites";
import { acceptPublisherInvite } from "../channels/channel-publishers";
import { t } from "../l10n";
import { Avatar } from "../ui/Avatar";
import { screenAnchor } from "../ui/screen-anchor";
import { joinGroupAuthLinks } from "./join-group-auth";
import { paths } from "./paths";

type Phase = "idle" | "busy" | "waiting" | "error";

export function ChannelLinkPage(props: { readonly slug: string; readonly opened: OpenedChannelInvite }): JSX.Element {
  const auth = useAuth();
  const chats = useChats();
  const navigate = useNavigate();
  const location = useLocation();
  const [phase, setPhase] = createSignal<Phase>("idle");
  const [error, setError] = createSignal<string | null>(null);
  const signedIn = (): boolean => auth.state().kind === "session" && auth.identitySecrets() !== null;
  const already = (): boolean => {
    const r = channelRoleOf(props.opened.channelIdHex);
    return props.opened.publisherInvite ? r === "owner" || r === "publisher" : r !== null;
  };
  const authLinks = (): ReturnType<typeof joinGroupAuthLinks> => joinGroupAuthLinks(props.slug, location.hash);

  async function subscribe(): Promise<void> {
    if (phase() === "busy") return;
    const here = screenAnchor(location);
    setPhase("busy");
    setError(null);
    try {
      const outcome = await subscribeToChannel(props.opened.channelIdHex, props.opened.access, {
        name: props.opened.name,
        description: props.opened.description,
        avatar: props.opened.avatar,
      });
      if (outcome === "waiting") {
        setPhase("waiting");
        return;
      }
      if (here()) navigate(paths.channel(props.opened.channelIdHex), { replace: true });
    } catch (err) {
      setError(channelErrorText(err, () => t("channels.subscribe.error.generic")));
      setPhase("error");
    }
  }

  async function accept(): Promise<void> {
    const identity = chats.conversationAccountIdentity();
    if (identity === null || phase() === "busy") return;
    const here = screenAnchor(location);
    setPhase("busy");
    setError(null);
    try {
      await acceptPublisherInvite(identity, props.opened);
      if (here()) navigate(paths.channel(props.opened.channelIdHex), { replace: true });
    } catch (err) {
      setError(channelErrorText(err, () => t("channels.subscribe.error.generic")));
      setPhase("error");
    }
  }

  return (
    <div class="lk-stack lk-conversation-card" style={{ "align-items": "center" }}>
      {/* A photo is downloaded only by a signed-in device (UX 4.5.4): the
          page shows the initials. */}
      <Avatar label={props.opened.name} id={props.opened.channelIdHex} size="profile" media={null} />
      <h1 style={{ margin: 0 }}>{props.opened.name}</h1>
      <p class="lk-card-desc">{channelHeaderLine(props.opened.visibility, null)}</p>
      <Show when={props.opened.description.length > 0}>
        <p style={{ "white-space": "pre-wrap", "text-align": "center" }}>{props.opened.description}</p>
      </Show>
      <Show when={props.opened.publisherInvite}>
        <p>{publisherInviteLine(props.opened)}</p>
      </Show>
      <Show
        when={signedIn()}
        fallback={
          <>
            <p>{t("channels.link_page.needs_account")}</p>
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
          </>
        }
      >
        <Switch>
          <Match when={already()}>
            <button
              type="button"
              class="primary"
              onClick={() => navigate(paths.channel(props.opened.channelIdHex), { replace: true })}
            >
              {t("search.conversation.open")}
            </button>
          </Match>
          <Match when={phase() === "waiting"}>
            <p role="status">{t("channels.link_page.waiting")}</p>
            <button
              type="button"
              class="primary"
              onClick={() => navigate(paths.channel(props.opened.channelIdHex), { replace: true })}
            >
              {t("search.conversation.open")}
            </button>
          </Match>
          <Match when={props.opened.publisherInvite}>
            <button type="button" class="primary" disabled={phase() === "busy"} onClick={() => void accept()}>
              {phase() === "busy" ? t("channels.join.waiting") : t("channels.publishers.accept")}
            </button>
          </Match>
          <Match when={true}>
            <button type="button" class="primary" disabled={phase() === "busy"} onClick={() => void subscribe()}>
              {phase() === "busy" ? t("channels.subscribe.status.subscribing") : t("channels.subscribe.action")}
            </button>
          </Match>
        </Switch>
        <Show when={error()}>
          {(m) => (
            <p class="form-error" role="alert">
              {m()}
            </p>
          )}
        </Show>
      </Show>
    </div>
  );
}
