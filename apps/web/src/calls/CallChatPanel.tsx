// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The chat panel of a group call (`docs/ux/calls.md` section 4.1.4).
 *
 * Under 900 px it covers the call screen (a header with Close and the round
 * mute button of the collapsed bar, since the control row is hidden behind
 * it); from 900 px it is a 360 px column at the end side and the control row
 * stays visible beside it. The decisions live in `call-chat-panel-state.ts`;
 * the messages in `call-chat.ts`.
 */
import { createEffect, createMemo, createSignal, For, onCleanup, onMount, Show, type Component } from "solid-js";

import { t } from "../l10n";
import { Icon } from "../ui/icons";
import { createLongPress } from "../ui/long-press";
import { MessageText } from "../chat/MessageText";
import { copyMessageText } from "../chat/copy-message";
import { shortTime } from "../chat/timeline-layout";
import { loadWasm } from "../wasm";
import { callActions, callState } from "../state/call-state";
import { callBarMuteControl } from "./call-bar-model";
import { callChatActions, callChatState, type CallChatMessage } from "./call-chat";
import {
  composerInput,
  EMPTY_COMPOSER,
  enterSends,
  startsOthersRun,
  type ChatTextRule,
  type ComposerText,
} from "./call-chat-panel-state";

/** The list counts as at the newest message within this distance (px). */
const AT_BOTTOM_SLACK = 24;
/** The field grows up to five lines, then scrolls. */
const COMPOSER_MAX_LINES = 5;

const [textRule, setTextRule] = createSignal<ChatTextRule | null>(null);
let ruleAsked = false;
function askForRule(): void {
  if (ruleAsked) return;
  ruleAsked = true;
  void loadWasm().then(
    (w) => setTextRule({ checkCallChatText: w.checkCallChatText, clampCallChatText: w.clampCallChatText }),
    () => {
      ruleAsked = false;
    },
  );
}

/** A phone's on-screen keyboard: its return key starts a line. */
function touchKeyboard(): boolean {
  return typeof window !== "undefined" && typeof window.matchMedia === "function"
    ? window.matchMedia("(hover: none) and (pointer: coarse)").matches
    : false;
}

const Bubble: Component<{
  message: CallChatMessage;
  name: string | null;
  onMenu: (message: CallChatMessage, x: number, y: number) => void;
}> = (props) => {
  const press = createLongPress((x, y) => props.onMenu(props.message, x, y));
  return (
    <li class="lk-call-chat-item" classList={{ own: props.message.own }}>
      <Show when={props.name}>{(name) => <span class="lk-call-chat-sender">{name()}</span>}</Show>
      <div
        class="lk-call-chat-bubble"
        classList={{ "not-sent": props.message.delivery === "not-sent" }}
        tabIndex={0}
        onContextMenu={(e): void => {
          e.preventDefault();
          props.onMenu(props.message, e.clientX, e.clientY);
        }}
        onPointerDown={press.onPointerDown}
        onPointerMove={press.onPointerMove}
        onPointerUp={press.onPointerUp}
        onPointerCancel={press.onPointerCancel}
        onPointerLeave={press.onPointerLeave}
        onClick={(e): void => void press.swallowClick(e)}
      >
        <span class="lk-call-chat-text">
          <MessageText text={props.message.text} />
        </span>
        <span class="lk-call-chat-time">{shortTime(new Date(props.message.arrivedAtMs).toISOString())}</span>
      </div>
      <Show when={props.message.delivery === "not-sent"}>
        <div class="lk-call-chat-failed">
          <span>{t("call.chat.not_sent")}</span>
          <button
            type="button"
            class="ghost lk-call-chat-retry"
            onClick={(): void => callChatState.control()?.sendAgain(props.message.id)}
          >
            {t("call.chat.send_again")}
          </button>
        </div>
      </Show>
    </li>
  );
};

