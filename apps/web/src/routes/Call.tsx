// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The in-call screen (`docs/ux/calls.md` §4, §4.1, §4.1.1, §4.2).
 *
 * ## What this replaced, and why
 *
 * The screen it replaces was "laid out for the debugger": an active-speaker
 * tile and a gallery row even for a two-person call, a phase badge floating
 * over the top repeating what a line already said, and nine loose circles
 * along the bottom edge — among them a raise-hand and a share-screen button
 * that wrote a flag nobody could see, and a gear opening a settings list of
 * checkboxes that no code read. Meanwhile **Speaker**, the control people reach
 * for most on a voice call, was not on the screen at all, and the accent
 * highlight landed on whichever button was **off**, so a muted microphone read
 * as a live one.
 *
 * §4 asks for three things instead: a **portrait** for a voice call, a **fill**
 * for a video call, and **one** status line. §4.1 asks for the whole control set
 * on the screen, in one rounded bar, with no ellipsis menu in a 1x1 call.
 *
 * ## Where the decisions live
 *
 * Not here. `vitest` runs with no jsdom and no Solid transform, so nothing in
 * this file is reachable from a test — which is how the encryption chip stayed
 * hidden on every live call for nine months. So every rule is a pure module:
 *
 * - `calls/call-screen-model.ts` — which surface, what the status line says,
 *   the duration format, whether the controls may fade.
 * - `calls/call-controls.ts` — which controls exist, their labels, their two
 *   states, what is disabled.
 * - `calls/encryption-chip.ts` — whether the lock chip is shown, and the sheet.
 *
 * This file is the markup that reads them.
 *
 * ## Two modes
 *
 * - Embedded over a live call (`CallHostView`) — `embedded` is set, the host
 *   drives `callState` from the real session, and end-call + the ended overlay
 *   route through `onExit`.
 * - The standalone design route (`/call/:id`, no props) — seeds a mock roster
 *   so the layout can be looked at. It used to open on the pre-join card;
 *   there is no pre-join screen any more (§3), so it opens on the call.
 *
 * The surface carries the dark half of the shared tokens (`styles.css`), so
 * this file has no palette of its own and no colour literal.
 */

import {
  createEffect,
  createMemo,
  createSignal,
  For,
  onCleanup,
  onMount,
  Show,
  type Component,
  type JSX,
} from "solid-js";
import { useNavigate } from "@solidjs/router";

import {
  featuredParticipant,
  callActions,
  callState,
  galleryParticipants,
  qualityColour,
  type CallDevice,
  type ChatMessage,
  type Participant,
  type QualityTier,
} from "../state/call-state";
import { CallSoundController } from "../calls/CallSoundController";
import {
  encryptionSheetSentence,
  safetyNumberGroups,
  safetyNumberSpokenLabel,
  showsEncryptionChip,
} from "../calls/encryption-chip";
import {
  audioOutputName,
  callControls,
  nextAudioOutput,
  type CallControlSpec,
} from "../calls/call-controls";
import {
  ENDED_DISMISS_MS,
  callFillSource,
  callPeerLabel,
  callStatusDetail,
  callSurfaceKind,
  controlsHidden,
  elapsedMs,
  endedSummary,
  selfViewVisible,
  spokenCallDuration,
  type CallPhaseKind,
  type CallSurfaceKind,
} from "../calls/call-screen-model";
import { paths } from "./paths";
import { Icon } from "../ui/icons";
import { initialsOf, toneOf } from "../ui/Avatar";
import { currentLanguage, t } from "../l10n";

// ============================================================================
// Small shared pieces
// ============================================================================

/** Indeterminate spinner glyph. Self-contained SVG (SMIL rotation, no global
 *  keyframes); takes the surrounding text colour. */
const Spinner: Component<{ size?: number }> = (props) => {
  const size = (): number => props.size ?? 22;
  return (
    <svg
      class="lk-spinner"
      width={size()}
      height={size()}
      viewBox="0 0 24 24"
      fill="none"
      aria-hidden="true"
    >
      <circle
        cx="12"
        cy="12"
        r="9"
        stroke="currentColor"
        stroke-width="3"
        stroke-linecap="round"
        stroke-dasharray="40 60"
      >
        <animateTransform
          attributeName="transform"
          attributeType="XML"
          type="rotate"
          from="0 12 12"
          to="360 12 12"
          dur="0.8s"
          repeatCount="indefinite"
        />
      </circle>
    </svg>
  );
};

interface QualityDotProps {
  tier: QualityTier;
  /** When true, clicking the dot toggles the connection-detail popover. When
   *  false it is a passive glyph. */
  interactive?: boolean;
}

const QualityDot: Component<QualityDotProps> = (props) => (
  <span
    class="lk-call-dot"
    classList={{ interactive: props.interactive === true }}
    data-tier={qualityColour(props.tier)}
    role={props.interactive === true ? "button" : undefined}
    tabindex={props.interactive === true ? 0 : undefined}
    aria-label={t("call.quality.a11y", { tier: qualityWord(props.tier) })}
    title={t("call.quality.title", { tier: qualityWord(props.tier) })}
    onClick={(e): void => {
      if (props.interactive !== true) return;
      e.stopPropagation();
      callActions.setQualityPopoverOpen(!callState.qualityPopoverOpen());
    }}
    onKeyDown={(e): void => {
      if (props.interactive !== true) return;
      if (e.key !== "Enter" && e.key !== " ") return;
      e.preventDefault();
      e.stopPropagation();
      callActions.setQualityPopoverOpen(!callState.qualityPopoverOpen());
    }}
  />
);

function localQualityTier(): QualityTier {
  const local = callState.participants().find((p) => p.isLocal);
  return local?.quality ?? "good";
}

/** The word for a quality tier, in the interface language. */
function qualityWord(tier: QualityTier): string {
  switch (tier) {
    case "excellent":
      return t("call.quality.tier.excellent");
    case "good":
      return t("call.quality.tier.good");
    case "fair":
      return t("call.quality.tier.fair");
    case "poor":
      return t("call.quality.tier.poor");
    case "bad":
      return t("call.quality.tier.bad");
  }
}

