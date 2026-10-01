// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The muted conversation's call, offered inside that conversation
 * (`docs/ux/calls.md` section 2.1.1, first surface).
 *
 * ## What it is, said by what it is not
 *
 * It is not {@link IncomingCallScreen} made smaller. That screen is a modal
 * dialog: it covers everything, it takes focus on mount, it traps Tab, and
 * Escape declines the call. Every one of those is right for a call that is
 * ALLOWED to interrupt, and every one of them is wrong here — a person who
 * muted a conversation asked not to be dragged out of what they are doing by
 * it, and a panel that steals the caret and swallows Escape does exactly
 * that, quietly.
 *
 * So this is a plain region in the chat's own column: no `role="dialog"`, no
 * `aria-modal`, no focus move, no focus trap, no key handler of any kind. It
 * sits between the conversation's header and its scrolling timeline, in flow,
 * so it covers nothing the person was reading — which is section 2.1.1's own
 * requirement, and the reason it is NOT a sticky strip over the timeline like
 * the group-call banner below it.
 *
 * Escape is deliberately not bound. On the full screen Escape is the
 * conventional dismissal and declining is what dismissal means; here the
 * person is reading a chat, and Escape belongs to whatever they are doing —
 * closing a menu, leaving a search field. Declining a call by reflex is not a
 * thing this surface may do.
 *
 * ## `Accept without video`
 *
 * Section 2.2 gives a video call three actions, and this panel carries all
 * three even though it is small. The alternative is worse than a cramped row:
 * the third action's only other home is the full screen, which is exactly the
 * surface mute withholds, so dropping it here would mean a muted video call
 * can be answered in no way except with the camera on. The row wraps rather
 * than squeezing, and the two words are a ghost button so the primary action
 * still reads first.
 */
import { Show } from "solid-js";
import type { JSX } from "solid-js";

import { Avatar } from "../ui/Avatar";
import { Icon } from "../ui/icons";
import { t } from "../l10n";
import { quietCallKindLabel } from "./quiet-call-offer";

export interface QuietIncomingCallPanelProps {
  /** The caller, as this conversation names them. */
  readonly callerName: string;
  /** Stable id the avatar tint is hashed from, so the colour matches the
   *  conversation's own avatar. */
  readonly callerId: string | null;
  /** The kind of call the CALLER placed. */
  readonly video: boolean;
  readonly onAccept: (withVideo: boolean) => void;
  readonly onDecline: () => void;
}

export function QuietIncomingCallPanel(props: QuietIncomingCallPanelProps): JSX.Element {
  const kindLabel = (): string => quietCallKindLabel(props.video);

  return (
    <section
      class="lk-quiet-call"
      /* Announced when it appears, because nothing else announces it: there is
         no sound, no vibration and no focus move. `polite` rather than
         `assertive` — the whole surface exists to not interrupt. */
      role="status"
      aria-label={t("chats.row.call.label", { kind: kindLabel(), name: props.callerName })}
    >
      <Avatar label={props.callerName} id={props.callerId ?? props.callerName} size="sm" />
      <div class="lk-quiet-call-who">
        <strong class="lk-ellipsis">{props.callerName}</strong>
        <span class="lk-quiet-call-kind">{kindLabel()}</span>
      </div>
      <div class="lk-quiet-call-actions">
        <button
          type="button"
          class="danger"
          aria-label={t(props.video ? "call.incoming.decline_video_a11y" : "call.incoming.decline_voice_a11y", { name: props.callerName })}
          onClick={() => props.onDecline()}
        >
          <Icon name="phoneEnd" size={16} />
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
          onClick={() => props.onAccept(props.video)}
        >
          <Icon name={props.video ? "video" : "phone"} size={16} />
          {t("call.incoming.accept")}
        </button>
      </div>
    </section>
  );
}