export const CallChatPanel: Component<{
  shape: "sheet" | "column";
  /** The name the sender's tile shows (section 4.4), for a peer id. */
  nameOf: (peerId: string) => string;
  onClose: () => void;
}> = (props) => {
  askForRule();
  let list: HTMLDivElement | undefined;
  let field: HTMLTextAreaElement | undefined;
  const [composer, setComposer] = createSignal<ComposerText>(EMPTY_COMPOSER);
  const [said, setSaid] = createSignal("");
  // On a phone the one-row action sheet sits at the bottom of the screen; with
  // a pointer it is a popover where the right click was (design system,
  // Action sheet).
  const [menu, setMenu] = createSignal<{
    message: CallChatMessage;
    x: number;
    y: number;
    sheet: boolean;
  } | null>(null);

  const messages = callChatState.messages;
  const panel = callChatState.panel;
  const local = (): boolean | undefined => callState.participants().find((p) => p.isLocal)?.micOn;
  const mute = createMemo(() =>
    props.shape === "sheet" ? callBarMuteControl(callState.phase().kind, local() === true) : null,
  );

  const scrollToNewest = (): void => {
    requestAnimationFrame(() => {
      if (list !== undefined) list.scrollTop = list.scrollHeight;
    });
  };

  onMount(() => {
    field?.focus();
    scrollToNewest();
  });

  // A message that arrives while the person is at the newest one keeps them
  // there; one that arrives while they scrolled up does not move the list.
  // An arrival from someone else is announced politely.
  let lastId: string | null = messages().at(-1)?.id ?? null;
  createEffect(() => {
    const all = messages();
    const last = all.at(-1);
    if (last === undefined || last.id === lastId) return;
    lastId = last.id;
    if (panel().atBottom) scrollToNewest();
    if (!last.own && last.peerId !== null) {
      setSaid(t("call.chat.message_a11y", { name: props.nameOf(last.peerId), text: last.text }));
    }
  });

  const onScroll = (): void => {
    if (list === undefined) return;
    const atBottom = list.scrollHeight - list.scrollTop - list.clientHeight <= AT_BOTTOM_SLACK;
    callChatActions.scrolled(atBottom);
  };

  const fitField = (): void => {
    if (field === undefined) return;
    field.style.height = "auto";
    const style = getComputedStyle(field);
    const px = (v: string): number => Number.parseFloat(v) || 0;
    const line = px(style.lineHeight) || 22;
    const padding = px(style.paddingTop) + px(style.paddingBottom);
    const border = px(style.borderTopWidth) + px(style.borderBottomWidth);
    const max = line * COMPOSER_MAX_LINES + padding + border;
    field.style.height = `${Math.min(field.scrollHeight + border, max)}px`;
  };

  const onInput = (raw: string): void => {
    const rule = textRule();
    const next =
      rule === null
        ? { text: raw.replace(/\r\n?/g, "\n"), tooLong: false, sendable: false }
        : composerInput(raw, composer(), rule);
    setComposer(next);
    if (field !== undefined && field.value !== next.text) {
      const at = Math.min(field.selectionStart ?? next.text.length, next.text.length);
      field.value = next.text;
      field.setSelectionRange(at, at);
    }
    fitField();
  };

  const send = (): void => {
    const c = composer();
    const control = callChatState.control();
    if (!c.sendable || control === null) return;
    control.send(c.text);
    setComposer(EMPTY_COMPOSER);
    if (field !== undefined) {
      field.value = "";
      fitField();
      field.focus();
    }
    callChatActions.ownSent();
    scrollToNewest();
  };

  const openMenu = (message: CallChatMessage, x: number, y: number): void => {
    setMenu({ message, x, y, sheet: touchKeyboard() });
  };

  // The menu's own Esc, then the panel's; the call screen's own `Esc`
  // (put the call away) never sees it.
  const onKeyDown = (e: KeyboardEvent): void => {
    if (e.key !== "Escape") return;
    e.preventDefault();
    e.stopPropagation();
    if (menu() !== null) setMenu(null);
    else props.onClose();
  };

  onCleanup(() => setMenu(null));

  return (
    <aside
      class="lk-call-chat"
      data-shape={props.shape}
      role="region"
      aria-label={t("call.chat.a11y")}
      // Not delegated: the call screen listens for `Esc` on the document,
      // and only a listener on this element can stop it reaching there.
      on:keydown={onKeyDown}
    >
      <header class="lk-call-chat-head">
        <h2>{t("call.control.chat")}</h2>
        <div class="lk-call-chat-head-actions">
          <Show when={mute()}>
            {(spec) => (
              <button
                type="button"
                class="lk-callbar-mute lk-call-chat-mute"
                data-tone={spec().tone}
                aria-label={spec().accessibleName}
                aria-pressed={spec().pressed ?? undefined}
                title={spec().accessibleName}
                disabled={spec().disabled}
                onClick={(): void => callActions.setMic(local() !== true)}
              >
                <Icon name={spec().icon} size={18} />
              </button>
            )}
          </Show>
          <button
            type="button"
            class="lk-call-chat-close"
            classList={{ down: props.shape === "sheet" }}
            aria-label={t("call.chat.close_a11y")}
            title={t("call.chat.close_a11y")}
            onClick={(): void => props.onClose()}
          >
            <Icon name={props.shape === "sheet" ? "chevron" : "close"} size={20} />
          </button>
        </div>
      </header>

      <div class="lk-call-chat-body">
      <div class="lk-call-chat-list" ref={list} onScroll={onScroll}>
        <Show
          when={messages().length > 0}
          fallback={
            <div class="lk-call-chat-empty">
              <p>{t("call.chat.notice_people")}</p>
              <p>{t("call.chat.notice_ends")}</p>
            </div>
          }
        >
          <p class="lk-call-chat-note">
            <span>{t("call.chat.notice_people")}</span>
            <span>{t("call.chat.notice_ends")}</span>
          </p>
          <ol class="lk-call-chat-messages">
            <For each={messages()}>
              {(message, i) => (
                <Bubble
                  message={message}
                  name={
                    startsOthersRun(messages(), i()) && message.peerId !== null
                      ? props.nameOf(message.peerId)
                      : null
                  }
                  onMenu={openMenu}
                />
              )}
            </For>
          </ol>
        </Show>
      </div>

      <Show when={panel().newBelow}>
        <button
          type="button"
          class="lk-call-chat-pill"
          onClick={(): void => {
            callChatActions.scrolled(true);
            scrollToNewest();
          }}
        >
          {t("call.chat.new_messages")}
        </button>
      </Show>
      </div>

      <div class="lk-call-chat-composer">
        <div class="lk-call-chat-field-row">
          <textarea
            ref={field}
            class="lk-call-chat-field"
            rows={1}
            placeholder={t("call.chat.placeholder")}
            aria-label={t("call.chat.placeholder")}
            onInput={(e): void => onInput(e.currentTarget.value)}
            onKeyDown={(e): void => {
              if (
                enterSends({
                  key: e.key,
                  shiftKey: e.shiftKey,
                  isComposing: e.isComposing,
                  touchKeyboard: touchKeyboard(),
                })
              ) {
                e.preventDefault();
                send();
              }
            }}
          />
          <button
            type="button"
            class="lk-call-chat-send"
            aria-label={t("call.chat.send_a11y")}
            title={t("call.chat.send_a11y")}
            disabled={!composer().sendable}
            onClick={send}
          >
            <Icon name="send" size={20} />
          </button>
        </div>
        <Show when={composer().tooLong}>
          <p class="lk-call-chat-too-long">{t("call.chat.too_long")}</p>
        </Show>
      </div>

      <p class="lakko-visually-hidden" role="status" aria-live="polite">
        {said()}
      </p>

      <Show when={menu()}>
        {(m) => (
          <>
            <div class="lk-call-chat-menu-scrim" onPointerDown={(): void => void setMenu(null)} />
            <div
              class="lk-call-menu lk-call-chat-menu"
              classList={{ sheet: m().sheet }}
              role="menu"
              style={m().sheet ? undefined : { left: `${m().x}px`, top: `${m().y}px` }}
              ref={(el): void => {
                // Read now: the menu may be gone by the next frame.
                const { x, y, sheet } = m();
                requestAnimationFrame(() => {
                  if (!el.isConnected) return;
                  if (sheet) {
                    el.querySelector<HTMLElement>("[role=menuitem]")?.focus();
                    return;
                  }
                  const rect = el.getBoundingClientRect();
                  const left = Math.max(8, Math.min(x, window.innerWidth - rect.width - 8));
                  const top = Math.max(8, Math.min(y, window.innerHeight - rect.height - 8));
                  el.style.left = `${left}px`;
                  el.style.top = `${top}px`;
                  el.querySelector<HTMLElement>("[role=menuitem]")?.focus();
                });
              }}
            >
              <button
                type="button"
                role="menuitem"
                class="lk-call-menu-item"
                onClick={(): void => {
                  const text = m().message.text;
                  setMenu(null);
                  void copyMessageText(text);
                }}
              >
                <Icon name="copy" size={20} />
                <span>{t("common.copy")}</span>
              </button>
            </div>
          </>
        )}
      </Show>
    </aside>
  );
};