/** A number with a fixed count of decimals, in the interface language. */
function formatNumber(value: number, decimals: number): string {
  return new Intl.NumberFormat(currentLanguage(), {
    minimumFractionDigits: decimals,
    maximumFractionDigits: decimals,
  }).format(value);
}

function formatBps(bps: number | null): string {
  if (bps === null) return "—";
  if (bps >= 1_000_000) return t("call.quality.mbps", { value: formatNumber(bps / 1_000_000, 1) });
  if (bps >= 1_000) return t("call.quality.kbps", { value: formatNumber(bps / 1_000, 0) });
  return t("call.quality.bps", { value: formatNumber(bps, 0) });
}

/**
 * The connection quality dot, and the measurements behind it.
 *
 * §4: "Connection quality stays as it is: a real measurement or nothing at
 * all." So the dot is only drawn once a round-trip time has actually been
 * sampled — before that there is no measurement, and a green dot would be an
 * invented signal bar (§1).
 */
const QualityIndicator: Component = () => (
  <Show when={callState.connectionDetail().rttMs !== null}>
    <div class="lk-call-quality">
      <QualityDot tier={localQualityTier()} interactive={true} />
      <Show when={callState.qualityPopoverOpen()}>
        <div class="lk-call-popover" role="dialog" aria-label={t("call.quality.details_a11y")}>
          <div class="lk-call-popover-head">
            <span>{qualityWord(localQualityTier())}</span>
            <span
              classList={{
                ok: callState.connectionDetail().connectionType === "direct",
                warn: callState.connectionDetail().connectionType !== "direct",
              }}
            >
              {callState.connectionDetail().connectionType === "direct"
                ? t("call.quality.direct")
                : callState.connectionDetail().connectionType === "relay"
                  ? t("call.quality.relay")
                  : "—"}
            </span>
          </div>
          <dl class="lk-call-popover-stats">
            <dt>{t("call.quality.round_trip")}</dt>
            <dd>
              {callState.connectionDetail().rttMs !== null
                ? t("call.quality.ms", {
                    value: formatNumber(callState.connectionDetail().rttMs ?? 0, 0),
                  })
                : "—"}
            </dd>
            <dt>{t("call.quality.down")}</dt>
            <dd>{formatBps(callState.connectionDetail().downBps)}</dd>
            <dt>{t("call.quality.up")}</dt>
            <dd>{formatBps(callState.connectionDetail().upBps)}</dd>
          </dl>
        </div>
      </Show>
    </div>
  </Show>
);

// ============================================================================
// EncryptionChip / EncryptionSheet — one lock and the word `Encrypted`
// (§4), with the digits one tap behind it (§5). Unchanged by this redesign
// beyond where the chip sits: it is now beside the status line rather than in
// a bar of its own.
// ============================================================================

const EncryptionChip: Component<{ onOpen: () => void }> = (props) => (
  <Show
    when={showsEncryptionChip(callState.e2ee())}
    fallback={
      // Not armed. Says so rather than claiming protection, and is not a
      // button: there is nothing to show behind it.
      <span class="lk-call-chip negotiating" role="status">
        <Icon name="lock" size={13} />
        <span>{t("call.encryption.setting_up")}</span>
      </span>
    }
  >
    <button
      type="button"
      class="lk-call-chip secure"
      aria-label={t("call.encryption.chip_a11y")}
      aria-haspopup="dialog"
      onClick={(e): void => {
        e.stopPropagation();
        props.onOpen();
      }}
    >
      <Icon name="lock" size={13} />
      <span>{t("call.encryption.chip")}</span>
    </button>
  </Show>
);

const EncryptionSheet: Component<{ peerName: string | null; onClose: () => void }> = (props) => {
  const [copied, setCopied] = createSignal(false);
  let doneButton: HTMLButtonElement | undefined;

  const number = (): string | null => callState.e2ee().shortFingerprint;

  createEffect(() => {
    doneButton?.focus();
  });
  createEffect(() => {
    const onKeyDown = (event: KeyboardEvent): void => {
      if (event.key !== "Escape") return;
      event.preventDefault();
      props.onClose();
    };
    document.addEventListener("keydown", onKeyDown);
    onCleanup(() => document.removeEventListener("keydown", onKeyDown));
  });

  let copiedTimer: number | null = null;
  onCleanup(() => {
    if (copiedTimer !== null) window.clearTimeout(copiedTimer);
  });

  const copy = (): void => {
    const value = number();
    if (value === null) return;
    void navigator.clipboard
      ?.writeText(value)
      .then(() => {
        setCopied(true);
        if (copiedTimer !== null) window.clearTimeout(copiedTimer);
        copiedTimer = window.setTimeout(() => setCopied(false), 2000);
      })
      .catch(() => {
        // A refused clipboard is not worth an error dialog: the number is on
        // screen and readable, which is what it is for.
      });
  };

  return (
    <div
      class="lk-call-modal-backdrop"
      role="dialog"
      aria-modal="true"
      aria-labelledby="lk-encryption-sheet-title"
      onClick={(e): void => {
        if (e.target === e.currentTarget) props.onClose();
      }}
    >
      <div class="lk-call-modal lk-encryption-sheet">
        <h2 id="lk-encryption-sheet-title">{t("call.encryption.title")}</h2>
        <p class="lk-encryption-sheet-lead">{encryptionSheetSentence(props.peerName)}</p>

        {/* One atomic thing to a screen reader: `role="img"` with a label that
            pauses between the groups (§8), rather than sixteen digits run
            together or four unexplained numbers. */}
        <p
          class="lk-encryption-sheet-number"
          role="img"
          aria-label={safetyNumberSpokenLabel(number())}
        >
          <For each={safetyNumberGroups(number())}>
            {(group) => <span aria-hidden="true">{group}</span>}
          </For>
        </p>

        <p class="lk-encryption-sheet-hint">{t("call.encryption.compare_hint")}</p>

        <div class="lk-encryption-sheet-actions">
          <button type="button" class="ghost" onClick={copy}>
            <Icon name="copy" size={16} />
            {copied() ? t("common.copied") : t("common.copy")}
          </button>
          <button
            type="button"
            class="primary"
            ref={(el) => {
              doneButton = el;
            }}
            onClick={() => props.onClose()}
          >
            {t("common.done")}
          </button>
        </div>
      </div>
    </div>
  );
};

