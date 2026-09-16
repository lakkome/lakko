// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The in-call UI (docs/ux/client-redesign.md section 5.7).
 *
 * Layout: active-speaker tile on top, gallery row below, a bottom row of
 * round controls, a participant list or the in-call chat as a right-hand
 * drawer, the settings sheet over everything. The surface is dark by design
 * (calls happen at any hour and a dark ground reads better behind camera
 * feeds): the `.lk-call` root carries the dark half of the shared tokens
 * (`styles.css`), so this file has no palette of its own and no colour
 * literal.
 *
 * Two modes:
 *   * Standalone design route (`/call/:id`, no props) — seeds mock
 *     participants on Join; the reference UI for the design system.
 *   * Embedded over a live call (`CallHostView`) — `embedded` is set and the
 *     host drives `callState` from the real session. The screen skips the
 *     mount-time `reset()` (the host owns lifecycle) and routes end-call +
 *     the ended-overlay dismissal through `onExit`.
 *
 * The safety-number pill shows exactly when there IS a number to show
 * (`calls/safety-pill.ts`), never on a layout flag.
 */

import {
  createEffect,
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
  activeSpeaker,
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
import { showsSafetyNumber } from "../calls/safety-pill";
import { paths } from "./paths";
import { Icon } from "../ui/icons";
import { initialsOf, toneOf } from "../ui/Avatar";

// ============================================================================
// QualityDot — small glyph in a tile corner + the participant list
// ============================================================================

interface QualityDotProps {
  tier: QualityTier;
  /** When true, clicking the dot toggles the connection-detail popover
   *  (the top-of-screen indicator). When false it is a passive glyph. */
  interactive?: boolean;
}

const QualityDot: Component<QualityDotProps> = (props) => (
  <span
    class="lk-call-dot"
    classList={{ interactive: props.interactive === true }}
    data-tier={qualityColour(props.tier)}
    role={props.interactive === true ? "button" : undefined}
    aria-label={`Connection quality: ${props.tier}`}
    title={`Connection: ${props.tier}`}
    onClick={(e): void => {
      if (props.interactive === true) {
        e.stopPropagation();
        callActions.setQualityPopoverOpen(!callState.qualityPopoverOpen());
      }
    }}
  />
);

// ============================================================================
// E2eePill — the safety number the two sides can compare. Shown only when
// there is a number; while keying is still being established the pill says
// so rather than claiming protection.
// ============================================================================

const E2eePill: Component = () => {
  const e = (): ReturnType<typeof callState.e2ee> => callState.e2ee();
  return (
    <Show
      when={showsSafetyNumber(e())}
      fallback={
        <div class="lk-call-pill negotiating" role="status" title="Setting up encryption">
          <Icon name="lock" size={14} />
          <span>Setting up...</span>
        </div>
      }
    >
      <div
        class="lk-call-pill secure"
        role="status"
        title="End-to-end encrypted. Compare this safety number with the other side."
      >
        <Icon name="lock" size={14} />
        <span class="lk-call-pill-number">{e().shortFingerprint}</span>
      </div>
    </Show>
  );
};

// ============================================================================
// QualityIndicator — top-of-screen quality dot + tappable popover with
// RTT / bitrate / connection type.
// ============================================================================

function localQualityTier(): QualityTier {
  const local = callState.participants().find((p) => p.isLocal);
  return local?.quality ?? "good";
}

function formatBps(bps: number | null): string {
  if (bps === null) return "—";
  if (bps >= 1_000_000) return `${(bps / 1_000_000).toFixed(1)} Mbps`;
  if (bps >= 1_000) return `${(bps / 1_000).toFixed(0)} kbps`;
  return `${bps} bps`;
}

const QualityIndicator: Component = () => (
  <div class="lk-call-quality">
    <QualityDot tier={localQualityTier()} interactive={true} />
    <Show when={callState.qualityPopoverOpen()}>
      <div class="lk-call-popover" role="dialog" aria-label="Connection details">
        <div class="lk-call-popover-head">
          <span>{localQualityTier()}</span>
          <span
            classList={{
              ok: callState.connectionDetail().connectionType === "direct",
              warn: callState.connectionDetail().connectionType !== "direct",
            }}
          >
            {callState.connectionDetail().connectionType === "direct"
              ? "Direct"
              : callState.connectionDetail().connectionType === "relay"
                ? "Relay"
                : "—"}
          </span>
        </div>
        <dl class="lk-call-popover-stats">
          <dt>Round trip</dt>
          <dd>
            {callState.connectionDetail().rttMs !== null
              ? `${callState.connectionDetail().rttMs} ms`
              : "—"}
          </dd>
          <dt>Down</dt>
          <dd>{formatBps(callState.connectionDetail().downBps)}</dd>
          <dt>Up</dt>
          <dd>{formatBps(callState.connectionDetail().upBps)}</dd>
        </dl>
      </div>
    </Show>
  </div>
);

// ============================================================================
// ParticipantTile — single video / avatar card
// ============================================================================

interface TileProps {
  participant: Participant;
  large: boolean;
  onPin?: (id: string) => void;
  /** SFU group-call only: resolve this participant to its remote
   *  `MediaStream` (see the `Call` prop of the same name). */
  remoteStreamFor?: ((participantId: string) => MediaStream | undefined) | undefined;
  /** 1x1 P2P only: the local capture `MediaStream`, for the self-preview
   *  `<video>` on the LOCAL tile (see the `Call` prop). */
  localStream?: (() => MediaStream | undefined) | undefined;
}

const ParticipantTile: Component<TileProps> = (props) => {
  const isPinned = (): boolean => callState.pinnedId() === props.participant.id;
  /** The live `MediaStream` for this tile, if any. The local tile reads the
   *  self-preview capture; a remote tile reads the SFU / P2P remote stream.
   *  Reading it inside JSX tracks the underlying signals. */
  const tileStream = (): MediaStream | undefined =>
    props.participant.isLocal
      ? props.localStream?.()
      : props.remoteStreamFor?.(props.participant.id);
  const name = (): string =>
    props.participant.displayName || (props.participant.isLocal ? "You" : "Participant");
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
        {/* A real <video> bound to the stream. Muted — audio plays through
            the dedicated hidden <audio> sinks, so this element never doubles
            the audio. The mock path passes no stream and falls through to
            the blank surface. */}
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

      {/* Mid-call camera-on hint on the LOCAL tile: while the camera is
          being acquired the tile is present but the track is not live, so
          the black tile reads as loading, not broken. */}
      <Show when={props.participant.isLocal && callState.videoPublishState() === "starting"}>
        <div class="lk-call-tile-overlay" role="status" aria-live="polite">
          <Spinner size={props.large ? 30 : 24} />
          <span>Starting camera...</span>
        </div>
      </Show>

      {/* Central mute glyph when the remote is muted, so the listener knows
          their audio input is off. Hidden on the local tile. */}
      <Show when={!props.participant.micOn && !props.participant.isLocal}>
        <div class="lk-call-tile-mute" aria-hidden="true">
          <Icon name="micOff" size={props.large ? 22 : 16} />
        </div>
      </Show>

      <Show when={props.participant.screenSharing}>
        <div class="lk-call-badge top-left">
          <Icon name="monitor" size={14} />
          Sharing
        </div>
      </Show>

      <Show when={props.participant.handRaised}>
        <div class="lk-call-badge top-right" title="Hand raised" aria-label="Hand raised">
          <Icon name="hand" size={14} />
        </div>
      </Show>

      {/* Bottom strip: name + mute + quality + pin */}
      <div class="lk-call-tile-strip">
        <Show when={!props.participant.micOn}>
          <span class="lk-call-tile-muted" aria-label="Microphone muted" title="Muted">
            <Icon name="micOff" size={14} />
          </span>
        </Show>
        <span class="lk-call-tile-name">{name()}</span>
        <QualityDot tier={props.participant.quality} />
        <Show when={isPinned()}>
          <span class="lk-call-tile-pin" title="Pinned">
            <Icon name="pin" size={14} />
          </span>
        </Show>
      </div>
    </div>
  );
};

