// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The collapsed call — one bar across the top of the app
 * (`docs/ux/calls.md` section 4.5).
 *
 * A call that owns the whole screen owns the whole app: a person on a call
 * could not look up the address they were being asked for, read the message
 * that had just arrived, or answer the one question the call was about. This is
 * the surface that gives the app back. It sits above whatever screen is showing
 * and survives every navigation, because the call is owned by the app-level
 * host (`CallHostContext.tsx`) and not by a screen — collapsing changes nothing
 * about the call itself, only the picture is put away.
 *
 * ## What it carries, left to right
 *
 * Who is on the call; the state or the running time and never both; a lock only
 * when the call screen would show its encryption chip; a camera glyph while
 * this device's camera is sending; one round mute button.
 *
 * ## What it deliberately does not carry
 *
 * **End.** Section 4.5: ending is the one irreversible control in a call, the
 * call screen sets it apart by twice the gap and paints it the only red on the
 * screen, and a strip a thumb crosses on the way to a chat is the wrong place
 * for it. Coming back costs one tap. Mute is here because it is the control a
 * person needs in the second they need it, and because pressing it the wrong
 * way round is undone by pressing it again.
 *
 * ## Where the decisions live
 *
 * Nowhere here. Whether the bar is on screen and what its line says are the
 * shared rules of `crates/lakko-ui-rules/src/call_bar.rs`, reached through
 * `call-bar-model.ts`; the label is `call-screen-model.ts` `callPeerLabel`, the
 * same function the call screen's own heading uses; the mute button's icon,
 * tone, name and disabled state are `call-controls.ts`, the same definition the
 * call screen's row is built from; and the lock is `encryption-chip.ts`
 * `showsEncryptionChip`, the same predicate that shows the chip. This file is
 * the markup, the two event handlers, and the one piece of layout bookkeeping
 * below.
 *
 * ## How it avoids covering anything
 *
 * The host mounts this outside the app shell, so it is a fixed strip over the
 * viewport and would otherwise sit on top of the navigation rail and the top of
 * whatever pane is open. Instead it marks `documentElement` while it is up, and
 * the stylesheet pads `body` by the bar's height with `box-sizing: border-box`
 * — so `#root` (and with it the rail, the pane, the composer and the mobile tab
 * bar) becomes exactly that much shorter and nothing is overlapped. The mark is
 * removed on cleanup, including when the call ends under it.
 */

import { Show, createEffect, createMemo, createSignal, onCleanup, type JSX } from "solid-js";

import { Icon } from "../ui/icons";
import { callActions, callState, type Participant } from "../state/call-state";
import { uiRules } from "../chat/ui-rules";
import { showsEncryptionChip } from "./encryption-chip";
import { callPeerLabel, callSurfaceKind } from "./call-screen-model";
import {
  callBarMuteControl,
  callBarPhase,
  callBarStatus,
  type CallBarRules,
} from "./call-bar-model";

/** The attribute the stylesheet reads to make room for the bar. */
const ROOM_ATTRIBUTE = "data-call-bar";

export interface CallBarProps {
  /**
   * The other person, as the host resolved them, or `null` on a group or link
   * call. Exactly what the host hands the call screen, so the two show the
   * same name.
   */
  readonly peerName: string | null;
  /** A group or link call: the group label rather than a person's name. */
  readonly groupCall: boolean;
  /** A 1x1 call is ringing and unanswered — the one phase the store does not
   *  carry, because such a call takes the whole screen instead (§2.2). */
  readonly ringing: boolean;
  /** The call screen is up; the bar is never shown over it (§4.5). */
  readonly callScreenUp: boolean;
  /** Bring the call back. */
  readonly onExpand: () => void;
}

/**
 * The bar, or nothing.
 *
 * Renders nothing at all until wasm has loaded: the visibility decision is the
 * shared rule's and there is no second copy of it here to fall back to. The
 * window is theoretical in the running app — a call needs an MLS exporter, so
 * wasm is long resolved before one exists — and erring towards no bar is the
 * side that cannot lie about a call.
 */