// ============================================================================
// The control bar (§4.1, §4.1.1)
// ============================================================================

/**
 * One circle plus its word.
 *
 * The two states are `data-tone` and nothing else: `outline` for a live device,
 * `solid` for one that is off. §4.1.1 puts the loud state on **off**, which is
 * the correction the old row needed most — it painted the off button in the
 * accent colour, which reads as "this is on".
 *
 * The label sits under the circle in a fixed size, and the spoken name is the
 * action plus the state it will produce, so a screen-reader user is told what
 * pressing does rather than what is currently true.
 */
const ControlButton: Component<{
  spec: CallControlSpec;
  busy?: boolean;
  onPress: () => void;
}> = (props) => {
  const inert = (): boolean => props.spec.disabled || props.busy === true;
  return (
    <div class="lk-call-ctl" classList={{ end: props.spec.tone === "end" }}>
      <button
        type="button"
        class="lk-call-ctl-btn"
        data-tone={props.spec.tone}
        aria-label={props.spec.accessibleName}
        aria-pressed={props.spec.pressed ?? undefined}
        aria-busy={props.busy === true ? "true" : undefined}
        title={props.spec.accessibleName}
        disabled={inert()}
        onClick={(e): void => {
          e.stopPropagation();
          if (inert()) return;
          props.onPress();
        }}
      >
        <Show when={props.busy === true} fallback={<Icon name={props.spec.icon} size={26} />}>
          <Spinner size={26} />
        </Show>
      </button>
      <span class="lk-call-ctl-label" aria-hidden="true">
        {props.spec.label}
      </span>
    </div>
  );
};

/**
 * The row, in one rounded translucent bar floating above the bottom edge
 * (§4.1, §4.2: "WhatsApp, Telegram and FaceTime all float them in one rounded
 * container over the content, not loose on the background. That container is
 * what our screen was missing.").
 *
 * On a voice call the bar sits in the lower third under the portrait rather
 * than welded to the bottom of an empty surface — a `data-surface` the
 * stylesheet reads, so the rule is one place rather than repeated per button.
 */
const ControlBar: Component<{
  surface: CallSurfaceKind;
  /** Faded out after an idle window on a connected video call (§4). A visual
   *  state only: the bar keeps its accessible name and its buttons keep their
   *  place in the tab order, and a focus inside it wakes it (the root's
   *  `focusin`) — marking it `aria-hidden` would hide focusable controls from
   *  a screen reader while leaving them reachable by keyboard. */
  hidden: boolean;
  controls: CallControlSpec[];
  cameraBusy: boolean;
  /** The link to this call is being prepared (`docs/ux/call-links.md`
   *  section 5). */
  inviteBusy: boolean;
  onPress: (spec: CallControlSpec) => void;
}> = (props) => (
  <div
    class="lk-call-bar"
    data-surface={props.surface}
    classList={{ faded: props.hidden }}
    role="toolbar"
    aria-label={t("call.control.bar_a11y")}
  >
    <For each={props.controls}>
      {(spec) => (
        <ControlButton
          spec={spec}
          busy={
            (spec.id === "camera" && props.cameraBusy) ||
            (spec.id === "invite" && props.inviteBusy)
          }
          onPress={(): void => props.onPress(spec)}
        />
      )}
    </For>
  </div>
);

// ============================================================================
// The status line (§4)
// ============================================================================

/**
 * Name, then state or duration — never two of them at once, and never a
 * floating badge elsewhere repeating it.
 *
 * The duration is announced as a duration rather than read digit by digit
 * (§8): `12:04` read aloud is a time of day, so the visible text is hidden from
 * the reader and an `aria-label` carries the words.
 */
const CallStatus: Component<{
  name: string;
  detail: string;
  spokenDetail: string;
  /** On the voice portrait the avatar belongs to this block: §4 stacks it,
   *  the name and the line as one figure in the upper half. Omitted on the
   *  video fill and the group grid, where the status floats at the top over
   *  the picture. */
  avatar?: boolean;
  /** What `Invite` is doing, or `null` (`docs/ux/call-links.md` section 5:
   *  the wait "says what it is doing rather than only spinning"). Rendered as
   *  a second plain line rather than a banner, so it reads like the rest of
   *  the status block. */
  inviteStatus?: string | null;
  onOpenEncryption: () => void;
}> = (props) => (
  <div class="lk-call-status">
    <Show when={props.avatar === true}>
      <div class="lk-call-portrait-avatar" aria-hidden="true">
        {initialsOf(props.name)}
      </div>
    </Show>
    <h1 class="lk-call-name">{props.name}</h1>
    <div class="lk-call-state">
      <span class="lk-call-state-text" role="status" aria-label={props.spokenDetail}>
        <span aria-hidden="true">{props.detail}</span>
      </span>
      <EncryptionChip onOpen={props.onOpenEncryption} />
      <QualityIndicator />
    </div>
    <Show when={callState.notice()}>
      {(text) => (
        <p class="lk-call-notice" role="status" aria-live="polite">
          {text()}
        </p>
      )}
    </Show>
    <Show when={props.inviteStatus}>
      {(text) => (
        <p class="lk-call-notice" role="status" aria-live="polite">
          {text()}
        </p>
      )}
    </Show>
  </div>
);

// ============================================================================
// The voice portrait (§4, §4.2)
// ============================================================================

/**
 * The other person, large and centred in the upper half.
 *
 * §4.2 on the background: "Telegram animates a gradient built from the
 * contact's colour; Signal and WhatsApp blur the contact's photo ... Flat
 * surface reads as an empty page, which is what made ours look unfinished." So
 * the ground is the contact's own avatar tint, heavily blurred and darkened —
 * Lakko has no profile photos yet, so the tint and the initials are what there
 * is to blur, and the same hash picks it as everywhere else in the app, which
 * keeps one person one colour.
 *
 * The one thing §4.2 says we deliberately do not copy is an animation that
 * keeps moving for the whole call: decoration with a battery cost.
 */
const CallPortrait: Component<{ name: string; toneId: string }> = (props) => (
  <div class="lk-call-portrait" data-tone={toneOf(props.toneId)} aria-hidden="true">
    <div class="lk-call-portrait-wash">
      <span class="lk-call-portrait-blur">{initialsOf(props.name)}</span>
    </div>
  </div>
);