// ============================================================================
// PrejoinScreen — a card with the preview and two toggles.
// ============================================================================

const PrejoinScreen: Component<{ onJoin: () => void }> = (props) => {
  // Local toggles for camera / mic preview. Once the user joins, these copy
  // into the participant entry's flags so the post-join state matches.
  const [previewCamOn, setPreviewCamOn] = createSignal(true);
  const [previewMicOn, setPreviewMicOn] = createSignal(true);

  return (
    <div class="lk-call lk-call-prejoin" role="region" aria-label="Before you join">
      <div class="lk-call-prejoin-card">
        <h1>Ready to join?</h1>
        <p>Check your camera and microphone first.</p>

        <div class="lk-call-preview">
          <Show
            when={previewCamOn()}
            fallback={
              <div class="lk-call-tile-blank" aria-hidden="true">
                <Icon name="videoOff" size={48} strokeWidth={1.4} />
              </div>
            }
          >
            <div class="lk-call-tile-blank" aria-hidden="true">
              <Icon name="video" size={48} strokeWidth={1.4} />
            </div>
          </Show>
        </div>

        <div class="lk-call-preview-toggles" role="group" aria-label="Devices">
          <CircleButton
            label={previewCamOn() ? "Turn off camera" : "Turn on camera"}
            glyph={<Icon name={previewCamOn() ? "video" : "videoOff"} size={22} />}
            active={!previewCamOn()}
            onClick={(): void => {
              setPreviewCamOn(!previewCamOn());
            }}
          />
          <CircleButton
            label={previewMicOn() ? "Mute microphone" : "Unmute microphone"}
            glyph={<Icon name={previewMicOn() ? "mic" : "micOff"} size={22} />}
            active={!previewMicOn()}
            onClick={(): void => {
              setPreviewMicOn(!previewMicOn());
            }}
          />
        </div>

        <button
          type="button"
          class="primary lk-call-join"
          onClick={(): void => {
            // Carry the preview toggles into the post-join participant
            // state, then transition.
            callActions.setMic(previewMicOn());
            callActions.setCamera(previewCamOn());
            callActions.join();
            props.onJoin();
          }}
        >
          Join
        </button>
      </div>
    </div>
  );
};

