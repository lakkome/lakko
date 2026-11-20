// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The full-screen incoming call (`docs/ux/calls.md` §2.2).
 *
 * ## What this replaces
 *
 * A small banner pinned to the top of whatever screen the user was on, with
 * three small buttons in a row. It read as a notice about a call rather than
 * as a call, and it competed for attention with the page under it — exactly
 * what §1 rules out: "a call is an interruption, and it is allowed to be one.
 * Ringing takes the whole screen."
 *
 * ## What is on it, and what is deliberately not
 *
 * Caller avatar at 96, caller name, one line saying `Voice call` or
 * `Video call`, a small lock reading `End-to-end encrypted`, and the actions.
 * Nothing else — §2.2 is explicit: "No code, no digits, no protocol words."
 * The safety number belongs to the call screen's encryption sheet (§5), not
 * here, and the 1x1 web path has none anyway.
 *
 * The word for the kind of call comes from the OFFER's media flags, which is
 * what §1 means by "the word on the callee's screen matches what the caller
 * chose, always". A video call adds `Accept without video` between Decline
 * and Accept, so answering a video call with audio only is one press rather
 * than a press followed by a scramble for the camera toggle.
 *
 * ## Accessibility (§8)
 *
 * A modal dialog with a spoken label naming the caller and the kind of call
 * (`Accept voice call from Anna` is §8's own example, and is what the Accept
 * button's label reads). Focus moves to Accept on mount, Escape declines —
 * the conventional dismissal, and the safe one here, since it matches the
 * button the user could reach anyway rather than silently leaving the call
 * ringing. Tab is trapped inside the dialog: with the whole screen covered,
 * tabbing out would move focus to controls the user cannot see.
 */
import { createEffect, onCleanup, Show } from "solid-js";
import type { JSX } from "solid-js";

import { Avatar } from "../ui/Avatar";
import { Icon } from "../ui/icons";
import { t } from "../l10n";

export interface IncomingCallScreenProps {
  /** The caller, as this device names them. */
  readonly callerName: string;
  /** Stable id the avatar tint is chosen from, so the colour matches the
   *  chat list. `null` falls back to the name. */
  readonly callerId: string | null;
  /** ADR-0084 - the caller's profile key (a one-to-one), for their photo. */
  readonly callerKey?: string | null;
  /** The kind of call the CALLER placed. */
  readonly video: boolean;
  readonly onAccept: (withVideo: boolean) => void;
  readonly onDecline: () => void;
}

export function IncomingCallScreen(props: IncomingCallScreenProps): JSX.Element {
  let root: HTMLDivElement | undefined;
  let acceptButton: HTMLButtonElement | undefined;

  const kindLabel = (): string => (props.video ? t("call.kind.video") : t("call.kind.voice"));

  // Focus the primary action as the screen appears, so a keyboard user can
  // answer with Enter and a screen reader announces the dialog.
  createEffect(() => {
    acceptButton?.focus();
  });

  // Escape declines, and Tab stays inside the dialog. Bound on the document
  // rather than the container because the screen covers everything: a key
  // pressed with focus anywhere belongs to the call.
  createEffect(() => {
    const onKeyDown = (event: KeyboardEvent): void => {
      if (event.key === "Escape") {
        event.preventDefault();
        props.onDecline();
        return;
      }
      if (event.key !== "Tab" || root === undefined) return;
      const focusable = [...root.querySelectorAll<HTMLElement>("button")].filter(
        (el) => !el.hasAttribute("disabled"),
      );
      if (focusable.length === 0) return;
      const first = focusable[0] as HTMLElement;
      const last = focusable[focusable.length - 1] as HTMLElement;
      const active = document.activeElement;
      if (event.shiftKey && (active === first || !root.contains(active))) {
        event.preventDefault();
        last.focus();
      } else if (!event.shiftKey && active === last) {
        event.preventDefault();
        first.focus();
      }
    };
    document.addEventListener("keydown", onKeyDown);
    onCleanup(() => document.removeEventListener("keydown", onKeyDown));
  });

  return (
    <div
      class="lk-incoming-call"
      role="dialog"
      aria-modal="true"
      aria-label={t(props.video ? "call.incoming.video_a11y" : "call.incoming.voice_a11y", { name: props.callerName })}
      ref={(el) => {
        root = el;
      }}
    >
      <div class="lk-incoming-call-who">
        <Avatar
          label={props.callerName}
          id={props.callerId ?? props.callerName}
          personKey={props.callerKey ?? undefined}

          size="xl"
        />
        <h1 class="lk-incoming-call-name">{props.callerName}</h1>
        <p class="lk-incoming-call-kind">{kindLabel()}</p>
        <p class="lk-incoming-call-lock">
          <Icon name="lock-fill" size={14} />
          <span>{t("call.e2ee")}</span>
        </p>
      </div>

      <div class="lk-incoming-call-actions">
        <button
          type="button"
          class="danger filled"
          aria-label={t(props.video ? "call.incoming.decline_video_a11y" : "call.incoming.decline_voice_a11y", { name: props.callerName })}
          onClick={() => props.onDecline()}
        >
          <Icon name="phone-down-fill" size={20} />
          {t("call.incoming.decline")}
        </button>
        <Show when={props.video}>
          <button
            type="button"
            class="ghost"
            aria-label={t("call.incoming.accept_without_video_a11y", { name: props.callerName })}
            onClick={() => props.onAccept(false)}
          >
            {t("call.incoming.accept_without_video")}
          </button>
        </Show>
        <button
          type="button"
          class="primary"
          aria-label={t(props.video ? "call.incoming.accept_video_a11y" : "call.incoming.accept_voice_a11y", { name: props.callerName })}
          ref={(el) => {
            acceptButton = el;
          }}
          onClick={() => props.onAccept(props.video)}
        >
          <Icon name={props.video ? "video-fill" : "phone-fill"} size={20} />
          {t("call.incoming.accept")}
        </button>
      </div>
    </div>
  );
}