// ============================================================================
// The self-view (§4)
// ============================================================================

/**
 * "The self-view is a small rounded rectangle in a corner, draggable, about a
 * quarter of the width."
 *
 * Draggable because the one place a corner rectangle is guaranteed to be in the
 * way is the corner the other person's face happens to occupy. Position is kept
 * in pixels relative to the call surface and clamped to it on every move, so a
 * drag cannot park it off-screen; `null` means "the default corner", which is
 * where it returns on the next call.
 *
 * §4.1: "Flip camera does not belong in the row. It lives on the self-view
 * itself, as a small button in that rectangle's corner, where the thing it
 * changes is visible."
 */
const SelfView: Component<{
  stream: MediaStream | undefined;
  starting: boolean;
  canFlip: boolean;
  onFlip: () => void;
}> = (props) => {
  const [pos, setPos] = createSignal<{ x: number; y: number } | null>(null);
  let box: HTMLDivElement | undefined;
  let dragging: { pointerId: number; dx: number; dy: number } | null = null;

  const onPointerDown = (e: PointerEvent): void => {
    if (box === undefined) return;
    // The flip button is inside the rectangle and must stay pressable.
    if ((e.target as HTMLElement).closest("button") !== null) return;
    const rect = box.getBoundingClientRect();
    const parent = box.offsetParent as HTMLElement | null;
    const origin = parent?.getBoundingClientRect() ?? { left: 0, top: 0 };
    dragging = {
      pointerId: e.pointerId,
      dx: e.clientX - rect.left,
      dy: e.clientY - rect.top,
    };
    setPos({ x: rect.left - origin.left, y: rect.top - origin.top });
    box.setPointerCapture(e.pointerId);
    e.preventDefault();
  };

  const onPointerMove = (e: PointerEvent): void => {
    if (dragging === null || box === undefined || e.pointerId !== dragging.pointerId) return;
    const parent = box.offsetParent as HTMLElement | null;
    const origin = parent?.getBoundingClientRect();
    if (origin === undefined) return;
    const maxX = Math.max(0, origin.width - box.offsetWidth);
    const maxY = Math.max(0, origin.height - box.offsetHeight);
    setPos({
      x: clamp(e.clientX - origin.left - dragging.dx, 0, maxX),
      y: clamp(e.clientY - origin.top - dragging.dy, 0, maxY),
    });
  };

  const endDrag = (e: PointerEvent): void => {
    if (dragging === null || e.pointerId !== dragging.pointerId) return;
    box?.releasePointerCapture(e.pointerId);
    dragging = null;
  };

  return (
    <div
      class="lk-call-self"
      ref={box}
      style={
        pos() === null
          ? undefined
          : { left: `${pos()?.x ?? 0}px`, top: `${pos()?.y ?? 0}px`, right: "auto", bottom: "auto" }
      }
      onPointerDown={onPointerDown}
      onPointerMove={onPointerMove}
      onPointerUp={endDrag}
      onPointerCancel={endDrag}
    >
      <Show
        when={props.stream}
        fallback={
          <div class="lk-call-self-blank" aria-hidden="true">
            <Icon name="video" size={20} strokeWidth={1.4} />
          </div>
        }
      >
        {(stream) => (
          <video
            class="lk-call-self-video"
            autoplay
            playsinline
            muted
            aria-label={t("call.self.camera_a11y")}
            ref={(el) => {
              el.srcObject = stream();
            }}
          />
        )}
      </Show>

      <Show when={props.starting}>
        <div class="lk-call-self-overlay" role="status" aria-live="polite">
          <Spinner size={20} />
          <span>{t("call.self.starting_camera")}</span>
        </div>
      </Show>

      <Show when={props.canFlip}>
        <button
          type="button"
          class="lk-call-self-flip"
          aria-label={t("call.self.flip_a11y")}
          title={t("call.self.flip_a11y")}
          onClick={(e): void => {
            e.stopPropagation();
            props.onFlip();
          }}
        >
          <Icon name="cameraFlip" size={16} />
        </button>
      </Show>
    </div>
  );
};

function clamp(value: number, low: number, high: number): number {
  return Math.min(high, Math.max(low, value));
}

// ============================================================================
// The group roster (§9 keeps this as it is)
// ============================================================================

interface TileProps {
  participant: Participant;
  large: boolean;
  onPin?: (id: string) => void;
  remoteStreamFor?: ((participantId: string) => MediaStream | undefined) | undefined;
  localStream?: (() => MediaStream | undefined) | undefined;
}

const ParticipantTile: Component<TileProps> = (props) => {
  const isPinned = (): boolean => callState.pinnedId() === props.participant.id;
  const tileStream = (): MediaStream | undefined =>
    props.participant.isLocal
      ? props.localStream?.()
      : props.remoteStreamFor?.(props.participant.id);
  const name = (): string =>
    props.participant.displayName ||
    (props.participant.isLocal ? t("call.participant.you") : t("call.participant.unnamed"));
  return (
    <div
      class="lk-call-tile"
      classList={{
        large: props.large,
        speaking: props.participant.speaking,
        pinnable: !props.participant.isLocal && props.onPin !== undefined,
      }}
      data-tone={toneOf(props.participant.id)}
      onClick={(): void => {
        if (props.onPin !== undefined && !props.participant.isLocal) {
          props.onPin(isPinned() ? "" : props.participant.id);
        }
      }}
    >
      <Show
        when={props.participant.camOn}
        fallback={
          <div class="lk-call-tile-avatar" aria-hidden="true">
            {initialsOf(name())}
          </div>
        }
      >
        {/* A real <video> bound to the stream. Muted — audio plays through the
            dedicated hidden <audio> sinks, so this element never doubles the
            audio. The mock path passes no stream and falls through. */}
        <Show
          when={tileStream()}
          fallback={
            <div class="lk-call-tile-blank" aria-hidden="true">
              <Icon name="video" size={props.large ? 36 : 24} strokeWidth={1.4} />
            </div>
          }
        >
          {(stream) => (
            <video
              class="lk-call-tile-video"
              classList={{ mirrored: props.participant.isLocal }}
              autoplay
              playsinline
              muted
              ref={(el) => {
                el.srcObject = stream();
              }}
            />
          )}
        </Show>
      </Show>

      <Show when={props.participant.isLocal && callState.videoPublishState() === "starting"}>
        <div class="lk-call-tile-overlay" role="status" aria-live="polite">
          <Spinner size={props.large ? 30 : 24} />
          <span>{t("call.self.starting_camera")}</span>
        </div>
      </Show>

      <Show when={!props.participant.micOn && !props.participant.isLocal}>
        <div class="lk-call-tile-mute" aria-hidden="true">
          <Icon name="micOff" size={props.large ? 22 : 16} />
        </div>
      </Show>

      <div class="lk-call-tile-strip">
        <Show when={!props.participant.micOn}>
          <span
            class="lk-call-tile-muted"
            aria-label={t("call.participant.muted_a11y")}
            title={t("call.participant.muted")}
          >
            <Icon name="micOff" size={14} />
          </span>
        </Show>
        <span class="lk-call-tile-name">{name()}</span>
        <QualityDot tier={props.participant.quality} />
        <Show when={isPinned()}>
          <span class="lk-call-tile-pin" title={t("call.participant.pinned")}>
            <Icon name="pin" size={14} />
          </span>
        </Show>
      </div>
    </div>
  );
};