// ============================================================================
// WaitingMessage — shown while the local participant is alone in the call
// ============================================================================

const WaitingMessage: Component = () => (
  <div class="lk-call-waiting" role="status" aria-live="polite">
    Waiting for others to join...
  </div>
);

// ============================================================================
// ControlsBar — the bottom row of round buttons
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

interface ButtonProps {
  label: string;
  glyph: JSX.Element;
  active: boolean;
  destructive?: boolean;
  /** Show a spinner in place of the glyph + make the button non-interactive
   *  — an async action is in flight (e.g. mid-call camera acquisition). */
  busy?: boolean;
  /** Dim + disable the button without a spinner. */
  disabled?: boolean;
  onClick: () => void;
}

const CircleButton: Component<ButtonProps> = (props) => {
  const inert = (): boolean => props.busy === true || props.disabled === true;
  return (
    <button
      type="button"
      class="lk-call-btn"
      classList={{
        active: props.active && props.destructive !== true,
        destructive: props.destructive === true,
        busy: props.busy === true,
      }}
      aria-label={props.label}
      aria-pressed={props.destructive === true ? undefined : props.active}
      aria-busy={props.busy === true ? "true" : undefined}
      title={props.label}
      disabled={inert()}
      onClick={(): void => {
        if (inert()) return;
        props.onClick();
      }}
    >
      <Show when={props.busy === true} fallback={props.glyph}>
        <Spinner />
      </Show>
    </button>
  );
};

