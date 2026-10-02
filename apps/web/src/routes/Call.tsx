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
  callActions,
  callState,
  qualityColour,
  type CallDevice,
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
import { callControls, type CallControlSpec } from "../calls/call-controls";
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
import { callBarPhase } from "../calls/call-bar-model";
import { callSettingsSections, type SettingsSectionKind } from "../calls/call-settings";
import {
  gridLayout,
  gridPage,
  groupLayoutKind,
  stagePerson,
  stageSize,
  stripMetrics,
  stripPeople,
} from "../calls/group-call-layout";
import { SpeakingMonitor } from "../calls/speaking-monitor";
import { uiRules } from "../chat/ui-rules";
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
// Call settings (§4.1): which microphone, camera and speaker the call uses
// ============================================================================

/** Whether this browser can move a call's sound to a chosen output. Absent in
 *  Firefox and in Safari on iOS, where the speaker section is left out rather
 *  than shown as a list whose rows change nothing. */
function canSelectAudioOutput(): boolean {
  return (
    typeof HTMLMediaElement !== "undefined" &&
    typeof HTMLMediaElement.prototype.setSinkId === "function"
  );
}

/**
 * The settings sheet. The app's own radio lists, never a `<select>`: on a
 * phone that opens the system's picker. A choice applies at once; the call
 * host already follows the store's selected devices for every kind of call.
 */
