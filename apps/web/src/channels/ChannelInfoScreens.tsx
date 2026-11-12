// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The screens a channel's info pushes (UX
 * `docs/ux/group-and-channel-privacy.md`): Channel type (4.1), Subscribers
 * (15.1, 7.3, 13.3's Banned), Publishers (15.2), Invite people (14.1) and
 * Invite link (14.2). Each draws what `channel-access.ts`,
 * `channel-invites.ts` and `channel-publishers.ts` hold; none of them holds a
 * key or talks to a server itself.
 */
import { createEffect, createResource, createSignal, For, onMount, Show, type JSX } from "solid-js";

import { base64Encode } from "../auth/account-id";
import { useChats } from "../chat/ChatContext";
import { copyMessageText } from "../chat/copy-message";
import { handleRandom, handleWellFormed } from "../chat/group-handle";
import { loadAccepted, subscribeAcceptedChanged, type AcceptedContact } from "../contacts/local-store";
import { appLanguage, languageLabel, languageOptions } from "../discover/languages";
import { topicLabel } from "../discover/topic-names";
import { HANDLE_CHECK_DEBOUNCE_MS, handleTaken as askHandleTaken } from "../discovery/handle-check";
import { memberNameOf } from "../profile/member-label";
import { useProfiles } from "../profile/ProfileContext";
import { channelsState } from "../state/channels-state";
import { currentLanguage, joinList, t } from "../l10n";
import { Avatar } from "../ui/Avatar";
import { requestConfirm } from "../ui/confirm-dialog";
import { Icon } from "../ui/icons";
import { LakkoPicker } from "../ui/LakkoPicker";
import { loadWasm } from "../wasm";
import { BannedList } from "./BannedList";
import {
  channelHandleOf,
  channelRoleOf,
  channelSeedFor,
  channelSubscriberPage,
  channelTopicOf,
  channelTypeProgress,
  channelVisibilityOf,
  removeChannelSubscriber,
  saveChannelType,
  type ChannelSubscriberRow,
  type ChannelVisibility,
} from "./channel-access";
import { channelErrorText } from "./channel-error-copy";
import { channelFacts } from "./channel-facts";
import {
  channelInviteLinks,
  mintChannelInviteCard,
  mintChannelInviteLink,
  revokeChannelInviteLink,
  type ChannelInviteLink,
} from "./channel-invites";
import {
  channelPublishers,
  mintPublisherInviteLink,
  decidePublisherRequest,
  pendingPublisherRequests,
  refreshChannelPublishers,
  removePublisher,
  stepDownAsPublisher,
} from "./channel-publishers";
import { bytesToHex } from "./composer";
import { useChannelInviteSender } from "./invite-contacts";
import { loadChannelMembers, type ChannelMembersResult } from "./member-cards";

// ---------------------------------------------------------------------------
// Channel type (UX 4.1)
// ---------------------------------------------------------------------------

export function ChannelTypeScreen(props: { readonly id: string; readonly onDone: () => void }): JSX.Element {
  const chats = useChats();
  const [wasm, setWasm] = createSignal<Awaited<ReturnType<typeof loadWasm>> | null>(null);
  void loadWasm().then(setWasm);
  const held = channelHandleOf(props.id);
  const heldVisibility = channelVisibilityOf(props.id) ?? "private";
  const heldTopic = channelTopicOf(props.id);
  const [visibility, setVisibility] = createSignal<ChannelVisibility>(heldVisibility);
  const [handle, setHandle] = createSignal(held ?? "");
  const [topic, setTopic] = createSignal(heldTopic?.topic ?? 0);
  const [language, setLanguage] = createSignal(
    heldTopic?.language && heldTopic.language.length > 0 ? heldTopic.language : appLanguage(currentLanguage()),
  );
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const isPublic = (): boolean => visibility() === "public";

  // A lapsed name (UX 4.4): the screen opens on a proposed one.
  createEffect(() => {
    const w = wasm();
    if (w === null || handle().length > 0) return;
    try {
      setHandle(w.suggestHandle(channelFacts(props.id).name, "channel", !isPublic(), handleRandom()));
    } catch {
      /* the field stays empty; the person types one */
    }
  });
  const handleWell = (): boolean => {
    const w = wasm();
    return w === null || handleWellFormed(w, handle());
  };

  const [taken, setTaken] = createSignal(false);
  let timer: ReturnType<typeof setTimeout> | null = null;
  createEffect(() => {
    const h = handle();
    setTaken(false);
    if (timer !== null) clearTimeout(timer);
    if (!isPublic() || h === held || h.length < 3 || !handleWell()) return;
    timer = setTimeout(() => {
      void askHandleTaken(h).then((v) => {
        if (handle() === h && v !== null) setTaken(v);
      });
    }, HANDLE_CHECK_DEBOUNCE_MS);
  });

  const changed = (): boolean =>
    visibility() !== heldVisibility ||
    handle() !== (held ?? "") ||
    (isPublic() && (topic() !== (heldTopic?.topic ?? 0) || language() !== (heldTopic?.language ?? "")));

  async function save(): Promise<void> {
    if (busy() || !handleWell() || handle().length < 3) return;
    const flipping = visibility() !== heldVisibility;
    if (flipping) {
      const ok = await requestConfirm(
        isPublic()
          ? {
              title: t("channel_type.confirm.public.title"),
              body: t("channel_type.confirm.public.body"),
              confirmLabel: t("group_type.confirm.public.confirm"),
              cancelLabel: t("common.cancel"),
              tone: "neutral",
            }
          : {
              title: t("channel_type.confirm.private.title"),
              body: t("channel_type.confirm.private.body"),
              confirmLabel: t("group_type.confirm.private.confirm"),
              cancelLabel: t("common.cancel"),
              tone: "neutral",
            },
      );
      if (!ok) return;
    }
    if (held !== null && handle() !== held) {
      const ok = await requestConfirm({
        title: t("group_type.confirm.rename.title"),
        body: t("channel_type.confirm.rename.body"),
        confirmLabel: t("group_type.confirm.rename.confirm"),
        cancelLabel: t("common.cancel"),
        tone: "neutral",
      });
      if (!ok) return;
    }
    const identity = chats.conversationAccountIdentity();
    const current = chats.accountIdentity();
    if (identity === null || current === null) {
      setError(t("channels.error.sign_in"));
      return;
    }
    setBusy(true);
    setError(null);
    try {
      await saveChannelType(identity, current, props.id, {
        visibility: visibility(),
        handle: handle(),
        topic: topic(),
        language: language(),
      });
      props.onDone();
    } catch (err) {
      setError(channelErrorText(err, () => t("group_handle.error.failed")));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div class="lk-stack" style={{ padding: "16px" }}>
      <section class="lk-card">
        <label id="lk-channel-type-who">{t("channel_type.who_can_subscribe")}</label>
        <LakkoPicker
          value={visibility()}
          labelledBy="lk-channel-type-who"
          options={[
            { value: "private", label: t("group_type.private") },
            { value: "public", label: t("group_type.public") },
          ]}
          disabled={busy()}
          onChange={(v) => setVisibility(v === "public" ? "public" : "private")}
        />
        <p class="lk-card-desc" style={{ margin: 0 }}>
          {isPublic() ? t("channel_type.public_desc") : t("channel_type.private_desc")}
        </p>
        <label>
          {t("group_type.handle")}
          <input
            type="text"
            dir="ltr"
            value={`@${handle()}`}
            maxLength={33}
            autocomplete="off"
            autocapitalize="none"
            spellcheck={false}
            disabled={busy()}
            aria-invalid={!handleWell()}
            onInput={(e) => setHandle(e.currentTarget.value.replace(/^@/, "").trim())}
          />
        </label>
        <Show
          when={handleWell()}
          fallback={<p class="form-error" style={{ margin: 0 }}>{t("group_handle.error.bad_form")}</p>}
        >
          <Show
            when={taken()}
            fallback={
              <p class="lk-card-desc" style={{ margin: 0 }}>
                {isPublic() ? t("channel_type.handle_public") : t("channel_type.handle_private")}
              </p>
            }
          >
            <p class="form-error" style={{ margin: 0 }}>{t("group_handle.error.taken")}</p>
          </Show>
        </Show>
        <Show when={isPublic()}>
          <label id="lk-channel-type-topic">{t("discover.listing.topic")}</label>
          <LakkoPicker
            value={String(topic())}
            labelledBy="lk-channel-type-topic"
            options={((): number[] => {
              const w = wasm();
              try {
                return w === null ? [0] : Array.from(w.channelTopicIds());
              } catch {
                return [0];
              }
            })().map((id) => ({ value: String(id), label: topicLabel(id) }))}
            disabled={busy()}
            onChange={(v) => setTopic(Number.parseInt(v, 10) || 0)}
          />
          <label id="lk-channel-type-language">{t("discover.language")}</label>
          <LakkoPicker
            value={language()}
            labelledBy="lk-channel-type-language"
            options={languageOptions(language()).map((code) => ({ value: code, label: languageLabel(code) }))}
            disabled={busy()}
            onChange={(v) => setLanguage(v)}
          />
        </Show>
        <Show when={channelTypeProgress(props.id)}>
          {(p) => (
            <p class="form-info" role="status">
              {t("channel_type.updating", { done: p().done, total: p().total })}
            </p>
          )}
        </Show>
        <Show when={error()}>
          {(m) => (
            <p class="form-error" role="alert">
              {m()}
            </p>
          )}
        </Show>
        <div class="lk-actions">
          <button type="button" class="primary" disabled={busy() || !changed() || !handleWell()} onClick={() => void save()}>
            {busy() ? t("common.saving") : t("common.save")}
          </button>
        </div>
      </section>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Subscribers (UX 15.1, 7.3, 13.3)
// ---------------------------------------------------------------------------

export function ChannelSubscribersScreen(props: { readonly id: string; readonly count: number | null }): JSX.Element {
  const chats = useChats();
  const role = (): ReturnType<typeof channelRoleOf> => channelRoleOf(props.id);
  const publishes = (): boolean => role() === "owner" || role() === "publisher";
  const isPrivate = (): boolean => channelVisibilityOf(props.id) === "private";

  const [members, { refetch }] = createResource(
    () => props.id,
    (id): Promise<ChannelMembersResult> => {
      const keys = channelsState.subscriberKeysByChannel().get(id);
      return loadChannelMembers({
        channelIdHex: id,
        seedForEpoch: (e) => channelSeedFor(id, e),
        ownSubscriberPkHex: keys === undefined ? null : bytesToHex(keys.public),
      });
    },
  );

  // Private channel, owner and publishers: every subscriber by the link they
  // came through, a page at a time, each with Remove.
  const [rows, setRows] = createSignal<readonly ChannelSubscriberRow[]>([]);
  const [after, setAfter] = createSignal(0);
  const [done, setDone] = createSignal(false);
  const [pageError, setPageError] = createSignal(false);
  const [removing, setRemoving] = createSignal<string | null>(null);
  const [removeError, setRemoveError] = createSignal<string | null>(null);

  async function loadPage(): Promise<void> {
    const identity = chats.conversationAccountIdentity();
    if (identity === null || done()) return;
    setPageError(false);
    try {
      const page = await channelSubscriberPage(identity, props.id, after());
      setRows([...rows(), ...page.rows]);
      setAfter(page.nextAfter);
      setDone(page.done);
    } catch {
      setPageError(true);
    }
  }
  onMount(() => {
    if (publishes() && isPrivate()) void loadPage();
  });

  async function remove(pkHex: string): Promise<void> {
    const ok = await requestConfirm({
      title: t("channels.subscribers.remove_title"),
      body: t("channels.subscribers.remove_body"),
      confirmLabel: t("common.remove"),
      cancelLabel: t("common.cancel"),
      tone: "danger",
    });
    if (!ok) return;
    const identity = chats.conversationAccountIdentity();
    if (identity === null) return;
    setRemoving(pkHex);
    setRemoveError(null);
    try {
      await removeChannelSubscriber(identity, props.id, pkHex);
      setRows(rows().filter((r) => r.pkHex !== pkHex));
    } catch (err) {
      setRemoveError(channelErrorText(err, () => t("group_handle.error.failed")));
    } finally {
      setRemoving(null);
    }
  }

  const viaLine = (r: ChannelSubscriberRow): string => {
    if (r.via === null) return "";
    return r.via.kind === "link" ? t("channels.subscribers.from_link", { label: r.via.label }) : t("channels.subscribers.from_invitation");
  };

  const others = (): number | null => {
    const m = members();
    if (props.count === null || m === undefined) return null;
    return Math.max(0, props.count - m.rows.length);
  };

  return (
    <div class="lk-stack" style={{ padding: "16px" }}>
      <section class="lk-card" aria-label={t("channels.subscribers.title")}>
        <h2>
          <Show when={props.count !== null} fallback={t("channels.subscribers.title")}>
            {t("channels.subscribers.title_count", { count: props.count ?? 0 })}
          </Show>
        </h2>
        <Show when={members.loading}>
          <p class="lk-meta">{t("common.loading")}</p>
        </Show>
        <Show when={members.error !== undefined && !members.loading}>
          <div class="lk-alert danger" role="alert">
            {t("channels.subscribers.load_failed")}
            <button type="button" class="ghost" onClick={() => void refetch()}>
              {t("common.try_again")}
            </button>
          </div>
        </Show>
        <Show when={!members.loading && members.error === undefined ? members() : undefined}>
          {(m) => (
            <>
              <Show when={m().rows.length === 0 && (props.count === null || props.count === 0)}>
                <p class="lk-meta">{t("channels.subscribers.none")}</p>
              </Show>
              <ul class="lk-member-list" style={{ "list-style": "none", margin: 0, padding: 0 }}>
                <For each={m().rows}>
                  {(row): JSX.Element => (
                    <li class="lk-card-row">
                      <span class="lk-card-row-icon">
                        <Avatar label={row.name} id={row.key} size="sm" personKey={row.personKeyB64} />
                      </span>
                      <div class="lk-card-row-body">
                        <span class="lk-card-row-title">{row.isSelf ? t("channels.members.you") : row.name}</span>
                        <Show when={row.username}>{(u) => <span class="lk-card-row-sub" dir="ltr">@{u()}</span>}</Show>
                      </div>
                    </li>
                  )}
                </For>
              </ul>
              <Show when={others() !== null && (others() ?? 0) > 0}>
                <p class="lk-meta">{t("channels.subscribers.others", { count: others() ?? 0 })}</p>
              </Show>
            </>
          )}
        </Show>
      </section>

      <Show when={publishes() && isPrivate()}>
        <section class="lk-card">
          <Show when={removeError()}>
            {(m) => (
              <p class="form-error" role="alert">
                {m()}
              </p>
            )}
          </Show>
          <ul style={{ "list-style": "none", margin: 0, padding: 0 }}>
            <For each={rows()}>
              {(r) => (
                <li class="lk-card-row">
                  <div class="lk-card-row-body">
                    <span class="lk-card-row-title">{viaLine(r) || t("channels.subscribers.title")}</span>
                  </div>
                  <button
                    type="button"
                    class="danger lk-card-row-action"
                    disabled={removing() !== null}
                    onClick={() => void remove(r.pkHex)}
                  >
                    {t("common.remove")}
                  </button>
                </li>
              )}
            </For>
          </ul>
          <Show when={pageError()}>
            <div class="lk-alert danger" role="alert">
              {t("channels.subscribers.load_failed")}
              <button type="button" class="ghost" onClick={() => void loadPage()}>
                {t("common.try_again")}
              </button>
            </div>
          </Show>
          <Show when={!done() && !pageError() && rows().length > 0}>
            <button type="button" class="ghost" onClick={() => void loadPage()}>
              {t("channels.subscribers.more")}
            </button>
          </Show>
        </section>
      </Show>

      <Show when={publishes()}>
        <BannedList channelIdHex={props.id} />
      </Show>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Publishers (UX 15.2)
// ---------------------------------------------------------------------------

export function ChannelPublishersScreen(props: { readonly id: string }): JSX.Element {
  const chats = useChats();
  const profiles = useProfiles();
  const sendInvite = useChannelInviteSender();
  const isOwner = (): boolean => channelRoleOf(props.id) === "owner";
  const [picking, setPicking] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const [sent, setSent] = createSignal<string | null>(null);
  const [steppedDown, setSteppedDown] = createSignal(false);

  onMount(() => {
    const identity = chats.conversationAccountIdentity();
    if (identity !== null) void refreshChannelPublishers(identity, props.id).catch(() => undefined);
  });

  const nameOf = (keyB64: string): string => memberNameOf(profiles.resolve(keyB64), "the channel publishers list");

  async function remove(keyB64: string): Promise<void> {
    const name = nameOf(keyB64);
    const ok = await requestConfirm({
      title: t("channels.publishers.remove"),
      body: t("channels.publishers.remove_body", { name }),
      confirmLabel: t("common.remove"),
      cancelLabel: t("common.cancel"),
      tone: "danger",
    });
    if (!ok) return;
    const identity = chats.conversationAccountIdentity();
    if (identity === null) return;
    setError(null);
    try {
      await removePublisher(identity, props.id, keyB64);
    } catch (err) {
      setError(channelErrorText(err, () => t("channels.publishers.remove_failed")));
    }
  }

  const ownKeyB64 = (): string | null => {
    const identity = chats.conversationAccountIdentity();
    return identity === null ? null : base64Encode(identity.signaturePublicKey);
  };

  async function stepDown(): Promise<void> {
    const ok = await requestConfirm({
      title: t("channels.publishers.step_down"),
      body: t("channels.publishers.step_down_body"),
      confirmLabel: t("channels.publishers.step_down"),
      cancelLabel: t("common.cancel"),
      tone: "danger",
    });
    if (!ok) return;
    const identity = chats.conversationAccountIdentity();
    if (identity === null) return;
    setError(null);
    setSent(null);
    try {
      await stepDownAsPublisher(identity, props.id);
      setSteppedDown(true);
    } catch (err) {
      setError(channelErrorText(err, () => t("channels.publishers.step_down_failed")));
    }
  }

  async function decide(keyB64: string, approve: boolean): Promise<void> {
    const identity = chats.conversationAccountIdentity();
    if (identity === null) return;
    setError(null);
    try {
      await decidePublisherRequest(identity, props.id, keyB64, approve);
    } catch (err) {
      setError(channelErrorText(err, () => t("channels.copublishers.error.approve")));
    }
  }

  async function invite(contact: AcceptedContact): Promise<void> {
    const identity = chats.conversationAccountIdentity();
    if (identity === null) return;
    setError(null);
    setSent(null);
    try {
      const url = await mintPublisherInviteLink(identity, props.id);
      const outcome = await sendInvite(contact, { kind: "link", url });
      // Only an invitation written into their conversation is "sent"; a
      // contact request or nothing at all is not.
      if (outcome !== "invited") throw new Error(`publisher invitation not delivered: ${outcome}`);
      setSent(nameOf(contact.signaturePubkeyB64));
      setPicking(false);
    } catch (err) {
      setError(t("channels.invite.failed", { names: nameOf(contact.signaturePubkeyB64) }));
      console.warn("[channels] publisher invite failed", err instanceof Error ? err.name : "");
    }
  }

  return (
    <div class="lk-stack" style={{ padding: "16px" }}>
      <section class="lk-card">
        <ul style={{ "list-style": "none", margin: 0, padding: 0 }}>
          <For each={channelPublishers(props.id)}>
            {(p) => (
              <li class="lk-card-row">
                <span class="lk-card-row-icon">
                  <Avatar label={nameOf(p.accountKeyB64)} id={p.accountKeyB64} size="sm" personKey={p.accountKeyB64} />
                </span>
                <div class="lk-card-row-body">
                  <span class="lk-card-row-title">{nameOf(p.accountKeyB64)}</span>
                  <span class="lk-card-row-sub">
                    {p.isOwner ? t("channels.publishers.role_owner") : t("channels.publishers.role_publisher")}
                  </span>
                </div>
                <Show when={isOwner() && !p.isOwner}>
                  <button type="button" class="danger lk-card-row-action" onClick={() => void remove(p.accountKeyB64)}>
                    {t("channels.publishers.remove")}
                  </button>
                </Show>
                <Show when={!isOwner() && !p.isOwner && p.accountKeyB64 === ownKeyB64() && !steppedDown()}>
                  <button type="button" class="danger lk-card-row-action" onClick={() => void stepDown()}>
                    {t("channels.publishers.step_down")}
                  </button>
                </Show>
              </li>
            )}
          </For>
        </ul>
        <Show when={isOwner() && pendingPublisherRequests(props.id).length > 0}>
          <h3 class="lk-card-row-title">
            {t("channels.publishers.requests", { count: pendingPublisherRequests(props.id).length })}
          </h3>
          <ul style={{ "list-style": "none", margin: 0, padding: 0 }}>
            <For each={pendingPublisherRequests(props.id)}>
              {(keyB64) => (
                <li class="lk-card-row">
                  <span class="lk-card-row-icon">
                    <Avatar label={nameOf(keyB64)} id={keyB64} size="sm" personKey={keyB64} />
                  </span>
                  <div class="lk-card-row-body">
                    <span class="lk-card-row-title">{nameOf(keyB64)}</span>
                  </div>
                  <button type="button" class="lk-card-row-action" onClick={() => void decide(keyB64, true)}>
                    {t("chat.members.join.approve")}
                  </button>
                  <button type="button" class="secondary lk-card-row-action" onClick={() => void decide(keyB64, false)}>
                    {t("chat.members.join.reject")}
                  </button>
                </li>
              )}
            </For>
          </ul>
        </Show>
        <Show when={isOwner()}>
          <button type="button" class="secondary" onClick={() => setPicking((v) => !v)}>
            <Icon name="person-plus" size={20} />
            {t("channels.publishers.add")}
          </button>
        </Show>
        <Show when={steppedDown()}>
          <p class="form-info" role="status">
            {t("channels.publishers.step_down_sent")}
          </p>
        </Show>
        <Show when={sent()}>
          {(n) => (
            <p class="form-info" role="status">
              {t("call.invite.sent", { name: n() })}
            </p>
          )}
        </Show>
        <Show when={error()}>
          {(m) => (
            <p class="form-error" role="alert">
              {m()}
            </p>
          )}
        </Show>
      </section>
      <Show when={picking()}>
        <ContactPicker
          single
          title={t("channels.publishers.add")}
          onPick={(contacts) => {
            const c = contacts[0];
            if (c !== undefined) void invite(c);
          }}
        />
      </Show>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Invite people (UX 14.1)
// ---------------------------------------------------------------------------

export function ChannelInvitePeopleScreen(props: { readonly id: string; readonly onDone: () => void }): JSX.Element {
  const chats = useChats();
  const profiles = useProfiles();
  const sendInvite = useChannelInviteSender();
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);

  async function invite(contacts: readonly AcceptedContact[]): Promise<void> {
    const identity = chats.conversationAccountIdentity();
    if (identity === null || busy() || contacts.length === 0) return;
    setBusy(true);
    setError(null);
    const failed: string[] = [];
    for (const c of contacts) {
      try {
        const { card, url } = await mintChannelInviteCard(identity, props.id);
        const outcome = await sendInvite(c, { kind: "card", url, card });
        if (outcome !== "invited") failed.push(memberNameOf(profiles.resolve(c.signaturePubkeyB64), "a channel invitation failure"));
      } catch {
        failed.push(memberNameOf(profiles.resolve(c.signaturePubkeyB64), "a channel invitation failure"));
      }
    }
    setBusy(false);
    if (failed.length > 0) {
      setError(t("channels.invite.failed", { names: joinList(failed) }));
      return;
    }
    props.onDone();
  }

  return (
    <div class="lk-stack" style={{ padding: "16px" }}>
      <ContactPicker title={t("channels.info.invite_people")} busy={busy()} onPick={(c) => void invite(c)} />
      <Show when={error()}>
        {(m) => (
          <p class="form-error" role="alert">
            {m()}
          </p>
        )}
      </Show>
    </div>
  );
}

/** The contacts list with check boxes and the trailing `Invite (n)` button;
 *  `single` picks one and acts at once. Contacts only: an invitation is a
 *  message in a one-to-one, and a stranger has none (UX 14.1). Every contact
 *  is offered: a contact can always be messaged, and the invitation founds the
 *  one-to-one when none is held. */
function ContactPicker(props: {
  readonly title: string;
  readonly single?: boolean;
  readonly busy?: boolean;
  readonly onPick: (contacts: readonly AcceptedContact[]) => void;
}): JSX.Element {
  const profiles = useProfiles();
  const [accepted, setAccepted] = createSignal<AcceptedContact[]>(loadAccepted());
  onMount(() => {
    const off = subscribeAcceptedChanged(() => setAccepted(loadAccepted()));
    return off;
  });
  const [picked, setPicked] = createSignal<ReadonlySet<string>>(new Set());
  const labelOf = (c: AcceptedContact): string => memberNameOf(profiles.resolve(c.signaturePubkeyB64), "the channel contact picker");
  const toggle = (key: string): void => {
    const next = new Set(picked());
    if (next.has(key)) next.delete(key);
    else next.add(key);
    setPicked(next);
  };
  return (
    <section class="lk-card" style={{ gap: "4px" }} aria-label={props.title}>
      <Show when={accepted().length > 0} fallback={<p class="lk-meta">{t("call.invite.picker_empty")}</p>}>
        <For each={accepted()}>
          {(c) => (
            <Show
              when={props.single !== true}
              fallback={
                <button type="button" class="lk-row" disabled={props.busy === true} onClick={() => props.onPick([c])}>
                  <Avatar label={labelOf(c)} id={c.signaturePubkeyB64} personKey={c.signaturePubkeyB64} />
                  <span class="lk-row-body">
                    <span class="lk-row-title">{labelOf(c)}</span>
                  </span>
                </button>
              }
            >
              <label class="lk-row pick">
                <input
                  type="checkbox"
                  checked={picked().has(c.signaturePubkeyB64)}
                  disabled={props.busy === true}
                  onChange={() => toggle(c.signaturePubkeyB64)}
                />
                <Avatar label={labelOf(c)} id={c.signaturePubkeyB64} personKey={c.signaturePubkeyB64} />
                <div class="lk-row-body">
                  <span class="lk-row-title">{labelOf(c)}</span>
                </div>
              </label>
            </Show>
          )}
        </For>
        <Show when={props.single !== true}>
          <div class="lk-actions">
            <button
              type="button"
              class="primary"
              disabled={props.busy === true || picked().size === 0}
              onClick={() => props.onPick(accepted().filter((c) => picked().has(c.signaturePubkeyB64)))}
            >
              {t("channels.info.invite")}
              <Show when={picked().size > 0}> {t("channels.subscribers.count", { count: picked().size })}</Show>
            </button>
          </div>
        </Show>
      </Show>
    </section>
  );
}

// ---------------------------------------------------------------------------
// Invite link (UX 14.2)
// ---------------------------------------------------------------------------

const DAY_MS = 24 * 60 * 60 * 1000;

export function ChannelInviteLinksScreen(props: { readonly id: string }): JSX.Element {
  const chats = useChats();
  const [label, setLabel] = createSignal("");
  const [expiry, setExpiry] = createSignal<"never" | "1" | "7" | "30">("never");
  const [uses, setUses] = createSignal<"unlimited" | "one">("unlimited");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const [notice, setNotice] = createSignal<string | null>(null);

  async function create(): Promise<void> {
    const identity = chats.conversationAccountIdentity();
    if (identity === null || busy()) return;
    setBusy(true);
    setError(null);
    setNotice(null);
    try {
      const link = await mintChannelInviteLink(identity, props.id, {
        label: label().trim(),
        expiryMs: expiry() === "never" ? 0 : Date.now() + Number(expiry()) * DAY_MS,
        maxUses: uses() === "one" ? 0 : 4294967295,
      });
      setLabel("");
      const ok = await copyMessageText(link.url);
      setNotice(ok ? t("chat.invite_link.copied") : null);
    } catch (err) {
      setError(channelErrorText(err, () => t("channels.links.failed")));
    } finally {
      setBusy(false);
    }
  }

  async function revoke(link: ChannelInviteLink): Promise<void> {
    const ok = await requestConfirm({
      title: t("invite.links.delete_confirm_title"),
      body: t("invite.links.delete_confirm_body"),
      confirmLabel: t("chat.invite_link.revoke"),
      cancelLabel: t("common.cancel"),
      tone: "danger",
    });
    if (!ok) return;
    const identity = chats.conversationAccountIdentity();
    if (identity === null) return;
    setError(null);
    try {
      await revokeChannelInviteLink(identity, props.id, link.linkIdHex);
      setNotice(t("chat.invite_link.revoked"));
    } catch (err) {
      setError(channelErrorText(err, () => t("channels.links.revoke_failed")));
    }
  }

  const termsOf = (l: ChannelInviteLink): string => {
    const parts: string[] = [];
    if (l.expiryMs !== 0 && l.expiryMs < Date.now()) parts.push(t("invite.links.expired"));
    else if (l.expiryMs === 0) parts.push(t("invite.links.never_expires"));
    if (l.maxUses === 4294967295) parts.push(t("invite.links.unlimited_uses"));
    else {
      const left = Math.max(0, (l.maxUses === 0 ? 1 : l.maxUses) - l.uses);
      parts.push(left === 0 ? t("invite.links.used_up") : t("invite.links.uses_left", { count: left }));
    }
    return parts.join(" · ");
  };

  return (
    <div class="lk-stack" style={{ padding: "16px" }}>
      <section class="lk-card">
        <label>
          {t("invite.links.label_field")}
          <input
            type="text"
            value={label()}
            maxLength={64}
            disabled={busy()}
            onInput={(e) => setLabel(e.currentTarget.value)}
          />
        </label>
        <label id="lk-channel-link-expiry">{t("invite.links.expires_field")}</label>
        <LakkoPicker
          value={expiry()}
          labelledBy="lk-channel-link-expiry"
          options={[
            { value: "never", label: t("invite.links.expires_option_never") },
            { value: "1", label: t("invite.links.expires_option_days", { count: 1 }) },
            { value: "7", label: t("invite.links.expires_option_days", { count: 7 }) },
            { value: "30", label: t("invite.links.expires_option_days", { count: 30 }) },
          ]}
          disabled={busy()}
          onChange={(v) => setExpiry(v === "1" || v === "7" || v === "30" ? v : "never")}
        />
        <label id="lk-channel-link-uses">{t("invite.links.uses_field")}</label>
        <LakkoPicker
          value={uses()}
          labelledBy="lk-channel-link-uses"
          options={[
            { value: "unlimited", label: t("invite.links.uses_option_unlimited") },
            { value: "one", label: t("invite.links.uses_option_one") },
          ]}
          disabled={busy()}
          onChange={(v) => setUses(v === "one" ? "one" : "unlimited")}
        />
        <div class="lk-actions">
          <button type="button" class="primary" disabled={busy()} onClick={() => void create()}>
            <Icon name="link" size={20} />
            {t("chat.invite_link.create")}
          </button>
        </div>
        <Show when={notice()}>
          {(m) => (
            <p class="form-info" role="status">
              {m()}
            </p>
          )}
        </Show>
        <Show when={error()}>
          {(m) => (
            <p class="form-error" role="alert">
              {m()}
            </p>
          )}
        </Show>
      </section>
      <section class="lk-card" aria-label={t("invite.links.list")}>
        <h2>{t("invite.links.list")}</h2>
        <Show
          when={channelInviteLinks(props.id).some((l) => !l.revoked)}
          fallback={<p class="lk-meta">{t("invite.links.empty")}</p>}
        >
          <ul style={{ "list-style": "none", margin: 0, padding: 0 }}>
            <For each={channelInviteLinks(props.id).filter((l) => !l.revoked)}>
              {(l) => (
                <li class="lk-card-row">
                  <div class="lk-card-row-body">
                    <span class="lk-card-row-title">{l.label.length > 0 ? l.label : t("chat.invite_link.label")}</span>
                    <span class="lk-card-row-sub">{termsOf(l)}</span>
                  </div>
                  <button
                    type="button"
                    class="lk-card-row-action"
                    onClick={() =>
                      void copyMessageText(l.url).then((ok) =>
                        setNotice(ok ? t("chat.invite_link.copied") : t("chat.invite_link.copy_failed")),
                      )
                    }
                  >
                    {t("chat.invite_link.copy")}
                  </button>
                  <Show when={l.mine || channelRoleOf(props.id) === "owner"}>
                    <button type="button" class="danger lk-card-row-action" onClick={() => void revoke(l)}>
                      {t("chat.invite_link.revoke")}
                    </button>
                  </Show>
                </li>
              )}
            </For>
          </ul>
        </Show>
      </section>
    </div>
  );
}