const ControlsBar: Component = () => {
  const local = (): Participant | null =>
    callState.participants().find((p) => p.isLocal) ?? null;

  return (
    <div class="lk-call-controls" role="toolbar" aria-label="Call controls">
      <CircleButton
        label={local()?.micOn === true ? "Mute microphone" : "Unmute microphone"}
        glyph={<Icon name={local()?.micOn === true ? "mic" : "micOff"} size={22} />}
        active={local()?.micOn === false}
        onClick={(): void => callActions.setMic(local()?.micOn !== true)}
      />
      <CircleButton
        label={
          callState.videoPublishState() === "starting"
            ? "Starting camera..."
            : local()?.camOn === true
              ? "Stop camera"
              : "Start camera"
        }
        glyph={<Icon name={local()?.camOn === true ? "video" : "videoOff"} size={22} />}
        active={local()?.camOn === false}
        // The acquire + renegotiate is async; a spinner + disabled prevents a
        // spam-tap while the session in-flight-guards.
        busy={callState.videoPublishState() === "starting"}
        onClick={(): void => callActions.setCamera(local()?.camOn !== true)}
      />
      {/* Switch front / back camera — surfaces only when the platform
          reported two or more cameras (a phone). Hidden on a laptop with one
          built-in camera. */}
      <Show when={callState.hasMultipleCameras() && local()?.camOn === true}>
        <CircleButton
          label="Switch camera"
          glyph={<Icon name="cameraFlip" size={22} />}
          active={false}
          onClick={(): void => {
            // Cycle to the next video input. The store's `selectedVideoInput`
            // drives the in-call device-switch effect, which calls
            // `session.switchVideoInput` + `replaceTrack` (same-kind, no
            // renegotiation) — so this reuses the shipped device-switch path
            // rather than re-capturing here.
            void (async (): Promise<void> => {
              try {
                const devices = await navigator.mediaDevices.enumerateDevices();
                const cams = devices.filter((d) => d.kind === "videoinput" && d.deviceId.length > 0);
                if (cams.length < 2) return;
                const current = callState.settings().selectedVideoInput;
                const idx =
                  current === null
                    ? 0
                    : Math.max(
                        0,
                        cams.findIndex((c) => c.deviceId === current),
                      );
                const next = cams[(idx + 1) % cams.length];
                if (next !== undefined) {
                  callActions.setSelectedVideoInput(next.deviceId);
                }
              } catch (err) {
                // eslint-disable-next-line no-console
                console.warn("call ui: switch camera enumerate failed", err);
              }
            })();
          }}
        />
      </Show>
      <CircleButton
        label="Participants"
        glyph={<Icon name="contacts" size={22} />}
        active={callState.participantListOpen()}
        onClick={(): void => callActions.setParticipantListOpen(!callState.participantListOpen())}
      />
      <CircleButton
        label={local()?.handRaised === true ? "Lower hand" : "Raise hand"}
        glyph={<Icon name="hand" size={22} />}
        active={local()?.handRaised === true}
        onClick={(): void => callActions.setHandRaised(local()?.handRaised !== true)}
      />
      <CircleButton
        label={local()?.screenSharing === true ? "Stop sharing screen" : "Share screen"}
        glyph={<Icon name="monitor" size={22} />}
        active={local()?.screenSharing === true}
        onClick={(): void => callActions.setScreenSharing(local()?.screenSharing !== true)}
      />
      <ChatToggleButton />
      <CircleButton
        label="Settings"
        glyph={<Icon name="settings" size={22} />}
        active={callState.settingsOpen()}
        onClick={(): void => callActions.setSettingsOpen(!callState.settingsOpen())}
      />
      <CircleButton
        label="End call"
        glyph={<Icon name="phoneEnd" size={22} />}
        active={false}
        destructive={true}
        onClick={(): void => callActions.endCall()}
      />
    </div>
  );
};

/** Chat toggle — a CircleButton with an unread-count badge. The badge
 *  surfaces only when there is something to read AND the panel is closed. */
const ChatToggleButton: Component = () => (
  <div class="lk-call-btn-wrap">
    <CircleButton
      label={
        callState.chatOpen()
          ? "Close chat"
          : callState.chatUnreadCount() > 0
            ? `Chat (${callState.chatUnreadCount()} unread)`
            : "Chat"
      }
      glyph={<Icon name="chat" size={22} />}
      active={callState.chatOpen()}
      onClick={(): void => callActions.setChatOpen(!callState.chatOpen())}
    />
    <Show when={callState.chatUnreadCount() > 0 && !callState.chatOpen()}>
      <span class="lk-call-btn-badge" aria-hidden="true">
        {callState.chatUnreadCount() > 99 ? "99+" : callState.chatUnreadCount()}
      </span>
    </Show>
  </div>
);

// ============================================================================
// SettingsPanel — the settings sheet
// ============================================================================
//
// Three device pickers + five processing toggles. The device pickers list
// the OS's real capture / playback devices via `refreshCallDevices`, pulled
// when the panel opens and on hot-plug (`devicechange`).