const GroupStage: Component<{
  remoteStreamFor?: ((participantId: string) => MediaStream | undefined) | undefined;
  localStream?: (() => MediaStream | undefined) | undefined;
}> = (props) => (
  <>
    <Show when={featuredParticipant()}>
      {(featured) => (
        <div class="lk-call-stage">
          <ParticipantTile
            participant={featured()}
            large={true}
            onPin={(id): void => callActions.pin(id === "" ? null : id)}
            remoteStreamFor={props.remoteStreamFor}
            localStream={props.localStream}
          />
        </div>
      )}
    </Show>
    <div class="lk-call-gallery" classList={{ equal: featuredParticipant() === null }}>
      <For each={galleryParticipants()}>
        {(p) => (
          <ParticipantTile
            participant={p}
            large={false}
            onPin={(id): void => callActions.pin(id === "" ? null : id)}
            remoteStreamFor={props.remoteStreamFor}
            localStream={props.localStream}
          />
        )}
      </For>
    </div>
  </>
);

// ============================================================================
// The group overflow: participants and chat (§4.1 allows exactly one)
// ============================================================================

const ParticipantList: Component = () => (
  <Show when={callState.participantListOpen()}>
    <aside class="lk-call-drawer" aria-label={t("call.control.participants")}>
      <div class="lk-call-drawer-head">
        <h2>{t("call.participants.title", { count: callState.participants().length })}</h2>
        <button
          type="button"
          class="lk-icon-button"
          aria-label={t("call.participants.close_a11y")}
          onClick={(): void => callActions.setParticipantListOpen(false)}
        >
          <Icon name="close" size={18} />
        </button>
      </div>
      <ul class="lk-call-people">
        <For each={callState.participants()}>
          {(p) => (
            <li class="lk-call-person">
              <QualityDot tier={p.quality} />
              <span class="lk-call-person-name">
                {p.displayName ||
                  (p.isLocal ? t("call.participant.you") : t("call.participant.unnamed"))}
              </span>
              <Show when={!p.micOn}>
                <span
                  class="lk-call-tile-muted"
                  title={t("call.participant.muted")}
                  aria-label={t("call.participant.muted")}
                >
                  <Icon name="micOff" size={14} />
                </span>
              </Show>
            </li>
          )}
        </For>
      </ul>
    </aside>
  </Show>
);

const ChatMessageRow: Component<{ msg: ChatMessage }> = (props) => {
  const isSystem = (): boolean => props.msg.kind === "system";
  const time = (): string => {
    const d = new Date(props.msg.ts);
    const hh = `${d.getHours()}`.padStart(2, "0");
    const mm = `${d.getMinutes()}`.padStart(2, "0");
    return `${hh}:${mm}`;
  };
  return (
    <Show when={!isSystem()} fallback={<div class="lk-call-msg-sys">{props.msg.text}</div>}>
      <div class="lk-call-msg">
        <div class="lk-call-msg-meta">
          <span class="lk-call-msg-sender">{props.msg.senderName}</span>
          <span>{time()}</span>
        </div>
        <div class="lk-call-msg-text">{props.msg.text}</div>
      </div>
    </Show>
  );
};

const ChatPanel: Component = () => {
  let scrollRef: HTMLDivElement | undefined;
  let inputRef: HTMLInputElement | undefined;

  createEffect((): void => {
    void callState.chatMessages().length;
    queueMicrotask((): void => {
      if (scrollRef !== undefined) scrollRef.scrollTop = scrollRef.scrollHeight;
    });
  });

  createEffect((): void => {
    if (callState.chatOpen() && inputRef !== undefined) {
      queueMicrotask((): void => inputRef?.focus());
    }
  });

  return (
    <Show when={callState.chatOpen()}>
      <aside class="lk-call-drawer lk-call-chat" aria-label={t("call.chat.a11y")}>
        <header class="lk-call-drawer-head">
          <h2>{t("call.control.chat")}</h2>
          <button
            type="button"
            class="lk-icon-button"
            aria-label={t("call.chat.close_a11y")}
            onClick={(): void => callActions.setChatOpen(false)}
          >
            <Icon name="close" size={18} />
          </button>
        </header>

        <div class="lk-call-chat-log" ref={scrollRef}>
          <Show
            when={callState.chatMessages().length > 0}
            fallback={
              <div class="lk-call-chat-empty">{t("call.chat.empty")}</div>
            }
          >
            <For each={callState.chatMessages()}>
              {(msg): JSX.Element => <ChatMessageRow msg={msg} />}
            </For>
          </Show>
        </div>

        <form
          class="lk-composer lk-call-chat-form"
          onSubmit={(e): void => {
            e.preventDefault();
            callActions.sendChatMessage(callState.composeText());
          }}
        >
          <input
            ref={inputRef}
            class="lk-composer-input"
            type="text"
            placeholder={t("chat.composer.placeholder")}
            aria-label={t("chat.composer.placeholder")}
            autocomplete="off"
            value={callState.composeText()}
            onInput={(e): void => callActions.setComposeText(e.currentTarget.value)}
          />
          <button
            type="submit"
            class="lk-icon-button lk-send"
            disabled={callState.composeText().trim().length === 0}
            aria-label={t("chat.composer.send")}
            title={t("chat.composer.send")}
          >
            <Icon name="send" size={20} />
          </button>
        </form>
      </aside>
    </Show>
  );
};