export function CallBar(props: CallBarProps): JSX.Element {
  const rules = (): CallBarRules | null => uiRules();

  const phase = createMemo(() => {
    const r = rules();
    if (r === null) return null;
    return callBarPhase(r, callState.phase().kind, props.ringing);
  });

  const shown = createMemo(() => {
    const r = rules();
    const p = phase();
    if (r === null || p === null) return false;
    return r.callBarVisible(p, props.callScreenUp);
  });

  /** A one-second tick, only while the bar is up: the running time needs no
   *  finer resolution, and nothing should be ticking when nothing is shown. */
  const now = tickingClock(shown);

  const local = (): Participant | null =>
    callState.participants().find((participant) => participant.isLocal) ?? null;
  const remotes = createMemo(() => callState.participants().filter((p) => !p.isLocal));

  /** The same label the call screen puts at the top, from the same inputs. */
  const label = createMemo(() =>
    callPeerLabel({
      surface: callSurfaceKind({
        groupCall: props.groupCall,
        remoteCount: remotes().length,
        remoteCameraOn: remotes()[0]?.camOn === true,
        localCameraOn: local()?.camOn === true,
      }),
      peerName: props.peerName,
      participantCount: callState.participants().length,
      soleRemoteName: remotes()[0]?.displayName,
    }),
  );

  const status = createMemo(() => {
    const r = rules();
    const p = phase();
    if (r === null || p === null) return { text: "", spoken: "" };
    return callBarStatus(r, {
      phase: p,
      connectedAtMs: callState.connectedAt(),
      nowMs: now(),
    });
  });

  const mute = createMemo(() =>
    callBarMuteControl(callState.phase().kind, local()?.micOn === true),
  );

  const encrypted = createMemo(() => showsEncryptionChip(callState.e2ee()));
  const cameraSending = (): boolean => local()?.camOn === true;

  /**
   * What a screen reader reads for the way back in.
   *
   * The lock and the camera glyph are folded in here rather than labelled where
   * they are drawn: an `aria-label` on a button replaces everything inside it,
   * so a labelled icon within one is never announced. Without this a screen
   * reader user would be told the call is there and not that their camera is
   * still sending, which is the one thing the glyph exists to say.
   */
  const spokenName = createMemo(() => {
    const parts = [`Return to the call. ${label()}`, status().spoken];
    if (encrypted()) parts.push("encrypted");
    if (cameraSending()) parts.push("your camera is on");
    return parts.filter((part) => part.length > 0).join(", ");
  });

  // Make room rather than cover: see the module note. Tied to `shown()` so a
  // call that ends while collapsed gives the height back without anyone
  // remembering to.
  createEffect(() => {
    const root = globalThis.document?.documentElement;
    if (root === undefined) return;
    if (shown()) root.setAttribute(ROOM_ATTRIBUTE, "");
    else root.removeAttribute(ROOM_ATTRIBUTE);
  });
  onCleanup(() => {
    globalThis.document?.documentElement?.removeAttribute(ROOM_ATTRIBUTE);
  });

  return (
    <Show when={shown()}>
      <div class="lk-callbar" role="region" aria-label="Call in progress">
        {/* Everything except the mute circle expands the call. A real button
            rather than a `role` on the container, so `Enter` and `Space` work
            without being re-implemented and the mute button is a sibling
            instead of an interactive element nested inside another one. */}
        <button
          type="button"
          class="lk-callbar-open"
          aria-label={spokenName()}
          onClick={(): void => props.onExpand()}
        >
          <span class="lk-callbar-who">{label()}</span>
          {/* The state or the time, never two at once — which is the shared
              rule's job, not a condition here: it answered one of them. */}
          <span class="lk-callbar-state" aria-hidden="true">
            {status().text}
          </span>
          {/* Only when the call screen would show its chip. No word beside it:
              §4.5 — "a claim about encryption belongs where the person can open
              it and read the safety number, and that is one tap away". */}
          <Show when={encrypted()}>
            <span class="lk-callbar-mark" aria-hidden="true">
              <Icon name="lock" size={13} />
            </span>
          </Show>
          {/* So a collapsed video call is never a camera you forgot about. */}
          <Show when={cameraSending()}>
            <span class="lk-callbar-mark" aria-hidden="true">
              <Icon name="video" size={13} />
            </span>
          </Show>
        </button>

        <Show when={mute()}>
          {(spec) => (
            <button
              type="button"
              class="lk-callbar-mute"
              data-tone={spec().tone}
              aria-label={spec().accessibleName}
              aria-pressed={spec().pressed ?? undefined}
              title={spec().accessibleName}
              disabled={spec().disabled}
              onClick={(event): void => {
                // The bar around it expands; this one control does not.
                event.stopPropagation();
                callActions.setMic(local()?.micOn !== true);
              }}
            >
              <Icon name={spec().icon} size={18} />
            </button>
          )}
        </Show>
      </div>
    </Show>
  );
}

/**
 * `Date.now()` as a signal that ticks once a second while `active` is true.
 *
 * A second is the resolution a running time needs — the same cadence the call
 * screen uses for its own line — and stopping it when the bar is away keeps a
 * timer from running for a call that is not collapsed, or for no call at all.
 */
function tickingClock(active: () => boolean): () => number {
  const [now, setNow] = createSignal(Date.now());
  createEffect(() => {
    if (!active()) return;
    setNow(Date.now());
    const handle = globalThis.setInterval(() => setNow(Date.now()), 1_000);
    onCleanup(() => globalThis.clearInterval(handle));
  });
  return now;
}