/** Enumerate the OS's real capture / playback devices and publish them into
 *  the shared call store so the pickers list the user's actual microphones,
 *  cameras and speakers. Device labels populate only once `getUserMedia` has
 *  been granted (during a live call); before that the browser returns stable
 *  ids with blank labels, which the picker renders with a generic fallback. */
async function refreshCallDevices(): Promise<void> {
  const md = typeof navigator !== "undefined" ? navigator.mediaDevices : undefined;
  if (!md?.enumerateDevices) {
    return;
  }
  try {
    const infos = await md.enumerateDevices();
    const devices: CallDevice[] = infos
      .filter((d) => d.kind === "audioinput" || d.kind === "videoinput" || d.kind === "audiooutput")
      .map((d) => ({
        id: d.deviceId,
        label: d.label,
        kind: d.kind as CallDevice["kind"],
      }));
    callActions.setAvailableDevices(devices);
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("enumerateDevices failed", err);
  }
}

const SettingsPanel: Component = () => {
  // Refresh the device list whenever the panel opens, and keep it current on
  // hot-plug (a mic / headset / camera connected or removed mid-call).
  createEffect(() => {
    if (!callState.settingsOpen()) {
      return;
    }
    void refreshCallDevices();
  });
  onMount(() => {
    const md = typeof navigator !== "undefined" ? navigator.mediaDevices : undefined;
    if (!md?.addEventListener) {
      return;
    }
    const handler = (): void => {
      void refreshCallDevices();
    };
    md.addEventListener("devicechange", handler);
    onCleanup(() => md.removeEventListener("devicechange", handler));
  });
  return (
    <Show when={callState.settingsOpen()}>
      <div
        class="lk-call-modal-backdrop"
        role="dialog"
        aria-modal="true"
        aria-label="Call settings"
        onClick={(e): void => {
          // Click-out to dismiss — only when the backdrop itself is the
          // target, not the inner card.
          if (e.target === e.currentTarget) {
            callActions.setSettingsOpen(false);
          }
        }}
      >
        <div class="lk-call-modal">
          <div class="lk-call-modal-head">
            <h2>Settings</h2>
            <button
              type="button"
              class="lk-icon-button"
              aria-label="Close settings"
              onClick={(): void => callActions.setSettingsOpen(false)}
            >
              <Icon name="close" size={18} />
            </button>
          </div>

          <DevicePicker
            label="Microphone"
            kind="audioinput"
            selectedId={callState.settings().selectedAudioInput}
            onPick={(id): void => callActions.setSelectedAudioInput(id)}
          />
          <DevicePicker
            label="Camera"
            kind="videoinput"
            selectedId={callState.settings().selectedVideoInput}
            onPick={(id): void => callActions.setSelectedVideoInput(id)}
          />
          <DevicePicker
            label="Speaker"
            kind="audiooutput"
            selectedId={callState.settings().selectedAudioOutput}
            onPick={(id): void => callActions.setSelectedAudioOutput(id)}
          />

          <hr class="lk-call-sep" />

          <SettingsToggle
            label="Noise suppression"
            checked={callState.settings().noiseSuppression}
            onChange={(v): void => callActions.setNoiseSuppression(v)}
          />
          <SettingsToggle
            label="Echo cancellation"
            checked={callState.settings().echoCancellation}
            onChange={(v): void => callActions.setEchoCancellation(v)}
          />
          <SettingsToggle
            label="Automatic gain control"
            checked={callState.settings().autoGain}
            onChange={(v): void => callActions.setAutoGain(v)}
          />
          <SettingsToggle
            label="Mirror my preview"
            checked={callState.settings().mirrorLocal}
            onChange={(v): void => callActions.setMirrorLocal(v)}
          />
          <SettingsToggle
            label="Low-data mode"
            description="Caps video at 360p. Useful on a metered connection."
            checked={callState.settings().lowDataMode}
            onChange={(v): void => callActions.setLowDataMode(v)}
          />
        </div>
      </div>
    </Show>
  );
};