// ============================================================================
// The ended screen (§4: "`Call ended` and the duration, the screen closes
// itself after 2 s")
// ============================================================================

const CallEndedOverlay: Component<{ summary: string; onDismiss: () => void }> = (props) => {
  let timer: number | null = null;
  onMount(() => {
    timer = window.setTimeout(props.onDismiss, ENDED_DISMISS_MS);
  });
  onCleanup(() => {
    if (timer !== null) window.clearTimeout(timer);
  });
  return (
    <div class="lk-call-ended" role="status" aria-live="polite">
      <p>{props.summary}</p>
    </div>
  );
};

// ============================================================================
// Devices
// ============================================================================

/** Enumerate the OS's capture / playback devices into the shared store. Labels
 *  populate only once a capture permission has been granted, which during a
 *  live call it has. Drives the Speaker control and the self-view's flip
 *  button, which is why it now runs from the call screen rather than from a
 *  settings panel that no longer exists. */
async function refreshCallDevices(): Promise<void> {
  const md = typeof navigator !== "undefined" ? navigator.mediaDevices : undefined;
  if (!md?.enumerateDevices) return;
  try {
    const infos = await md.enumerateDevices();
    const devices: CallDevice[] = infos
      .filter((d) => d.kind === "audioinput" || d.kind === "videoinput" || d.kind === "audiooutput")
      .map((d) => ({ id: d.deviceId, label: d.label, kind: d.kind as CallDevice["kind"] }));
    callActions.setAvailableDevices(devices);
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("call ui: enumerateDevices failed", err);
  }
}

/** Whether this browser can move a call's sound to a chosen output. Absent in
 *  Firefox and in Safari on iOS, where §4.1 says to omit Speaker entirely
 *  rather than show a button that does nothing. */
function canSelectAudioOutput(): boolean {
  return (
    typeof HTMLMediaElement !== "undefined" &&
    typeof HTMLMediaElement.prototype.setSinkId === "function"
  );
}

// ============================================================================
// Mock data — the standalone design route
// ============================================================================

function seedMockCall(): () => void {
  callActions.setE2ee({ active: true, shortFingerprint: "3F7A B2C1 9E0D 4415" });
  callActions.setConnectionDetail({
    rttMs: 45,
    downBps: 720_000,
    upBps: 380_000,
    connectionType: "direct",
  });
  callActions.setParticipants([
    {
      id: "local",
      displayName: t("call.participant.you"),
      isLocal: true,
      micOn: true,
      camOn: true,
      quality: "excellent",
      speaking: false,
    },
    {
      id: "leaf-1",
      displayName: "Anna Weber",
      isLocal: false,
      micOn: true,
      camOn: false,
      quality: "good",
      speaking: true,
    },
  ]);
  callActions.setPhase({ kind: "calling" });
  const timer = window.setTimeout((): void => {
    callActions.setPhase({ kind: "connected" });
  }, 900);
  return (): void => window.clearTimeout(timer);
}

// ============================================================================
// Top-level route
// ============================================================================

