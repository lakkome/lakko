// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `docs/ux/group-and-channel-privacy.md` section 9.1 — how a person learns
 * they are out of a group: once per device, and never who did it.
 *
 *   - anywhere else: a short notice at the bottom that goes by itself after
 *     4 s, no close icon, one at a time (2026-11-06: replaced the banners);
 *   - in that conversation: a dialog over it, with one button, and the text
 *     the composer held, selectable, with `Copy`.
 *
 * The rules are `membership-end.ts` and `end-toast.ts`; the name is held
 * in memory only until it was shown.
 */

import { createEffect, createSignal, onCleanup, onMount, Show, type JSX } from "solid-js";

import { t } from "../l10n";
import { addToEndToast, END_TOAST_CLAIM_WAIT_MS, END_TOAST_MS, endToastText, type EndToast } from "./end-toast";
import { copyMessageText } from "./copy-message";
import { UNSENT_DRAFT_LABEL } from "./membership-ended";
import type { EndNotice } from "./membership-end";
import { useMembershipEnd } from "./MembershipEndContext";

/** The name a notice speaks of. A conversation with none known reads as the
 *  generic word rather than as a sentence with a hole in it. */
export function noticeTitle(notice: Pick<EndNotice, "title">): string {
  const name = notice.title.trim();
  return t("chat.ended.notice", { name: name.length > 0 ? name : t("chat.group.default_name") });
}

/**
 * The short notice at the bottom (UX section 9, 2026-11-06): one at a time,
 * gone by itself after {@link END_TOAST_MS}, no close icon. Mounted once for
 * the whole app. A new end waits {@link END_TOAST_CLAIM_WAIT_MS} for the
 * dialog over its open conversation to claim it; an end not claimed is told
 * here and forgotten (nothing is kept for later).
 */
export function MembershipEndToast(): JSX.Element {
  const end = useMembershipEnd();
  const [toast, setToast] = createSignal<EndToast | null>(null);
  const handled = new Set<string>();
  let hideTimer: ReturnType<typeof setTimeout> | null = null;
  const timers = new Set<ReturnType<typeof setTimeout>>();

  const show = (names: readonly string[]): void => {
    if (names.length === 0) return;
    setToast((t0) => addToEndToast(t0, names));
    if (hideTimer !== null) clearTimeout(hideTimer);
    hideTimer = setTimeout(() => {
      hideTimer = null;
      setToast(null);
    }, END_TOAST_MS);
  };

  createEffect(() => {
    const fresh = end.notices().filter((n) => !handled.has(n.key));
    if (fresh.length === 0) return;
    for (const n of fresh) handled.add(n.key);
    const timer = setTimeout(() => {
      timers.delete(timer);
      const tell = fresh.filter((n) => !end.isNoticeClaimed(n.key));
      for (const n of tell) end.dismissNotice(n.key);
      show(tell.map(noticeName));
    }, END_TOAST_CLAIM_WAIT_MS);
    timers.add(timer);
  });
  onCleanup(() => {
    if (hideTimer !== null) clearTimeout(hideTimer);
    for (const timer of timers) clearTimeout(timer);
  });

  return (
    <Show when={toast()}>
      {(t0) => (
        <div class="lk-toast lk-end-toast" role="status" aria-live="polite">
          {endToastText(
            t0(),
            (name) => t("chat.ended.notice", { name }),
            (name, count) => t("chat.ended.notice_more", { name, count }),
          )}
        </div>
      )}
    </Show>
  );
}

/** The name a notice speaks of, with the generic word when none is known. */
function noticeName(notice: Pick<EndNotice, "title">): string {
  const name = notice.title.trim();
  return name.length > 0 ? name : t("chat.group.default_name");
}

/** The dialog over a conversation that ended while it was open. */
export function MembershipEndDialog(props: {
  readonly notice: EndNotice;
  readonly onDone: () => void;
  /** The body line, when the conversation is not a group (a channel). */
  readonly body?: string;
}): JSX.Element {
  let el: HTMLDialogElement | undefined;
  const [copied, setCopied] = createSignal(false);
  const end = useMembershipEnd();
  onMount(() => {
    // The dialog tells of this end: the notice at the bottom leaves it out.
    end.claimNotice(props.notice.key);
    el?.showModal();
  });
  return (
    <dialog
      ref={el}
      class="lk-end-dialog"
      aria-labelledby="lakko-end-title"
      aria-describedby="lakko-end-body"
      // Escape and OK are the same answer: there is nothing to decline.
      onClose={() => props.onDone()}
    >
      <div class="lk-end-dialog-card">
        <strong id="lakko-end-title">{noticeTitle(props.notice)}</strong>
        <span id="lakko-end-body" class="lk-muted">
          {props.body ?? t("chat.ended.notice_body")}
        </span>
        <Show when={props.notice.draft}>
          {(draft) => (
            <div class="lk-end-dialog-draft">
              <span class="lk-meta">{UNSENT_DRAFT_LABEL()}</span>
              {/* Selectable text, never a disabled field: the person gets
                  their words out of it. */}
              <p class="lk-end-dialog-text">{draft()}</p>
              <button
                type="button"
                class="small ghost"
                onClick={() => {
                  void copyMessageText(draft()).then(setCopied);
                }}
              >
                <Show when={copied()} fallback={t("common.copy")}>
                  {t("common.copied")}
                </Show>
              </button>
            </div>
          )}
        </Show>
        <div class="lk-row-end">
          <button type="button" class="primary" onClick={() => el?.close()}>
            {t("common.ok")}
          </button>
        </div>
      </div>
    </dialog>
  );
}