const DevicePicker: Component<{
  label: string;
  kind: CallDevice["kind"];
  selectedId: string | null;
  onPick: (id: string | null) => void;
}> = (props) => {
  const available = (): CallDevice[] =>
    callState.settings().availableDevices.filter((d) => d.kind === props.kind);
  return (
    <label class="lk-call-field">
      {props.label}
      <select
        value={props.selectedId ?? ""}
        onChange={(e): void => {
          const value = e.currentTarget.value;
          props.onPick(value === "" ? null : value);
        }}
      >
        <option value="">System default</option>
        <For each={available()}>
          {(d): JSX.Element => <option value={d.id}>{d.label || `(${d.kind})`}</option>}
        </For>
      </select>
      <Show when={available().length === 0}>
        <span class="lk-meta lk-block lk-mt-4">
          No devices listed yet. They appear once you allow the microphone or camera.
        </span>
      </Show>
    </label>
  );
};

const SettingsToggle: Component<{
  label: string;
  description?: string;
  checked: boolean;
  onChange: (value: boolean) => void;
}> = (props) => (
  <label class="lk-check lk-call-toggle">
    <input
      type="checkbox"
      checked={props.checked}
      onChange={(e): void => props.onChange(e.currentTarget.checked)}
    />
    <span class="lk-call-toggle-text">
      <span>{props.label}</span>
      <Show when={props.description}>{(d) => <span class="lk-meta">{d()}</span>}</Show>
    </span>
  </label>
);

// ============================================================================
// ParticipantList drawer
// ============================================================================

const ParticipantList: Component = () => (
  <Show when={callState.participantListOpen()}>
    <aside class="lk-call-drawer" aria-label="Participants">
      <div class="lk-call-drawer-head">
        <h2>Participants ({callState.participants().length})</h2>
        <button
          type="button"
          class="lk-icon-button"
          aria-label="Close participants"
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
                {p.displayName || (p.isLocal ? "You" : "Participant")}
              </span>
              <Show when={p.handRaised}>
                <span title="Hand raised" aria-label="Hand raised">
                  <Icon name="hand" size={14} />
                </span>
              </Show>
              <Show when={!p.micOn}>
                <span class="lk-call-tile-muted" title="Muted" aria-label="Muted">
                  <Icon name="micOff" size={14} />
                </span>
              </Show>
              <Show when={p.screenSharing}>
                <span title="Sharing screen" aria-label="Sharing screen">
                  <Icon name="monitor" size={14} />
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
// PhaseBanner — connecting / reconnecting / ended
// ============================================================================

const PhaseBanner: Component = () => (
  <Show when={callState.phase().kind !== "connected"}>
    <div class="lk-call-phase" data-kind={callState.phase().kind} role="status">
      {phaseText(callState.phase())}
    </div>
  </Show>
);

function phaseText(p: ReturnType<typeof callState.phase>): string {
  switch (p.kind) {
    case "prejoin":
      return "";
    case "connecting":
      return "Connecting...";
    case "reconnecting":
      return `Reconnecting. ${p.reason}`;
    case "ended":
      return "Call ended";
    case "connected":
      return "";
  }
}

// ============================================================================
// CallEndedOverlay — dim + hand back after 3 s
// ============================================================================

const CallEndedOverlay: Component<{ onDismiss: () => void }> = (props) => {
  const [timer, setTimer] = createSignal<number | null>(null);

  onMount(() => {
    setTimer(window.setTimeout(props.onDismiss, 3000));
  });
  onCleanup(() => {
    const t = timer();
    if (t !== null) window.clearTimeout(t);
  });

  return (
    <div class="lk-call-ended">
      <h1>Call ended</h1>
      <p>Returning to your chats...</p>
    </div>
  );
};

// ============================================================================
// ChatPanel — in-call chat
// ============================================================================
//
// Right-hand drawer, full height. Text only; system events (peer joined /
// left) render inline as muted centred lines.

const ChatPanel: Component = () => {
  let scrollRef: HTMLDivElement | undefined;
  let inputRef: HTMLInputElement | undefined;

  // Auto-scroll to the bottom whenever the message list grows.
  createEffect((): void => {
    // Re-read so the effect is reactive to message changes.
    void callState.chatMessages().length;
    queueMicrotask((): void => {
      if (scrollRef !== undefined) {
        scrollRef.scrollTop = scrollRef.scrollHeight;
      }
    });
  });

  // Auto-focus the compose box when the panel opens.
  createEffect((): void => {
    if (callState.chatOpen() && inputRef !== undefined) {
      // Delay one tick — the input is being mounted in the same microtask.
      queueMicrotask((): void => inputRef?.focus());
    }
  });

  const submit = (): void => {
    callActions.sendChatMessage(callState.composeText());
  };

  return (
    <Show when={callState.chatOpen()}>
      <aside class="lk-call-drawer lk-call-chat" aria-label="Call chat">
        <header class="lk-call-drawer-head">
          <h2>Chat</h2>
          <button
            type="button"
            class="lk-icon-button"
            aria-label="Close chat"
            onClick={(): void => callActions.setChatOpen(false)}
          >
            <Icon name="close" size={18} />
          </button>
        </header>

        <div class="lk-call-chat-log" ref={scrollRef}>
          <Show
            when={callState.chatMessages().length > 0}
            fallback={
              <div class="lk-call-chat-empty">
                No messages yet. Messages here are end-to-end encrypted.
              </div>
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
            submit();
          }}
        >
          <input
            ref={inputRef}
            class="lk-composer-input"
            type="text"
            placeholder="Message"
            aria-label="Message"
            autocomplete="off"
            value={callState.composeText()}
            onInput={(e): void => callActions.setComposeText(e.currentTarget.value)}
          />
          <button
            type="submit"
            class="lk-icon-button lk-send"
            disabled={callState.composeText().trim().length === 0}
            aria-label="Send"
            title="Send"
          >
            <Icon name="send" size={20} />
          </button>
        </form>
      </aside>
    </Show>
  );
};

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