const Call: Component<{
  embedded?: boolean;
  onExit?: () => void;
  /** A group call (SFU or call link): the group surface from the first
   *  second, whoever has joined yet (§4.4). */
  group?: boolean;
  /** The other person, for the portrait, the status line and the encryption
   *  sheet's one sentence (§5). Omitted on a group call, where there is no
   *  "other person". */
  peerName?: string | null;
  /** What the portrait's colour is hashed from, so one person is one colour
   *  everywhere in the app. Falls back to the name. */
  peerId?: string | null;
  /** SFU group-call only: resolve a roster participant id to its live remote
   *  `MediaStream`. */
  remoteStreamFor?: ((participantId: string) => MediaStream | undefined) | undefined;
  /** The local capture `MediaStream`, for the self-view. */
  localStream?: (() => MediaStream | undefined) | undefined;
  /** Hand out a link to this call (`docs/ux/call-links.md` section 3). The
   *  `Invite` control exists only where this is given: a call in a
   *  conversation is reached through that conversation, not through a link. */
  onInvite?: (() => void) | undefined;
  /** The link is being prepared — a network round trip and a proof of work. */
  inviteBusy?: boolean;
  /** One line on what `Invite` is doing, or `null`. */
  inviteStatus?: string | null;
  /**
   * Put the call away without ending it (`docs/ux/calls.md` section 4.5).
   *
   * Absent when there is nothing the bar could show in its place — the host
   * asks the shared visibility rule, so this screen carries no second list of
   * phases. Absent here means the chevron is not drawn and `Esc` does nothing,
   * which is what keeps an ENDED call from being put away into a bar that does
   * not take ended calls, leaving its overlay and its End button unreachable.
   */
  onCollapse?: (() => void) | undefined;
}> = (props) => {
  const navigate = useNavigate();
  const [encryptionSheetOpen, setEncryptionSheetOpen] = createSignal(false);
  const [now, setNow] = createSignal(Date.now());
  const [lastInteraction, setLastInteraction] = createSignal(Date.now());

  // One second is the resolution the running time needs, and it is also what
  // drives the controls' idle fade — two timers for one cadence would drift
  // against each other for no gain.
  onMount(() => {
    const tick = window.setInterval(() => setNow(Date.now()), 1_000);
    onCleanup(() => window.clearInterval(tick));
  });

  // Devices: once at mount (a live call has already been granted capture, so
  // the labels are real) and again whenever one is plugged in or removed.
  onMount(() => {
    void refreshCallDevices();
    const md = typeof navigator !== "undefined" ? navigator.mediaDevices : undefined;
    if (!md?.addEventListener) return;
    const handler = (): void => void refreshCallDevices();
    md.addEventListener("devicechange", handler);
    onCleanup(() => md.removeEventListener("devicechange", handler));
  });

  // A call that loses its encryption (it ended, or the slot was handed to
  // another call) must not leave the previous safety number on screen behind a
  // sheet the user has to notice and close.
  createEffect(() => {
    if (!showsEncryptionChip(callState.e2ee())) setEncryptionSheetOpen(false);
  });

  const sounds = new CallSoundController();

  onMount(() => {
    // Embedded: the host already drove `callState` to a live phase before
    // mounting us, so resetting would wipe it. Standalone: seed the design
    // route's mock call.
    if (props.embedded !== true) {
      callActions.reset();
      onCleanup(seedMockCall());
    }
  });
  onCleanup(() => {
    sounds.dispose();
    if (props.embedded !== true) callActions.reset();
  });

  // Phase -> sound. Mirrors the same trigger map on iOS / Android.
  createEffect(() => {
    switch (callState.phase().kind) {
      case "idle":
        sounds.stopAll();
        break;
      case "calling":
      case "connecting":
        sounds.startLoop("ringback-outgoing");
        break;
      case "connected":
        sounds.stopLoop("ringback-outgoing");
        sounds.playOnce("join");
        break;
      case "reconnecting":
        sounds.stopLoop("ringback-outgoing");
        break;
      case "ended":
        sounds.stopAll();
        sounds.playOnce("leave");
        break;
    }
  });

  // Peer arrival / departure cues + chat system events.
  let lastRemoteIds: ReadonlyArray<string> = [];
  createEffect(() => {
    const remote = callState.participants().filter((p) => !p.isLocal);
    const remoteIds = remote.map((p) => p.id);
    if (callState.phase().kind === "connected" && lastRemoteIds.length > 0) {
      for (const p of remote) {
        if (!lastRemoteIds.includes(p.id)) {
          sounds.playOnce("join");
          callActions.addSystemEvent(
            t("call.chat.joined", {
              name: p.displayName || t("call.participant.unnamed"),
            }),
          );
        }
      }
      const present = new Set(remoteIds);
      for (const prevId of lastRemoteIds) {
        if (!present.has(prevId)) {
          sounds.playOnce("leave");
          callActions.addSystemEvent(t("call.chat.left"));
        }
      }
    }
    lastRemoteIds = remoteIds;
  });

  // ----- derived state ------------------------------------------------------

  const local = (): Participant | null => callState.participants().find((p) => p.isLocal) ?? null;
  const remotes = createMemo(() => callState.participants().filter((p) => !p.isLocal));
  const phaseKind = (): CallPhaseKind => callState.phase().kind;

  const remoteCameraOn = (): boolean => remotes()[0]?.camOn === true;
  const localCameraOn = (): boolean => local()?.camOn === true;

  const surface = createMemo<CallSurfaceKind>(() =>
    callSurfaceKind({
      groupCall: props.group === true,
      remoteCount: remotes().length,
      remoteCameraOn: remoteCameraOn(),
      localCameraOn: localCameraOn(),
    }),
  );

  /** Whose picture the video surface is showing. §4.1: a person who turns their
   *  camera on gets the video layout even when the other side has not, and the
   *  only picture that exists then is their own. */
  const fill = createMemo(() => callFillSource(surface(), remoteCameraOn(), localCameraOn()));

  /** Who is on the call, by the shared rule — lifted out of this view when the
   *  collapsed bar was built, so the bar can show "the same label the call
   *  screen uses" (§4.5) from one function rather than a second copy. */
  const peerLabel = createMemo((): string =>
    callPeerLabel({
      surface: surface(),
      peerName: props.peerName,
      participantCount: callState.participants().length,
      soleRemoteName: remotes()[0]?.displayName,
    }),
  );

  const toneId = (): string => props.peerId ?? props.peerName ?? remotes()[0]?.id ?? "peer";

  const statusDetail = createMemo(() =>
    callStatusDetail({
      phase: phaseKind(),
      connectedAtMs: callState.connectedAt(),
      nowMs: now(),
    }),
  );

  /** §8: the running time is announced as a duration, not read digit by
   *  digit. Every other state already reads as words. */
  const spokenDetail = createMemo(() =>
    phaseKind() === "connected"
      ? spokenCallDuration(elapsedMs(callState.connectedAt(), now()))
      : statusDetail(),
  );

  const audioOutputs = createMemo(() =>
    callState
      .settings()
      .availableDevices.filter((d) => d.kind === "audiooutput")
      .map((d) => ({ id: d.id, label: d.label })),
  );
  const nextOutput = createMemo(() =>
    nextAudioOutput(audioOutputs(), callState.settings().selectedAudioOutput),
  );
  const videoInputs = createMemo(() =>
    callState.settings().availableDevices.filter((d) => d.kind === "videoinput"),
  );

  const controls = createMemo(() =>
    callControls({
      surface: surface(),
      phase: phaseKind(),
      micOn: local()?.micOn === true,
      cameraOn: local()?.camOn === true,
      cameraBusy: callState.videoPublishState() === "starting",
      canSelectAudioOutput: canSelectAudioOutput(),
      nextAudioOutputLabel: audioOutputName(nextOutput()),
      participantsOpen: callState.participantListOpen(),
      chatOpen: callState.chatOpen(),
      canInvite: props.onInvite !== undefined,
      inviteBusy: props.inviteBusy === true,
    }),
  );

  const barHidden = createMemo(() =>
    controlsHidden({
      surface: surface(),
      phase: phaseKind(),
      msSinceInteraction: now() - lastInteraction(),
    }),
  );

  // ----- actions ------------------------------------------------------------

  function press(spec: CallControlSpec): void {
    switch (spec.id) {
      case "speaker": {
        const next = nextOutput();
        if (next !== null) callActions.setSelectedAudioOutput(next.id);
        return;
      }
      case "mic":
        callActions.setMic(local()?.micOn !== true);
        return;
      case "camera":
        callActions.setCamera(local()?.camOn !== true);
        return;
      case "participants":
        callActions.setParticipantListOpen(!callState.participantListOpen());
        return;
      case "chat":
        callActions.setChatOpen(!callState.chatOpen());
        return;
      case "invite":
        props.onInvite?.();
        return;
      case "end":
        callActions.endCall();
        return;
    }
  }

  /** Cycle to the next camera. The store's `selectedVideoInput` drives the
   *  in-call device-switch effect in the call host, which calls
   *  `switchVideoInput` + `replaceTrack` (same kind, no renegotiation) — so
   *  this reuses the shipped path rather than re-capturing here. */
  function flipCamera(): void {
    const cams = videoInputs().filter((d) => d.id.length > 0);
    if (cams.length < 2) return;
    const current = callState.settings().selectedVideoInput;
    const at = current === null ? 0 : Math.max(0, cams.findIndex((c) => c.id === current));
    const next = cams[(at + 1) % cams.length];
    if (next !== undefined) callActions.setSelectedVideoInput(next.id);
  }

  /** Any interaction wakes the faded bar (§4: "come back on a tap"). */
  const wake = (): void => {
    setLastInteraction(Date.now());
  };

  const exit = (): void => {
    if (props.embedded === true) props.onExit?.();
    else navigate(paths.home);
  };

  /**
   * `Esc` puts the call away (§4.5's web row: "the same chevron, or `Esc`").
   *
   * Not while the encryption sheet is open: the sheet owns `Esc` (it is a
   * dialog, and closing it is what `Esc` means there), and both listeners sit
   * on `document` in bubble phase, so which one runs first depends on mount
   * order rather than on anything meaningful. Asking the state directly is
   * order-independent.
   *
   * The participant list and the chat panel are deliberately NOT treated the
   * same way: neither closes on `Esc` today, so guarding on them would make the
   * key do nothing at all while one is open. They are panels inside the call and
   * keep their state for when the person comes back.
   */
  createEffect(() => {
    const collapse = props.onCollapse;
    if (collapse === undefined) return;
    const onKeyDown = (event: KeyboardEvent): void => {
      if (event.key !== "Escape") return;
      if (encryptionSheetOpen()) return;
      event.preventDefault();
      collapse();
    };
    document.addEventListener("keydown", onKeyDown);
    onCleanup(() => document.removeEventListener("keydown", onKeyDown));
  });

  return (
    <div
      class="lk-call"
      data-surface={surface()}
      data-phase={phaseKind()}
      data-notice={callState.notice() !== null ? "" : undefined}
      onPointerMove={wake}
      onPointerDown={(): void => {
        wake();
        if (callState.qualityPopoverOpen()) callActions.setQualityPopoverOpen(false);
      }}
      onKeyDown={wake}
      onFocusIn={wake}
    >
      {/* Put the call away, top left — §4.5's web collapse affordance, and the
          same chevron iOS and Android use. Above the ground, the video fill and
          the status block so a picture cannot bury it; the drawers (11) and the
          ended overlay (12) still sit over it, which is right — a drawer is a
          thing to close, and the chevron is absent for an ended call anyway.
          Outside the control row on purpose: it is not one of §4.1's call
          controls, it is a way out of the screen. */}
      <Show when={props.onCollapse}>
        {(collapse) => (
          <button
            type="button"
            class="lk-call-collapse"
            aria-label={t("call.collapse_a11y")}
            title={t("call.collapse_title")}
            onClick={(event): void => {
              event.stopPropagation();
              collapse()();
            }}
          >
            <Icon name="chevron" size={22} />
          </button>
        )}
      </Show>

      {/* The ground. A voice call gets the other person's colour, blurred and
          darkened (§4.2); a video call gets the other person. */}
      <Show when={surface() !== "group"}>
        <CallPortrait name={peerLabel()} toneId={toneId()} />
      </Show>

      {/* The picture, edge to edge. `object-fit: cover` in the stylesheet, so a
          portrait phone camera crops rather than letterboxing into the black
          bars that read as an empty box. Own camera mirrored, the other
          person's never. */}
      <Show when={fill() === "remote"}>
        <Show when={props.remoteStreamFor?.(remotes()[0]?.id ?? "")}>
          {(stream) => (
            <video
              class="lk-call-fill"
              autoplay
              playsinline
              muted
              aria-label={t("call.fill.remote_camera_a11y", { name: peerLabel() })}
              ref={(el) => {
                el.srcObject = stream();
              }}
            />
          )}
        </Show>
      </Show>
      <Show when={fill() === "local"}>
        <Show when={props.localStream?.()}>
          {(stream) => (
            <video
              class="lk-call-fill mirrored"
              autoplay
              playsinline
              muted
              aria-label={t("call.self.camera_a11y")}
              ref={(el) => {
                el.srcObject = stream();
              }}
            />
          )}
        </Show>
      </Show>

      <CallStatus
        name={peerLabel()}
        detail={statusDetail()}
        spokenDetail={spokenDetail()}
        avatar={surface() === "voice"}
        inviteStatus={props.inviteStatus ?? null}
        onOpenEncryption={(): void => {
          setEncryptionSheetOpen(true);
        }}
      />

      <Show when={surface() === "group"}>
        <main class="lk-call-main">
          <GroupStage remoteStreamFor={props.remoteStreamFor} localStream={props.localStream} />
        </main>
      </Show>

      <Show when={selfViewVisible(surface(), localCameraOn(), fill())}>
        <SelfView
          stream={props.localStream?.()}
          starting={callState.videoPublishState() === "starting"}
          canFlip={videoInputs().length >= 2}
          onFlip={flipCamera}
        />
      </Show>

      <ControlBar
        surface={surface()}
        hidden={barHidden()}
        controls={controls()}
        cameraBusy={callState.videoPublishState() === "starting"}
        inviteBusy={props.inviteBusy === true}
        onPress={press}
      />

      <ParticipantList />
      <ChatPanel />

      <Show when={encryptionSheetOpen()}>
        <EncryptionSheet
          peerName={props.peerName ?? remotes()[0]?.displayName ?? null}
          onClose={(): void => {
            setEncryptionSheetOpen(false);
          }}
        />
      </Show>

      <Show when={phaseKind() === "ended"}>
        <CallEndedOverlay
          summary={endedSummary(callState.connectedAt(), endedAtMs(callState.phase()))}
          onDismiss={exit}
        />
      </Show>
    </div>
  );
};

/** When the call ended, or now if the phase carries no moment (it always
 *  does; the fallback keeps the duration from going negative rather than
 *  asserting). */
function endedAtMs(phase: ReturnType<typeof callState.phase>): number {
  return phase.kind === "ended" ? phase.endedAt : Date.now();
}

export default Call;
