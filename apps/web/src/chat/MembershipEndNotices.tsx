// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `docs/ux/group-and-channel-privacy.md` section 9.1 — how a person learns
 * they are out of a group: once per device, and never who did it.
 *
 *   - anywhere else: a banner at the top of Chats, with a close icon;
 *   - in that conversation: a dialog over it, with one button, and the text
 *     the composer held, selectable, with `Copy`.
 *
 * The rules are `membership-end.ts`; the name is held only until the notice
 * closes.
 */

import { createSignal, For, onMount, Show, type JSX } from "solid-js";

import { base64Encode, base64UrlDecodeNoPad } from "../auth/account-id";
import { t } from "../l10n";
import { Icon } from "../ui/icons";
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

/** The route slug of a chat as the id a notice is filed under, or `null`. */
function routeIdOf(slug: string | null): string | null {
  if (slug === null) return null;
  try {
    return base64Encode(base64UrlDecodeNoPad(slug));
  } catch {
    return null;
  }
}

/** The banners at the top of Chats. A conversation open behind its dialog
 *  has its notice there, and shows none here. */
export function MembershipEndBanners(props: { readonly activeSlug: string | null }): JSX.Element {
  const end = useMembershipEnd();
  const shown = (): readonly EndNotice[] => {
    const open = routeIdOf(props.activeSlug);
    return end.notices().filter((n) => open === null || !n.routeIdsB64.includes(open));
  };
  return (
    <For each={shown()}>
      {(notice) => (
        <div class="lk-alert info lk-end-banner" role="status" aria-live="polite">
          <span class="lk-fill">{noticeTitle(notice)}</span>
          <button
            type="button"
            class="lk-icon-button"
            aria-label={t("common.close")}
            title={t("common.close")}
            onClick={() => end.dismissNotice(notice.key)}
          >
            <Icon name="close" size={16} />
          </button>
        </div>
      )}
    </For>
  );
}

/** The dialog over a conversation that ended while it was open. */
export function MembershipEndDialog(props: {
  readonly notice: EndNotice;
  readonly onDone: () => void;
}): JSX.Element {
  let el: HTMLDialogElement | undefined;
  const [copied, setCopied] = createSignal(false);
  onMount(() => el?.showModal());
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
          {t("chat.ended.notice_body")}
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