// ============================================================================
// Mock data — seeds the standalone design route with placeholder participants
// ============================================================================

function seedMockParticipants(): void {
  // Mock safety number and connection metrics — the live paths write real
  // ones through the same actions.
  callActions.setE2ee({
    active: true,
    shortFingerprint: "3F7A B2C1 9E0D",
  });
  callActions.setConnectionDetail({
    rttMs: 45,
    downBps: 720_000,
    upBps: 380_000,
    connectionType: "direct",
  });
  // Hard-coded false so the desktop preview does not show the switch-camera
  // button.
  callActions.setHasMultipleCameras(false);
  callActions.setParticipants([
    {
      id: "local",
      displayName: "You",
      isLocal: true,
      micOn: true,
      camOn: true,
      handRaised: false,
      quality: "excellent",
      speaking: false,
      screenSharing: false,
    },
    {
      id: "leaf-1",
      displayName: "Alice",
      isLocal: false,
      micOn: true,
      camOn: true,
      handRaised: false,
      quality: "good",
      speaking: true,
      screenSharing: false,
    },
    {
      id: "leaf-2",
      displayName: "Bob",
      isLocal: false,
      micOn: false,
      camOn: true,
      handRaised: false,
      quality: "fair",
      speaking: false,
      screenSharing: false,
    },
    {
      id: "leaf-3",
      displayName: "Carl",
      isLocal: false,
      micOn: true,
      camOn: false,
      handRaised: true,
      quality: "poor",
      speaking: false,
      screenSharing: false,
    },
  ]);
  // Synthesise the "connected" transition after a short delay.
  window.setTimeout((): void => {
    callActions.setPhase({ kind: "connected" });
  }, 600);
}

// ============================================================================
// Top-level route
// ============================================================================