const CallSettingsSheet: Component<{ onClose: () => void }> = (props) => {
  let doneButton: HTMLButtonElement | undefined;
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

  const sections = createMemo(() => {
    const s = callState.settings();
    return callSettingsSections(
      s.availableDevices,
      {
        audioInput: s.selectedAudioInput,
        videoInput: s.selectedVideoInput,
        audioOutput: s.selectedAudioOutput,
      },
      canSelectAudioOutput(),
    );
  });

  const choose = (kind: SettingsSectionKind, id: string | null): void => {
    if (kind === "microphone") callActions.setSelectedAudioInput(id);
    else if (kind === "camera") callActions.setSelectedVideoInput(id);
    else callActions.setSelectedAudioOutput(id);
  };

  return (
    <div
      class="lk-call-modal-backdrop"
      role="dialog"
      aria-modal="true"
      aria-labelledby="lk-call-settings-title"
      onClick={(e): void => {
        if (e.target === e.currentTarget) props.onClose();
      }}
    >
      <div class="lk-call-modal lk-call-settings">
        <h2 id="lk-call-settings-title">{t("call.settings.title")}</h2>
        <Show
          when={sections().length > 0}
          fallback={<p class="lk-call-settings-empty">{t("call.settings.no_devices")}</p>}
        >
          <div class="lk-call-settings-body">
            <For each={sections()}>
              {(section) => (
                <section>
                  <h3 id={`lk-call-settings-${section.kind}`}>{section.title}</h3>
                  <div role="radiogroup" aria-labelledby={`lk-call-settings-${section.kind}`}>
                    <For each={section.options}>
                      {(option) => (
                        <button
                          type="button"
                          role="radio"
                          aria-checked={option.selected}
                          class="lk-call-menu-item lk-call-settings-option"
                          onClick={(): void => choose(section.kind, option.id)}
                        >
                          <span class="lk-ellipsis">{option.label}</span>
                          <Show when={option.selected}>
                            <Icon name="check" size={18} />
                          </Show>
                        </button>
                      )}
                    </For>
                  </div>
                </section>
              )}
            </For>
          </div>
        </Show>
        <div class="lk-encryption-sheet-actions">
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
    style={{ "--call-ctl-count": String(props.controls.length) }}
    role="toolbar"
    aria-label={t("call.control.bar_a11y")}
  >
    <For each={props.controls}>
      {(spec) => (
        <ControlButton
          spec={spec}
          busy={
            (spec.id === "camera" && props.cameraBusy) ||
            (spec.id === "participants" && props.inviteBusy)
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

/** Whether a tile shows its connection: fair or worse only. */
function connectionIsWorthShowing(tier: QualityTier): boolean {
  return tier === "fair" || tier === "poor" || tier === "bad";
}

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
        {/* Section 7.2: a connection is drawn only when it is fair or worse.
            A dot on every healthy tile says nothing, and for other people it
            was never a measurement at all. */}
        <Show when={connectionIsWorthShowing(props.participant.quality)}>
          <QualityDot tier={props.participant.quality} />
        </Show>
        <Show when={isPinned()}>
          <span class="lk-call-tile-pin" title={t("call.participant.pinned")}>
            <Icon name="pin" size={14} />
          </span>
        </Show>
      </div>
    </div>
  );
};

/**
 * The tiles of a group call (`docs/ux/group-call-grid.md` sections 3 to 6).
 *
 * Two layouts and no scrolling. **Grid**: everyone the same size, filling the
 * space between the status line and the control bar; more people than a page
 * holds means pages. **Focus**: the person speaking, or the pinned one, large
 * in the middle, and everyone else in a strip above.
 *
 * The geometry and the choice of the stage are the shared rules
 * (`calls/group-call-layout.ts`); this component measures its own area and
 * draws the answer. Tiles are keyed by participant id, not by the roster
 * object: the store replaces a person's object whenever anything about them
 * changes, and a tile rebuilt on every change of the speaking ring would
 * restart its video each time.
 */
const GroupArea: Component<{
  remoteStreamFor?: ((participantId: string) => MediaStream | undefined) | undefined;
  localStream?: (() => MediaStream | undefined) | undefined;
}> = (props) => {
  let area: HTMLDivElement | undefined;
  const [size, setSize] = createSignal({ width: 0, height: 0 });
  const [page, setPage] = createSignal(0);

  onMount(() => {
    if (area === undefined || typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver((entries) => {
      const rect = entries[0]?.contentRect;
      if (rect === undefined) return;
      setSize({ width: Math.floor(rect.width), height: Math.floor(rect.height) });
    });
    observer.observe(area);
    onCleanup(() => observer.disconnect());
  });

  const people = (): Participant[] => callState.participants();
  const personById = (id: string): Participant | undefined => people().find((p) => p.id === id);
  const kind = createMemo(() =>
    groupLayoutKind(callState.layoutChoice(), callState.pinnedId(), people()),
  );

  const grid = createMemo(() => {
    const rules = uiRules();
    if (rules === null) return null;
    return gridLayout(rules, people().length, size().width, size().height);
  });
  const shown = createMemo(() => gridPage(people(), grid()?.perPage ?? 0, page()));
  const gridIds = createMemo(() => shown().tiles.map((p) => p.id), undefined, {
    equals: (a, b) => a.length === b.length && a.every((id, i) => id === b[i]),
  });

  const stageId = createMemo((): string | null => {
    const rules = uiRules();
    if (rules === null) return null;
    return stagePerson(rules, people(), callState.pinnedId(), callState.speech())?.id ?? null;
  });
  const stripIds = createMemo(
    () => stripPeople(people(), personById(stageId() ?? "") ?? null).map((p) => p.id),
    undefined,
    { equals: (a, b) => a.length === b.length && a.every((id, i) => id === b[i]) },
  );
  const strip = createMemo(() => stripMetrics(size().width));
  const stage = createMemo(() => {
    const rules = uiRules();
    if (rules === null) return { width: 0, height: 0 };
    return stageSize(rules, size().width, size().height);
  });

  // A horizontal swipe turns the page (section 4.5). A move that is mostly
  // vertical, or short, is not a swipe, so a tap on a tile still pins it.
  let swipeFrom: { readonly x: number; readonly y: number } | null = null;
  let swiped = false;
  const onSwipeStart = (event: PointerEvent): void => {
    swiped = false;
    swipeFrom = shown().pages > 1 ? { x: event.clientX, y: event.clientY } : null;
  };
  const onSwipeEnd = (event: PointerEvent): void => {
    const from = swipeFrom;
    swipeFrom = null;
    if (from === null) return;
    const dx = event.clientX - from.x;
    const dy = event.clientY - from.y;
    if (Math.abs(dx) < 48 || Math.abs(dx) < 2 * Math.abs(dy)) return;
    swiped = true;
    const next = shown().page + (dx < 0 ? 1 : -1);
    setPage(Math.min(shown().pages - 1, Math.max(0, next)));
  };

  const togglePin = (id: string): void => {
    // The click that ends a swipe is not a tap on a tile.
    if (swiped) {
      swiped = false;
      return;
    }
    callActions.pin(callState.pinnedId() === id ? null : id);
  };
  const tile = (id: string, large: boolean): JSX.Element => (
    <Show when={personById(id)}>
      {(p) => (
        <ParticipantTile
          participant={p()}
          large={large}
          onPin={togglePin}
          remoteStreamFor={props.remoteStreamFor}
          localStream={props.localStream}
        />
      )}
    </Show>
  );

  return (
    <main
      class="lk-call-main"
      data-layout={kind()}
      ref={area}
      onPointerDown={onSwipeStart}
      onPointerUp={onSwipeEnd}
      onPointerCancel={(): void => {
        swipeFrom = null;
      }}
    >
      <Show when={kind() === "grid"}>
        <Show when={grid()}>
          {(g) => (
            <div
              class="lk-call-grid"
              style={{
                width: `${g().cols * g().tileWidth + Math.max(0, g().cols - 1) * g().gap}px`,
                gap: `${g().gap}px`,
              }}
            >
              <For each={gridIds()}>
                {(id) => (
                  <div
                    class="lk-call-cell"
                    style={{ width: `${g().tileWidth}px`, height: `${g().tileHeight}px` }}
                  >
                    {tile(id, false)}
                  </div>
                )}
              </For>
            </div>
          )}
        </Show>
        <Show when={shown().pages > 1}>
          <button
            type="button"
            class="lk-call-page-arrow prev"
            aria-label={t("call.grid.prev_a11y")}
            disabled={shown().page === 0}
            onClick={(): void => void setPage(Math.max(0, shown().page - 1))}
          >
            <Icon name="chevron" size={20} />
          </button>
          <button
            type="button"
            class="lk-call-page-arrow next"
            aria-label={t("call.grid.next_a11y")}
            disabled={shown().page >= shown().pages - 1}
            onClick={(): void => void setPage(Math.min(shown().pages - 1, shown().page + 1))}
          >
            <Icon name="chevron" size={20} />
          </button>
          <div class="lk-call-pages" role="tablist">
            <For each={Array.from({ length: shown().pages }, (_, i) => i)}>
              {(i) => (
                <button
                  type="button"
                  role="tab"
                  class="lk-call-page-dot"
                  aria-selected={shown().page === i}
                  aria-label={t("call.grid.page_a11y", {
                    page: String(i + 1),
                    total: String(shown().pages),
                  })}
                  onClick={(): void => void setPage(i)}
                />
              )}
            </For>
          </div>
        </Show>
      </Show>

      <Show when={kind() === "focus"}>
        <div class="lk-call-strip" style={{ gap: `${strip().gap}px`, height: `${strip().tileHeight}px` }}>
          <For each={stripIds()}>
            {(id) => (
              <div
                class="lk-call-cell"
                style={{ width: `${strip().tileWidth}px`, height: `${strip().tileHeight}px` }}
              >
                {tile(id, false)}
              </div>
            )}
          </For>
        </div>
        <Show when={stageId()}>
          {(id) => (
            <div
              class="lk-call-cell lk-call-stage"
              style={{
                width: `${stage().width}px`,
                height: `${stage().height}px`,
                "margin-top": `${strip().gap}px`,
              }}
            >
              {tile(id(), true)}
            </div>
          )}
        </Show>
      </Show>
    </main>
  );
};

/**
 * Switch between the two group layouts, top right.
 *
 * It shows the layout a press leads to, not the one on screen. Pinning
 * someone already means Focus, so while a pin is up the button unpins and
 * returns to the grid rather than changing a choice the pin overrides.
 */
const LayoutSwitch: Component = () => {
  const kind = (): "grid" | "focus" =>
    groupLayoutKind(callState.layoutChoice(), callState.pinnedId(), callState.participants());
  return (
    <button
      type="button"
      class="lk-call-layout"
      aria-label={kind() === "grid" ? t("call.layout.focus_a11y") : t("call.layout.grid_a11y")}
      title={kind() === "grid" ? t("call.layout.focus_a11y") : t("call.layout.grid_a11y")}
      onClick={(event): void => {
        event.stopPropagation();
        if (kind() === "focus") {
          callActions.pin(null);
          callActions.setLayoutChoice("grid");
        } else {
          callActions.setLayoutChoice("focus");
        }
      }}
    >
      <Icon name={kind() === "grid" ? "layoutFocus" : "layoutGrid"} size={20} />
    </button>
  );
};

// ============================================================================
// The participant list
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
 *  live call it has. Drives the self-view's flip button, which is why it runs
 *  from the call screen rather than from a settings panel that no longer
 *  exists. */
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
  /** Copy the link to this call (`docs/ux/call-links.md` section 3). The
   *  `Invite` control exists only where this is given: a call in a
   *  conversation is reached through that conversation, not through a link. */
  onCopyInviteLink?: (() => void) | undefined;
  /** Choose a conversation and send the invitation into it. */
  onInviteContact?: (() => void) | undefined;
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
  const [inviteMenuOpen, setInviteMenuOpen] = createSignal(false);
  const [settingsOpen, setSettingsOpen] = createSignal(false);
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

  // The outgoing tone (`docs/ux/calls.md` section 3.4). The shared rule
  // decides: only a one-to-one call this device placed, and only until it is
  // answered. A group call rings nobody, and neither does joining one from a
  // link; before this every call that was not yet connected looped the tone,
  // so a person alone in a call they had just started heard a phone ringing.
  createEffect(() => {
    const rules = uiRules();
    const rings =
      rules !== null &&
      rules.ringbackPlays(
        props.group === true,
        callBarPhase(rules, callState.phase().kind, false),
      );
    if (rings) sounds.startLoop("ringback-outgoing");
    else sounds.stopLoop("ringback-outgoing");
  });

  // The two short cues: in, and out. On the change of phase only, so a
  // re-run of this effect for any other reason cannot play one twice.
  let lastPhaseKind: CallPhaseKind | null = null;
  createEffect(() => {
    const kind = callState.phase().kind;
    if (kind === lastPhaseKind) return;
    lastPhaseKind = kind;
    if (kind === "idle") sounds.stopAll();
    else if (kind === "connected") sounds.playOnce("join");
    else if (kind === "ended") {
      sounds.stopAll();
      sounds.playOnce("leave");
    }
  });

  // Peer arrival / departure cues.
  let lastRemoteIds: ReadonlyArray<string> = [];
  createEffect(() => {
    const remote = callState.participants().filter((p) => !p.isLocal);
    const remoteIds = remote.map((p) => p.id);
    if (callState.phase().kind === "connected" && lastRemoteIds.length > 0) {
      for (const p of remote) {
        if (!lastRemoteIds.includes(p.id)) {
          sounds.playOnce("join");
        }
      }
      const present = new Set(remoteIds);
      for (const prevId of lastRemoteIds) {
        if (!present.has(prevId)) {
          sounds.playOnce("leave");
        }
      }
    }
    lastRemoteIds = remoteIds;
  });

  // Who is speaking (`docs/ux/group-call-grid.md` section 7.3): read on this
  // device from the audio it plays, for the ring on a tile and for the large
  // tile in the Focus layout. A group call only; a one-to-one has one voice.
  const speaking = new SpeakingMonitor((snapshot) => {
    callActions.setSpeech(snapshot.speakingIds, {
      activeSpeakerId: snapshot.activeSpeakerId,
      lastSpokeAtMs: snapshot.lastSpokeAtMs,
    });
  });
  onCleanup(() => speaking.dispose());
  const voiceStreams = createMemo(
    () =>
      props.group === true && props.embedded === true
        ? callState.participants().map((p) => ({
            id: p.id,
            stream: p.isLocal ? props.localStream?.() : props.remoteStreamFor?.(p.id),
          }))
        : [],
    undefined,
    {
      equals: (a, b) =>
        a.length === b.length && a.every((x, i) => x.id === b[i]?.id && x.stream === b[i]?.stream),
    },
  );
  createEffect(() => speaking.sync(voiceStreams()));

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
      groupCall: props.group === true,
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
      participantsOpen: callState.participantListOpen(),
      othersPresent: remotes().length > 0,
      canInvite: props.onCopyInviteLink !== undefined,
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
      case "mic":
        callActions.setMic(local()?.micOn !== true);
        return;
      case "camera":
        callActions.setCamera(local()?.camOn !== true);
        return;
      case "participants":
        // With a link to hand out: the menu. Without one: the list itself,
        // since a menu of one item is a second step for nothing.
        if (props.onCopyInviteLink !== undefined) setInviteMenuOpen(!inviteMenuOpen());
        else callActions.setParticipantListOpen(!callState.participantListOpen());
        return;
      case "settings":
        void refreshCallDevices();
        setSettingsOpen(true);
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
      if (encryptionSheetOpen() || settingsOpen()) return;
      if (inviteMenuOpen()) {
        event.preventDefault();
        setInviteMenuOpen(false);
        return;
      }
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
        <GroupArea remoteStreamFor={props.remoteStreamFor} localStream={props.localStream} />
        <Show when={remotes().length > 0}>
          <LayoutSwitch />
        </Show>
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

      {/* Participants: the list of who is here and the two ways to invite, in a
          menu of the app's own, never the system's share sheet
          (`docs/ux/call-links.md` section 3). */}
      <Show when={inviteMenuOpen() && props.onCopyInviteLink !== undefined}>
        <div
          class="lk-call-menu-scrim"
          onPointerDown={(event): void => {
            event.stopPropagation();
            setInviteMenuOpen(false);
          }}
        />
        <div class="lk-call-menu" role="menu" aria-label={t("call.invite.menu_a11y")}>
          <Show when={remotes().length > 0}>
            <button
              type="button"
              role="menuitem"
              class="lk-call-menu-item"
              onClick={(): void => {
                setInviteMenuOpen(false);
                callActions.setParticipantListOpen(true);
              }}
            >
              <Icon name="groups" size={20} />
              <span>{t("call.control.participants")}</span>
            </button>
          </Show>
          <button
            type="button"
            role="menuitem"
            class="lk-call-menu-item"
            onClick={(): void => {
              setInviteMenuOpen(false);
              props.onCopyInviteLink?.();
            }}
          >
            <Icon name="copy" size={20} />
            <span>{t("call.invite.menu_copy")}</span>
          </button>
          <Show when={props.onInviteContact}>
            {(invite) => (
              <button
                type="button"
                role="menuitem"
                class="lk-call-menu-item"
                onClick={(): void => {
                  setInviteMenuOpen(false);
                  invite()();
                }}
              >
                <Icon name="user" size={20} />
                <span>{t("call.invite.menu_contact")}</span>
              </button>
            )}
          </Show>
        </div>
      </Show>

      <Show when={settingsOpen()}>
        <CallSettingsSheet onClose={(): void => void setSettingsOpen(false)} />
      </Show>

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
