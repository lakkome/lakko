// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `/invited/:hex` — the chat with a person this account added through their
 * invite link, before their app has answered (ADR-0079 section 5,
 * `docs/ux/contact-invite-link.md` section 4).
 *
 * The conversation does not exist yet: the inviter's app founds it when it
 * takes the request. Until then this view shows one line at the top of the
 * timeline ("Anna will get your messages when their app is next online.",
 * after 7 days "Anna has not answered yet. You can ask them for a new
 * link."), the messages written so far in the sending state, a composer that
 * keeps what is written on this device in the one held-send queue under the
 * inviter's account id (`chat/held-send-queue.ts`, `pendingQueueKey`), and
 * the call buttons disabled with the hint "Available once Anna is
 * connected." Text only: an attachment needs the conversation to exist.
 *
 * When the inviter's Welcome lands, the mailbox moves the messages to the
 * held-send queue of the new one-to-one and drops the record; this view then
 * follows to the real chat.
 *
 * Above them: what this account wrote to the same person on ANOTHER of its
 * devices before this one was paired (state bundle v3), as display-only
 * copies in the same waiting state. They are never sent from here; the
 * device where they were typed sends them (`contacts/carried-copies.ts`).
 */
import { accountSuspended } from "../auth/suspension-state";
import { SuspendedComposerLine } from "../auth/SuspendedReadOnly";
import { createEffect, createMemo, createSignal, For, Show, type JSX } from "solid-js";
import { useNavigate, useParams } from "@solidjs/router";

import { useAuth } from "../auth/AuthContext";
import { base64UrlEncodeNoPad } from "../auth/account-id";
import { useChats } from "../chat/ChatContext";
import { conversationKindOf } from "../chat/conversation-kind";
import { personKeysOf } from "../chat/person-key";
import { heldSends, persistHeldSends, restoreHeldSends } from "../chat/held-send-queue";
import { carriedCopiesFor, loadCarriedCopies } from "../contacts/carried-copies";
import {
  loadPendingContacts,
  pendingContacts,
  pendingLine,
  pendingNameOf,
  pendingQueueKey,
  type PendingContact,
} from "../contacts/pending-contacts";
import { t } from "../l10n";
import { Avatar } from "../ui/Avatar";
import { Icon } from "../ui/icons";
import { paths } from "./paths";

export default function PendingChat(): JSX.Element {
  const params = useParams<{ hex: string }>();
  const auth = useAuth();
  const chats = useChats();
  const navigate = useNavigate();
  const [input, setInput] = createSignal("");
  const [loaded, setLoaded] = createSignal(false);
  /** Bumped after every change of the queue this view makes (the held-send
   *  store is not reactive). */
  const [queueVersion, setQueueVersion] = createSignal(0);

  const accountIdB64 = (): string | null => {
    const s = auth.state();
    return s.kind === "session" ? s.account.accountId : null;
  };
  createEffect(() => {
    const account = accountIdB64();
    if (account === null) return;
    void loadPendingContacts(account).then(() => setLoaded(true));
    void loadCarriedCopies(account);
    // The held messages survive a reload in the message log.
    void restoreHeldSends(account).then(() => setQueueVersion((v) => v + 1));
  });

  const contact = createMemo((): PendingContact | undefined =>
    pendingContacts().find((c) => c.accountIdHex === params.hex),
  );

  /** What was written to this person so far, in order. */
  const queued = createMemo(() => {
    queueVersion();
    const c = contact();
    if (c === undefined) return [];
    const key = pendingQueueKey(c);
    return heldSends.all().find(([k]) => k === key)?.[1] ?? [];
  });
  /** What was written to this person on another device of this account
   *  (display only, never sent from here), in order. */
  const carried = createMemo(() => {
    const c = contact();
    return c === undefined ? [] : carriedCopiesFor(pendingQueueKey(c));
  });
  const name = (): string => {
    const c = contact();
    return (c === undefined ? null : pendingNameOf(c)) ?? t("invite.contact.page.unnamed");
  };

  /** The one-to-one with this person, once their app has answered. */
  const conversation = createMemo(() => {
    const c = contact();
    const key = c?.signaturePubkeyB64;
    if (key === undefined) return undefined;
    return chats
      .groups()
      .find((g) => conversationKindOf(g.group) === "chat" && personKeysOf(g.group).includes(key));
  });

  // Follow to the real chat when it exists; leave when the record is gone
  // and there is nothing to follow to.
  createEffect(() => {
    const g = conversation();
    if (g !== undefined) {
      navigate(paths.chat(base64UrlEncodeNoPad(g.group.groupId)), { replace: true });
      return;
    }
    if (loaded() && contact() === undefined) navigate(paths.home, { replace: true });
  });

  async function onSend(e: SubmitEvent): Promise<void> {
    e.preventDefault();
    const text = input();
    const account = accountIdB64();
    const c = contact();
    if (text.trim().length === 0 || account === null || c === undefined) return;
    setInput("");
    heldSends.hold(pendingQueueKey(c), { text, attached: [], reply: undefined }, Date.now());
    setQueueVersion((v) => v + 1);
    await persistHeldSends(account);
  }

  const callHint = (): string => t("chat.pending.call_disabled", { name: name() });

  return (
    <Show when={contact()}>
      {(c) => (
        <article class="lk-chat">
          <header class="lk-chat-head">
            <button
              type="button"
              class="lk-icon-button lk-narrow-only"
              aria-label={t("chat.header.back")}
              title={t("chat.header.back")}
              onClick={() => navigate(paths.home)}
            >
              <Icon name="chevron-back" size={24} />
            </button>
            <div class="lk-chat-title">
              <Avatar label={name()} id={c().signaturePubkeyB64} size="sm" personKey={c().signaturePubkeyB64} playVideo="header" />
              <div class="lk-chat-title-text">
                <h1>{name()}</h1>
                <Show when={c().username.length > 0}>
                  <div class="lk-chat-sub">@{c().username}</div>
                </Show>
              </div>
            </div>
            <div class="lk-chat-actions">
              {/* UX section 4: no call until the chat is open on both sides. */}
              <button type="button" class="lk-icon-button" title={callHint()} aria-label={callHint()} disabled>
                <Icon name="phone" size={24} />
              </button>
              <button type="button" class="lk-icon-button" title={callHint()} aria-label={callHint()} disabled>
                <Icon name="video" size={24} />
              </button>
            </div>
          </header>
          <div class="lk-chat-body">
            <div class="lk-chat-scroll">
          <ol class="lk-timeline">
            <li class="lk-day-sep" role="status">
              <span>
                {pendingLine(c(), Date.now()) === "stale"
                  ? t("chat.pending.stale", { name: name() })
                  : t("chat.pending.waiting", { name: name() })}
              </span>
            </li>
            <For each={carried()}>
              {(m) => (
                <li class="lk-bubble-row outgoing run-start run-end">
                  <div class="lk-bubble outgoing run-start run-end">
                    <div class="lk-bubble-text">{m.text}</div>
                    <div class="lk-bubble-meta">
                      <span>{t("chat.message.sending")}</span>
                    </div>
                  </div>
                </li>
              )}
            </For>
            <For each={queued()}>
              {(m) => (
                <li class="lk-bubble-row outgoing run-start run-end">
                  <div class="lk-bubble outgoing run-start run-end">
                    <div class="lk-bubble-text">{m.payload.text}</div>
                    <div class="lk-bubble-meta">
                      <span>{t("chat.message.sending")}</span>
                    </div>
                  </div>
                </li>
              )}
            </For>
          </ol>
            </div>
          {/* ADR-0099 step 3: nothing is queued for a contact while suspended. */}
          <SuspendedComposerLine />
          <form
            class="lk-composer"
            style={accountSuspended() ? { display: "none" } : undefined}
            onSubmit={(e) => void onSend(e)}
          >
            <textarea
              class="lk-composer-input"
              rows={1}
              placeholder={t("chat.composer.placeholder")}
              autocomplete="off"
              value={input()}
              onInput={(e) => setInput(e.currentTarget.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter" && !e.shiftKey && !e.isComposing) {
                  e.preventDefault();
                  e.currentTarget.form?.requestSubmit();
                }
              }}
            />
            <button
              type="submit"
              class="lk-send-glyph"
              aria-label={t("chat.composer.send")}
              title={t("chat.composer.send")}
              disabled={input().trim().length === 0}
            >
              <Icon name="arrow-up-circle-fill" size={26} />
            </button>
          </form>
          </div>
        </article>
      )}
    </Show>
  );
}