const Call: Component<{
  embedded?: boolean;
  onExit?: () => void;
  /** SFU group-call only: resolve a roster participant id to its live remote
   *  `MediaStream` so the tile can render real video. Omitted on the 1x1 and
   *  mock paths (tiles fall back to the avatar / blank surface). */
  remoteStreamFor?: ((participantId: string) => MediaStream | undefined) | undefined;
  /** 1x1 P2P only: the local capture `MediaStream`, for the LOCAL tile's
   *  self-preview `<video>`. Omitted on the SFU / mock paths. */
  localStream?: (() => MediaStream | undefined) | undefined;
}> = (props) => {
  const navigate = useNavigate();

  // Sound cues — one controller per Call mount; disposed on unmount so cached
  // <audio> elements + their decoders are released cleanly.
  const sounds = new CallSoundController();

  onMount(() => {
    // Embedded mode: the host already drove `callState` to a live phase
    // before mounting us — resetting would wipe it.
    if (props.embedded !== true) {
      callActions.reset();
      // `reset()` leaves us in `prejoin`. The mock participants + safety
      // number get seeded only after the user clicks Join.
    }
  });
  onCleanup(() => {
    sounds.dispose();
    // Embedded mode: the host owns the call lifecycle and resets `callState`
    // when the session reaches `done`.
    if (props.embedded !== true) {
      callActions.reset();
    }
  });

  // Phase -> sound mapping. Mirrors the same trigger map on iOS / Android so
  // the user-facing audio is identical across platforms.
  //   prejoin      -> silence
  //   connecting   -> ringback-outgoing loop ("we are calling")
  //   connected    -> stop ringback, play join one-shot
  //   reconnecting -> stop everything (the banner conveys it)
  //   ended        -> stop everything, play leave one-shot
  createEffect(() => {
    const kind = callState.phase().kind;
    switch (kind) {
      case "prejoin":
        sounds.stopAll();
        break;
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

  // Peer-arrival / peer-departure cues + chat system events. Track the
  // remote id set; on each delta fire the right cue AND post a system event
  // into the chat. The local participant is excluded.
  let lastRemoteIds: ReadonlyArray<string> = [];
  createEffect(() => {
    const remote = callState.participants().filter((p) => !p.isLocal);
    const remoteIds = remote.map((p) => p.id);

    if (callState.phase().kind === "connected" && lastRemoteIds.length > 0) {
      // Joiners — present now but not in the previous snapshot.
      for (const p of remote) {
        if (!lastRemoteIds.includes(p.id)) {
          sounds.playOnce("join");
          callActions.addSystemEvent(`${p.displayName} joined the call`);
        }
      }
      // Leavers — in the previous snapshot but not now.
      const remoteById = new Map(remote.map((p) => [p.id, p]));
      const prevSnapshot = lastRemoteIds;
      for (const prevId of prevSnapshot) {
        if (!remoteById.has(prevId)) {
          sounds.playOnce("leave");
          callActions.addSystemEvent(`A participant left the call`);
          // The display name has already been dropped from state by the
          // time we observe the deletion.
        }
      }
    }
    lastRemoteIds = remoteIds;
  });

  // Whether we are alone in the call (no remote yet). The waiting line
  // fires off this.
  const isAlone = (): boolean => callState.participants().filter((p) => !p.isLocal).length === 0;

  return (
    <Show
      when={callState.phase().kind !== "prejoin"}
      fallback={
        <PrejoinScreen
          onJoin={(): void => {
            // Synthesise the "connected" transition on the design route.
            seedMockParticipants();
            window.setTimeout((): void => {
              callActions.setPhase({ kind: "connected" });
            }, 400);
          }}
        />
      }
    >
      <div class="lk-call">
        <PhaseBanner />

        {/* Top status bar — the safety-number pill (left) + the connection
            quality dot with its popover (right). The phase banner floats
            over the centre. */}
        <header class="lk-call-head">
          <E2eePill />
          <QualityIndicator />
        </header>

        <main
          class="lk-call-main"
          onClick={(): void => {
            // Tap anywhere on the main area closes the quality popover.
            if (callState.qualityPopoverOpen()) {
              callActions.setQualityPopoverOpen(false);
            }
          }}
        >
          <Show when={isAlone() && callState.phase().kind === "connected"}>
            <WaitingMessage />
          </Show>

          <Show when={activeSpeaker() !== null}>
            <div class="lk-call-stage">
              <ParticipantTile
                participant={activeSpeaker() as Participant}
                large={true}
                onPin={(id): void => callActions.pin(id === "" ? null : id)}
                remoteStreamFor={props.remoteStreamFor}
                localStream={props.localStream}
              />
            </div>
          </Show>

          <div class="lk-call-gallery">
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
        </main>

        <ControlsBar />
        <ParticipantList />
        <ChatPanel />
        <SettingsPanel />

        <Show when={callState.phase().kind === "ended"}>
          <CallEndedOverlay
            onDismiss={(): void => {
              // Embedded: hand control back to the host (which tears the
              // overlay down + resets state). Standalone design route:
              // navigate home as before.
              if (props.embedded === true) {
                props.onExit?.();
              } else {
                navigate(paths.home);
              }
            }}
          />
        </Show>
      </div>
    </Show>
  );
};

export default Call;
