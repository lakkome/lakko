// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `/call/:id` route — full in-call UI (Phase 3c6-web).
 *
 * Layout: active-speaker tile on top (or full-width on
 * portrait), gallery row below, controls bar at the bottom,
 * participant-list drawer on the right (toggleable). Dark
 * theme by default — calls happen at any hour and the dark
 * surface reads more naturally against camera feeds than a
 * light one.
 *
 * No real WebRTC integration here — the live media path lives
 * in each platform's WebRTC pod (3a-pod-followup). This route
 * renders **state**: participant tiles, mic / cam / hand
 * toggles, connection-quality glyphs, the lifecycle banner. A
 * mock-data seed runs on mount so the screen has something to
 * render until the pod wires real participants in.
 *
 * Phase-3c6 scope:
 *   - Active speaker promoted to large tile (`activeSpeaker`
 *     memo in call-state).
 *   - Gallery row with all-but-active.
 *   - Controls bar: mic / camera / participant list / raise
 *     hand / screen share / end call.
 *   - Connection-quality dot per tile (green / yellow / red
 *     mapped from the BandwidthController (#39) quality tier).
 *   - Participant-list drawer with mute / hand / quality
 *     glyphs.
 *   - Lifecycle banners: "connecting", "reconnecting...",
 *     "call ended" overlay.
 *
 * Out of scope (separate sprints):
 *   - Real video frames (3a-pod-followup).
 *   - Real screen-share (`getDisplayMedia()` integration).
 *   - In-call chat panel, reactions, closed captions.
 *   - Background blur / virtual backgrounds.
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
import { paths } from "./paths";

// ============================================================================
// Theme tokens (dark)
// ============================================================================

const COLOURS = {
  background: "#0F0F0F",
  surface: "#1A1A1A",
  surfaceHover: "#202020",
  border: "#2A2A2A",
  textPrimary: "#FAFAFA",
  textSecondary: "#A1A1AA",
  accentBlue: "#3B82F6",
  endCallRed: "#DC2626",
  mutedRed: "#EF4444",
  qualityGreen: "#10B981",
  qualityYellow: "#FBBF24",
  qualityRed: "#EF4444",
} as const;

function colourForQuality(tier: QualityTier): string {
  switch (qualityColour(tier)) {
    case "green":
      return COLOURS.qualityGreen;
    case "yellow":
      return COLOURS.qualityYellow;
    case "red":
      return COLOURS.qualityRed;
  }
}

// ============================================================================
// QualityDot — small glyph in tile corner + participant list
// ============================================================================

interface QualityDotProps {
  tier: QualityTier;
  /** When true, clicking the dot toggles the connection-detail
   *  popover (used by the top-of-screen indicator). When false
   *  it's a passive glyph (e.g. inside a participant tile). */
  interactive?: boolean;
}

const QualityDot: Component<QualityDotProps> = (props) => (
  <span
    role={props.interactive === true ? "button" : undefined}
    aria-label={`Connection quality: ${props.tier}`}
    title={`Quality: ${props.tier}`}
    onClick={(e): void => {
      if (props.interactive === true) {
        e.stopPropagation();
        callActions.setQualityPopoverOpen(!callState.qualityPopoverOpen());
      }
    }}
    style={{
      display: "inline-block",
      width: "8px",
      height: "8px",
      "border-radius": "50%",
      "background-color": colourForQuality(props.tier),
      "box-shadow": `0 0 4px ${colourForQuality(props.tier)}`,
      cursor: props.interactive === true ? "pointer" : "default",
    }}
  />
);

// ============================================================================
// E2eePill — small indicator that the call is cryptographically
// established + the short fingerprint the user can verify out-of-
// band. Lakko's security-UX cornerstone: if this is missing or
// reads `inactive`, the user knows the connection is not yet
// authenticated.
// ============================================================================

const E2eePill: Component = () => {
  const e = (): ReturnType<typeof callState.e2ee> => callState.e2ee();
  return (
    <Show
      when={e().active && e().shortFingerprint !== null}
      fallback={
        <div
          role="status"
          title="Encryption negotiating…"
          style={{
            display: "inline-flex",
            "align-items": "center",
            gap: "6px",
            padding: "4px 8px",
            "background-color": COLOURS.surface,
            color: COLOURS.qualityYellow,
            "border-radius": "999px",
            border: `1px solid ${COLOURS.border}`,
            "font-size": "0.7rem",
            "font-weight": "600",
          }}
        >
          🔒 …
        </div>
      }
    >
      <div
        role="status"
        title="End-to-end encrypted. Verify the fingerprint with your contact out-of-band."
        style={{
          display: "inline-flex",
          "align-items": "center",
          gap: "6px",
          padding: "4px 8px",
          "background-color": COLOURS.surface,
          color: COLOURS.qualityGreen,
          "border-radius": "999px",
          border: `1px solid ${COLOURS.border}`,
          "font-size": "0.7rem",
          "font-weight": "600",
          "font-family":
            "ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace",
        }}
      >
        🔒 {e().shortFingerprint}
      </div>
    </Show>
  );
};

// ============================================================================
// QualityIndicator — top-of-screen quality dot + tappable popover
// with RTT / bitrate / connection-type stats. Mirror of lakko_me's
// connectionQualityIndicator.
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
  <div
    style={{
      position: "relative",
      display: "inline-flex",
      "align-items": "center",
    }}
  >
    <QualityDot tier={localQualityTier()} interactive={true} />
    <Show when={callState.qualityPopoverOpen()}>
      <div
        role="dialog"
        aria-label="Connection details"
        style={{
          position: "absolute",
          top: "20px",
          right: "0",
          "min-width": "180px",
          "background-color": COLOURS.surface,
          border: `1px solid ${COLOURS.border}`,
          "border-radius": "8px",
          padding: "10px 12px",
          "font-size": "0.8rem",
          color: COLOURS.textPrimary,
          "box-shadow": "0 4px 16px rgba(0,0,0,0.5)",
          "z-index": "25",
        }}
      >
        <div
          style={{
            display: "flex",
            "justify-content": "space-between",
            "margin-bottom": "6px",
            "font-weight": "600",
            "text-transform": "capitalize",
          }}
        >
          <span>{localQualityTier()}</span>
          <span
            style={{
              color:
                callState.connectionDetail().connectionType === "direct"
                  ? COLOURS.qualityGreen
                  : COLOURS.qualityYellow,
            }}
          >
            {callState.connectionDetail().connectionType === "direct"
              ? "Direct"
              : callState.connectionDetail().connectionType === "relay"
                ? "Relay"
                : "—"}
          </span>
        </div>
        <div style={{ color: COLOURS.textSecondary, "line-height": "1.6" }}>
          <div>
            RTT:{" "}
            <span style={{ color: COLOURS.textPrimary }}>
              {callState.connectionDetail().rttMs !== null
                ? `${callState.connectionDetail().rttMs} ms`
                : "—"}
            </span>
          </div>
          <div>
            ↓{" "}
            <span style={{ color: COLOURS.textPrimary }}>
              {formatBps(callState.connectionDetail().downBps)}
            </span>{" "}
            &nbsp;↑{" "}
            <span style={{ color: COLOURS.textPrimary }}>
              {formatBps(callState.connectionDetail().upBps)}
            </span>
          </div>
        </div>
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
  remoteStreamFor?:
    | ((participantId: string) => MediaStream | undefined)
    | undefined;
  /** 1×1 P2P only: the local capture `MediaStream`, for the
   *  self-preview `<video>` on the LOCAL tile (see the `Call` prop). */
  localStream?: (() => MediaStream | undefined) | undefined;
}

const ParticipantTile: Component<TileProps> = (props) => {
  const isPinned = (): boolean =>
    callState.pinnedId() === props.participant.id;
  /** The live `MediaStream` for this tile, if any. The local tile reads
   *  the self-preview capture; a remote tile reads the SFU / P2P remote
   *  stream. Reading it inside JSX tracks the underlying signals. */
  const tileStream = (): MediaStream | undefined =>
    props.participant.isLocal
      ? props.localStream?.()
      : props.remoteStreamFor?.(props.participant.id);
  return (
    <div
      onClick={(): void => {
        if (props.onPin !== undefined && !props.participant.isLocal) {
          props.onPin(isPinned() ? "" : props.participant.id);
        }
      }}
      style={{
        position: "relative",
        "background-color": COLOURS.surface,
        border: `1px solid ${
          props.participant.speaking ? COLOURS.accentBlue : COLOURS.border
        }`,
        "border-radius": "12px",
        overflow: "hidden",
        "aspect-ratio": props.large ? "16 / 9" : "4 / 3",
        cursor: props.participant.isLocal ? "default" : "pointer",
        "box-shadow": props.participant.speaking
          ? `0 0 0 2px ${COLOURS.accentBlue}`
          : "none",
        transition: "box-shadow 120ms ease",
      }}
    >
      {/* Mock video / avatar surface */}
      <Show
        when={props.participant.camOn}
        fallback={
          <div
            style={{
              position: "absolute",
              inset: "0",
              display: "flex",
              "align-items": "center",
              "justify-content": "center",
              "font-size": props.large ? "5rem" : "2.5rem",
              "font-weight": "600",
              color: COLOURS.textPrimary,
              "background-color": "#27272A",
            }}
          >
            {props.participant.displayName.slice(0, 1).toUpperCase() || "?"}
          </div>
        }
      >
        {/* SFU group call: a real <video> bound to the remote
            MediaStream. Muted — audio plays through the dedicated
            hidden <audio> sinks (one per stream) so this element
            never doubles the audio. The 1×1 + mock paths pass no
            `remoteStreamFor`, so they fall through to the placeholder. */}
        <Show
          when={tileStream()}
          fallback={
            <div
              style={{
                position: "absolute",
                inset: "0",
                "background-image":
                  "linear-gradient(135deg, #1f2937, #111827, #0b0f19)",
                display: "flex",
                "align-items": "center",
                "justify-content": "center",
                color: COLOURS.textSecondary,
                "font-size": "0.85rem",
              }}
            >
              <span style={{ opacity: "0.6" }}>(video)</span>
            </div>
          }
        >
          {(stream) => (
            <video
              autoplay
              playsinline
              muted
              ref={(el) => {
                el.srcObject = stream();
              }}
              style={{
                position: "absolute",
                inset: "0",
                width: "100%",
                height: "100%",
                "object-fit": "cover",
                // The local self-preview is mirrored (front-camera
                // convention); a remote tile is never mirrored.
                transform: props.participant.isLocal ? "scaleX(-1)" : "none",
              }}
            />
          )}
        </Show>
      </Show>

      {/* Large mute overlay — central icon when the remote is
          muted, so the listener immediately knows their audio
          input is off (matches lakko_me's `.mute-indicator` on
          the video tile). Hidden on the local tile (you know
          you muted yourself). */}
      <Show when={!props.participant.micOn && !props.participant.isLocal}>
        <div
          aria-hidden="true"
          style={{
            position: "absolute",
            top: "50%",
            left: "50%",
            transform: "translate(-50%, -50%)",
            "background-color": "rgba(0,0,0,0.55)",
            color: COLOURS.mutedRed,
            "font-size": props.large ? "2rem" : "1.25rem",
            width: props.large ? "56px" : "36px",
            height: props.large ? "56px" : "36px",
            "border-radius": "50%",
            display: "flex",
            "align-items": "center",
            "justify-content": "center",
            "pointer-events": "none",
          }}
        >
          🔇
        </div>
      </Show>

      {/* Screen-share badge */}
      <Show when={props.participant.screenSharing}>
        <div
          style={{
            position: "absolute",
            top: "8px",
            left: "8px",
            "background-color": "rgba(0,0,0,0.6)",
            color: COLOURS.textPrimary,
            "font-size": "0.7rem",
            padding: "2px 6px",
            "border-radius": "6px",
          }}
        >
          Sharing
        </div>
      </Show>

      {/* Raise-hand badge */}
      <Show when={props.participant.handRaised}>
        <div
          style={{
            position: "absolute",
            top: "8px",
            right: "8px",
            "background-color": "rgba(0,0,0,0.6)",
            "font-size": "1rem",
            padding: "4px 6px",
            "border-radius": "6px",
          }}
        >
          ✋
        </div>
      </Show>

      {/* Bottom strip: name + mute + quality */}
      <div
        style={{
          position: "absolute",
          left: "0",
          right: "0",
          bottom: "0",
          display: "flex",
          "align-items": "center",
          gap: "8px",
          padding: "8px 10px",
          "background-color": "rgba(0,0,0,0.45)",
          color: COLOURS.textPrimary,
          "font-size": "0.85rem",
        }}
      >
        <Show when={!props.participant.micOn}>
          <span
            aria-label="Microphone muted"
            title="Muted"
            style={{ color: COLOURS.mutedRed, "font-size": "0.9rem" }}
          >
            🔇
          </span>
        </Show>
        <span
          style={{
            "white-space": "nowrap",
            overflow: "hidden",
            "text-overflow": "ellipsis",
            flex: "1",
          }}
        >
          {props.participant.displayName ||
            (props.participant.isLocal ? "You" : "Unknown")}
        </span>
        <QualityDot tier={props.participant.quality} />
        <Show when={isPinned()}>
          <span
            title="Pinned"
            style={{ color: COLOURS.accentBlue, "font-size": "0.8rem" }}
          >
            📌
          </span>
        </Show>
      </div>
    </div>
  );
};

// ============================================================================
// PrejoinScreen — camera/mic preview before joining the call.
// Mirror of `preJoinScreen` from lakko_me.
// ============================================================================

const PrejoinScreen: Component<{ onJoin: () => void }> = (props) => {
  const [displayName, setDisplayName] = createSignal("");
  // Local toggles for camera / mic preview. Once the user
  // joins, these copy into the participant entry's flags so the
  // post-join state matches the preview.
  const [previewCamOn, setPreviewCamOn] = createSignal(true);
  const [previewMicOn, setPreviewMicOn] = createSignal(true);

  return (
    <div
      role="region"
      aria-label="Pre-join screen"
      style={{
        position: "fixed",
        inset: "0",
        "background-color": COLOURS.background,
        color: COLOURS.textPrimary,
        display: "flex",
        "flex-direction": "column",
        "align-items": "center",
        "justify-content": "center",
        gap: "20px",
        padding: "24px",
      }}
    >
      <h1 style={{ margin: "0", "font-size": "1.5rem", "font-weight": "600" }}>
        Join Call
      </h1>
      <p style={{ margin: "0", color: COLOURS.textSecondary }}>
        Set up your camera and microphone
      </p>

      {/* Video preview box — same gradient placeholder as the
          in-call tile until the WebRTC pod (3a-pod-followup)
          wires a real <video> element. */}
      <div
        style={{
          width: "min(480px, 90vw)",
          "aspect-ratio": "16 / 9",
          position: "relative",
          "background-color": COLOURS.surface,
          border: `1px solid ${COLOURS.border}`,
          "border-radius": "12px",
          overflow: "hidden",
        }}
      >
        <Show
          when={previewCamOn()}
          fallback={
            <div
              style={{
                position: "absolute",
                inset: "0",
                display: "flex",
                "align-items": "center",
                "justify-content": "center",
                "font-size": "3rem",
                color: COLOURS.textSecondary,
              }}
            >
              📷
            </div>
          }
        >
          <div
            style={{
              position: "absolute",
              inset: "0",
              "background-image":
                "linear-gradient(135deg, #1f2937, #111827, #0b0f19)",
              display: "flex",
              "align-items": "center",
              "justify-content": "center",
              color: COLOURS.textSecondary,
              "font-size": "0.9rem",
            }}
          >
            (camera preview)
          </div>
        </Show>

        {/* Display-name overlay — input over the preview, same
            layout as lakko_me prejoin. */}
        <div
          style={{
            position: "absolute",
            left: "0",
            right: "0",
            bottom: "0",
            padding: "10px 12px",
            "background-color": "rgba(0,0,0,0.5)",
          }}
        >
          <input
            type="text"
            value={displayName()}
            onInput={(e): void => {
              setDisplayName(e.currentTarget.value);
            }}
            placeholder="Your name"
            maxLength={50}
            autocomplete="name"
            aria-label="Display name"
            style={{
              width: "100%",
              padding: "8px 12px",
              "background-color": "rgba(255,255,255,0.08)",
              color: COLOURS.textPrimary,
              border: `1px solid ${COLOURS.border}`,
              "border-radius": "8px",
              "font-size": "0.95rem",
              outline: "none",
            }}
          />
        </div>

        {/* Camera / mic toggle overlays — small floating buttons
            on the preview. */}
        <div
          style={{
            position: "absolute",
            top: "10px",
            right: "10px",
            display: "flex",
            gap: "8px",
          }}
        >
          <button
            type="button"
            aria-label={previewCamOn() ? "Turn off camera" : "Turn on camera"}
            onClick={(): void => {
              setPreviewCamOn(!previewCamOn());
            }}
            style={{
              width: "36px",
              height: "36px",
              "border-radius": "50%",
              border: "none",
              cursor: "pointer",
              "background-color": previewCamOn()
                ? "rgba(0,0,0,0.55)"
                : COLOURS.mutedRed,
              color: COLOURS.textPrimary,
            }}
          >
            {previewCamOn() ? "🎥" : "📷"}
          </button>
          <button
            type="button"
            aria-label={
              previewMicOn() ? "Mute microphone" : "Unmute microphone"
            }
            onClick={(): void => {
              setPreviewMicOn(!previewMicOn());
            }}
            style={{
              width: "36px",
              height: "36px",
              "border-radius": "50%",
              border: "none",
              cursor: "pointer",
              "background-color": previewMicOn()
                ? "rgba(0,0,0,0.55)"
                : COLOURS.mutedRed,
              color: COLOURS.textPrimary,
            }}
          >
            {previewMicOn() ? "🎤" : "🔇"}
          </button>
        </div>
      </div>

      <button
        type="button"
        onClick={(): void => {
          // Carry preview toggles + display name into the
          // post-join participant state, then transition.
          callActions.setMic(previewMicOn());
          callActions.setCamera(previewCamOn());
          callActions.join();
          props.onJoin();
        }}
        style={{
          padding: "12px 32px",
          "background-color": COLOURS.accentBlue,
          color: COLOURS.textPrimary,
          border: "none",
          "border-radius": "10px",
          "font-size": "1rem",
          "font-weight": "600",
          cursor: "pointer",
          "box-shadow": "0 2px 8px rgba(59, 130, 246, 0.4)",
        }}
      >
        Join
      </button>
    </div>
  );
};

// ============================================================================
// WaitingMessage — overlay shown when the local participant is
// the only one in the call (no remote yet to render in active
// speaker / gallery).
// ============================================================================

const WaitingMessage: Component = () => (
  <div
    role="status"
    aria-live="polite"
    style={{
      position: "absolute",
      top: "50%",
      left: "50%",
      transform: "translate(-50%, -50%)",
      padding: "16px 24px",
      "background-color": "rgba(26, 26, 26, 0.9)",
      border: `1px solid ${COLOURS.border}`,
      "border-radius": "12px",
      color: COLOURS.textPrimary,
      "font-size": "0.95rem",
      "z-index": "5",
      "text-align": "center",
    }}
  >
    Waiting for participant…
  </div>
);

// ============================================================================
// ControlsBar — bottom action bar
// ============================================================================

interface ButtonProps {
  label: string;
  glyph: string;
  active: boolean;
  destructive?: boolean;
  onClick: () => void;
}

const CircleButton: Component<ButtonProps> = (props) => (
  <button
    type="button"
    aria-label={props.label}
    title={props.label}
    onClick={props.onClick}
    style={{
      width: "52px",
      height: "52px",
      "border-radius": "50%",
      border: "none",
      cursor: "pointer",
      "font-size": "1.25rem",
      "background-color":
        props.destructive === true
          ? COLOURS.endCallRed
          : props.active
            ? COLOURS.accentBlue
            : COLOURS.surface,
      color: COLOURS.textPrimary,
      "box-shadow":
        props.destructive === true
          ? "0 2px 8px rgba(220, 38, 38, 0.4)"
          : "0 1px 3px rgba(0,0,0,0.5)",
      transition: "background-color 120ms ease",
    }}
  >
    {props.glyph}
  </button>
);

const ControlsBar: Component = () => {
  const local = (): Participant | null =>
    callState.participants().find((p) => p.isLocal) ?? null;

  return (
    <div
      role="toolbar"
      aria-label="Call controls"
      style={{
        display: "flex",
        gap: "12px",
        "justify-content": "center",
        "align-items": "center",
        padding: "14px 16px",
        "background-color": COLOURS.surface,
        "border-top": `1px solid ${COLOURS.border}`,
      }}
    >
      <CircleButton
        label={local()?.micOn === true ? "Mute microphone" : "Unmute microphone"}
        glyph={local()?.micOn === true ? "🎤" : "🔇"}
        active={local()?.micOn === false}
        onClick={(): void => callActions.setMic(local()?.micOn !== true)}
      />
      <CircleButton
        label={local()?.camOn === true ? "Stop camera" : "Start camera"}
        glyph={local()?.camOn === true ? "🎥" : "📷"}
        active={local()?.camOn === false}
        onClick={(): void => callActions.setCamera(local()?.camOn !== true)}
      />
      {/* Switch front/back camera — mobile only. Surfaces only
          when the platform reported ≥ 2 cameras (typically
          smartphone front + back). On a laptop with a single
          built-in cam, hidden. Mirror of lakko_me's
          `switchCameraBtn`. */}
      <Show when={callState.hasMultipleCameras() && local()?.camOn === true}>
        <CircleButton
          label="Switch camera"
          glyph="🔄"
          active={false}
          onClick={(): void => {
            // The WebRTC pod (3a-pod-followup) owns the actual
            // facingMode toggle on the local MediaStreamTrack;
            // this UI just emits the intent. Until the pod
            // lands, the click is a no-op informational beat.
            // eslint-disable-next-line no-console
            console.info("call ui: switch camera requested");
          }}
        />
      </Show>
      <CircleButton
        label="Participants"
        glyph="👥"
        active={callState.participantListOpen()}
        onClick={(): void =>
          callActions.setParticipantListOpen(!callState.participantListOpen())
        }
      />
      <CircleButton
        label={
          local()?.handRaised === true ? "Lower hand" : "Raise hand"
        }
        glyph="✋"
        active={local()?.handRaised === true}
        onClick={(): void =>
          callActions.setHandRaised(local()?.handRaised !== true)
        }
      />
      <CircleButton
        label={
          local()?.screenSharing === true
            ? "Stop sharing screen"
            : "Share screen"
        }
        glyph="🖥️"
        active={local()?.screenSharing === true}
        onClick={(): void =>
          callActions.setScreenSharing(local()?.screenSharing !== true)
        }
      />
      <ChatToggleButton />
      <CircleButton
        label="Settings"
        glyph="⚙️"
        active={callState.settingsOpen()}
        onClick={(): void => callActions.setSettingsOpen(!callState.settingsOpen())}
      />
      <CircleButton
        label="End call"
        glyph="📞"
        active={false}
        destructive={true}
        onClick={(): void => callActions.endCall()}
      />
    </div>
  );
};

/** Chat toggle — CircleButton variant with an unread-count
 *  badge in the top-right corner. The badge surfaces only
 *  when there is something to draw the user's attention to
 *  AND the panel itself is closed. */
const ChatToggleButton: Component = () => (
  <div style={{ position: "relative" }}>
    <CircleButton
      label={
        callState.chatOpen()
          ? "Close chat"
          : callState.chatUnreadCount() > 0
            ? `Chat (${callState.chatUnreadCount()} unread)`
            : "Chat"
      }
      glyph="💬"
      active={callState.chatOpen()}
      onClick={(): void => callActions.setChatOpen(!callState.chatOpen())}
    />
    <Show when={callState.chatUnreadCount() > 0 && !callState.chatOpen()}>
      <span
        aria-hidden="true"
        style={{
          position: "absolute",
          top: "-2px",
          right: "-2px",
          "background-color": COLOURS.endCallRed,
          color: "#fff",
          "border-radius": "10px",
          "min-width": "18px",
          height: "18px",
          padding: "0 5px",
          "font-size": "11px",
          "font-weight": "700",
          display: "inline-flex",
          "align-items": "center",
          "justify-content": "center",
          "pointer-events": "none",
        }}
      >
        {callState.chatUnreadCount() > 99 ? "99+" : callState.chatUnreadCount()}
      </span>
    </Show>
  </div>
);

// ============================================================================
// SettingsPanel — gear-menu overlay (S-3-A-settings)
// ============================================================================
//
// Centred modal on desktop / right side panel on tablet /
// bottom-sheet on mobile via a `@media` shorthand on the
// outer container. Holds three device pickers + five
// processing toggles. The device pickers list the OS's real
// capture / playback devices via `refreshCallDevices` (below),
// pulled when the panel opens and on hot-plug (`devicechange`).

/** Enumerate the OS's real capture / playback devices and publish them into
 *  the shared call store so the gear-menu pickers list the user's actual
 *  microphones, cameras, and speakers (replacing the empty placeholder list).
 *  Device labels populate only once `getUserMedia` has been granted (i.e.
 *  during a live call); before that the browser returns stable ids with blank
 *  labels, which the picker renders with a generic fallback. */
async function refreshCallDevices(): Promise<void> {
  const md =
    typeof navigator !== "undefined" ? navigator.mediaDevices : undefined;
  if (!md?.enumerateDevices) {
    return;
  }
  try {
    const infos = await md.enumerateDevices();
    const devices: CallDevice[] = infos
      .filter(
        (d) =>
          d.kind === "audioinput" ||
          d.kind === "videoinput" ||
          d.kind === "audiooutput",
      )
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
    const md =
      typeof navigator !== "undefined" ? navigator.mediaDevices : undefined;
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
      role="dialog"
      aria-modal="true"
      aria-label="Call settings"
      style={{
        position: "fixed",
        inset: "0",
        "background-color": "rgba(0,0,0,0.55)",
        "z-index": "10",
        display: "flex",
        "align-items": "center",
        "justify-content": "center",
        padding: "16px",
      }}
      onClick={(e): void => {
        // Click-out to dismiss — only when the backdrop itself
        // is the target, not the inner card.
        if (e.target === e.currentTarget) {
          callActions.setSettingsOpen(false);
        }
      }}
    >
      <div
        style={{
          width: "min(440px, 100%)",
          "max-height": "min(720px, 90vh)",
          "overflow-y": "auto",
          "background-color": COLOURS.surface,
          color: COLOURS.textPrimary,
          "border-radius": "12px",
          padding: "20px 22px",
          display: "flex",
          "flex-direction": "column",
          gap: "18px",
        }}
      >
        <div
          style={{
            display: "flex",
            "justify-content": "space-between",
            "align-items": "center",
          }}
        >
          <h2 style={{ margin: "0", "font-size": "18px", "font-weight": "600" }}>
            Settings
          </h2>
          <button
            type="button"
            aria-label="Close settings"
            onClick={(): void => callActions.setSettingsOpen(false)}
            style={{
              background: "none",
              border: "none",
              color: COLOURS.textSecondary,
              "font-size": "22px",
              cursor: "pointer",
              padding: "4px 8px",
            }}
          >
            ×
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

        <hr
          style={{
            border: "none",
            "border-top": `1px solid ${COLOURS.border}`,
            margin: "4px 0",
          }}
        />

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
          label="Mirror local preview"
          checked={callState.settings().mirrorLocal}
          onChange={(v): void => callActions.setMirrorLocal(v)}
        />
        <SettingsToggle
          label="Low-data mode"
          description="Caps video at 360p — useful on metered mobile connections."
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
    <label
      style={{
        display: "flex",
        "flex-direction": "column",
        gap: "6px",
        "font-size": "13px",
        color: COLOURS.textSecondary,
      }}
    >
      <span style={{ "font-weight": "500", color: COLOURS.textPrimary }}>
        {props.label}
      </span>
      <select
        value={props.selectedId ?? ""}
        onChange={(e): void => {
          const value = e.currentTarget.value;
          props.onPick(value === "" ? null : value);
        }}
        style={{
          padding: "8px 10px",
          "background-color": COLOURS.background,
          color: COLOURS.textPrimary,
          border: `1px solid ${COLOURS.border}`,
          "border-radius": "8px",
          "font-size": "14px",
        }}
      >
        <option value="">System default</option>
        <For each={available()}>
          {(d): JSX.Element => (
            <option value={d.id}>{d.label || `(${d.kind})`}</option>
          )}
        </For>
      </select>
      <Show when={available().length === 0}>
        <span style={{ "font-size": "12px", color: COLOURS.textSecondary }}>
          No devices reported yet. They appear here once you grant
          microphone / camera permission.
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
  <label
    style={{
      display: "flex",
      "align-items": "flex-start",
      gap: "10px",
      cursor: "pointer",
      "font-size": "14px",
    }}
  >
    <input
      type="checkbox"
      checked={props.checked}
      onChange={(e): void => props.onChange(e.currentTarget.checked)}
      style={{ "margin-top": "3px" }}
    />
    <span style={{ display: "flex", "flex-direction": "column", gap: "2px" }}>
      <span style={{ color: COLOURS.textPrimary }}>{props.label}</span>
      <Show when={props.description !== undefined}>
        <span style={{ "font-size": "12px", color: COLOURS.textSecondary }}>
          {props.description}
        </span>
      </Show>
    </span>
  </label>
);

// ============================================================================
// ParticipantList drawer
// ============================================================================

const ParticipantList: Component = () => (
  <Show when={callState.participantListOpen()}>
    <aside
      aria-label="Participant list"
      style={{
        position: "absolute",
        right: "0",
        top: "0",
        bottom: "0",
        width: "320px",
        "background-color": COLOURS.surface,
        "border-left": `1px solid ${COLOURS.border}`,
        "overflow-y": "auto",
        padding: "16px",
        "z-index": "10",
      }}
    >
      <div
        style={{
          display: "flex",
          "justify-content": "space-between",
          "align-items": "center",
          "margin-bottom": "12px",
        }}
      >
        <h2
          style={{
            margin: "0",
            "font-size": "1rem",
            color: COLOURS.textPrimary,
          }}
        >
          Participants ({callState.participants().length})
        </h2>
        <button
          type="button"
          aria-label="Close participant list"
          onClick={(): void => callActions.setParticipantListOpen(false)}
          style={{
            background: "none",
            border: "none",
            color: COLOURS.textSecondary,
            cursor: "pointer",
            "font-size": "1.25rem",
          }}
        >
          ✕
        </button>
      </div>
      <ul style={{ "list-style": "none", margin: "0", padding: "0" }}>
        <For each={callState.participants()}>
          {(p) => (
            <li
              style={{
                display: "flex",
                "align-items": "center",
                gap: "10px",
                padding: "10px 8px",
                "border-bottom": `1px solid ${COLOURS.border}`,
                color: COLOURS.textPrimary,
              }}
            >
              <QualityDot tier={p.quality} />
              <span style={{ flex: "1" }}>
                {p.displayName || (p.isLocal ? "You" : "Unknown")}
              </span>
              <Show when={p.handRaised}>
                <span title="Hand raised" aria-label="Hand raised">
                  ✋
                </span>
              </Show>
              <Show when={!p.micOn}>
                <span
                  title="Muted"
                  aria-label="Muted"
                  style={{ color: COLOURS.mutedRed }}
                >
                  🔇
                </span>
              </Show>
              <Show when={p.screenSharing}>
                <span title="Sharing screen" aria-label="Sharing screen">
                  🖥️
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
// Banner — connecting / reconnecting / ended overlays
// ============================================================================

const PhaseBanner: Component = () => (
  <Show when={callState.phase().kind !== "connected"}>
    <div
      role="status"
      style={{
        position: "absolute",
        top: "12px",
        left: "50%",
        transform: "translateX(-50%)",
        "background-color":
          callState.phase().kind === "reconnecting"
            ? COLOURS.qualityYellow
            : callState.phase().kind === "ended"
              ? COLOURS.endCallRed
              : COLOURS.accentBlue,
        color: "#0F0F0F",
        padding: "6px 14px",
        "border-radius": "6px",
        "font-size": "0.85rem",
        "font-weight": "600",
        "z-index": "20",
      }}
    >
      {phaseText(callState.phase())}
    </div>
  </Show>
);

function phaseText(
  p: ReturnType<typeof callState.phase>,
): string {
  switch (p.kind) {
    case "prejoin":
      return "";
    case "connecting":
      return "Connecting…";
    case "reconnecting":
      return `Reconnecting — ${p.reason}`;
    case "ended":
      return "Call ended";
    case "connected":
      return "";
  }
}

// ============================================================================
// CallEndedOverlay — dim + auto-navigate after 3s
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
    <div
      style={{
        position: "absolute",
        inset: "0",
        "background-color": "rgba(15, 15, 15, 0.85)",
        "z-index": "30",
        display: "flex",
        "flex-direction": "column",
        "align-items": "center",
        "justify-content": "center",
        color: COLOURS.textPrimary,
        gap: "12px",
      }}
    >
      <h1 style={{ margin: "0", "font-size": "2rem" }}>Call ended</h1>
      <p style={{ margin: "0", color: COLOURS.textSecondary }}>
        Returning to your chats…
      </p>
    </div>
  );
};

// ============================================================================
// ChatPanel — in-call chat (S-3-A-chat)
// ============================================================================
//
// Right-rail panel on desktop (320 px), full-height. Transport
// is text-only; emoji, attachments, and reactions are scoped
// out of this sprint. System events (peer joined / left) are
// rendered inline as grey centered lines.

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
      // Delay one tick — the input is being mounted in the
      // same microtask.
      queueMicrotask((): void => inputRef?.focus());
    }
  });

  const submit = (): void => {
    callActions.sendChatMessage(callState.composeText());
  };

  return (
    <Show when={callState.chatOpen()}>
      <aside
        aria-label="Call chat"
        style={{
          position: "absolute",
          right: "0",
          top: "0",
          bottom: "0",
          width: "320px",
          "background-color": COLOURS.surface,
          "border-left": `1px solid ${COLOURS.border}`,
          display: "flex",
          "flex-direction": "column",
          "z-index": "10",
        }}
      >
        <header
          style={{
            display: "flex",
            "align-items": "center",
            "justify-content": "space-between",
            padding: "14px 16px",
            "border-bottom": `1px solid ${COLOURS.border}`,
          }}
        >
          <span
            style={{
              "font-weight": "600",
              "font-size": "15px",
              color: COLOURS.textPrimary,
            }}
          >
            Chat
          </span>
          <button
            type="button"
            aria-label="Close chat"
            onClick={(): void => callActions.setChatOpen(false)}
            style={{
              background: "none",
              border: "none",
              color: COLOURS.textSecondary,
              "font-size": "20px",
              cursor: "pointer",
              padding: "2px 6px",
            }}
          >
            ×
          </button>
        </header>

        <div
          ref={scrollRef}
          style={{
            flex: "1",
            "overflow-y": "auto",
            padding: "12px 14px",
            display: "flex",
            "flex-direction": "column",
            gap: "10px",
          }}
        >
          <Show
            when={callState.chatMessages().length > 0}
            fallback={
              <div
                style={{
                  color: COLOURS.textSecondary,
                  "font-size": "13px",
                  "text-align": "center",
                  "padding-top": "32px",
                  "line-height": "1.5",
                }}
              >
                No messages yet. Anything you send is end-to-end encrypted
                inside this call's MLS group — the server cannot read it.
              </div>
            }
          >
            <For each={callState.chatMessages()}>
              {(msg): JSX.Element => <ChatMessageRow msg={msg} />}
            </For>
          </Show>
        </div>

        <form
          style={{
            display: "flex",
            gap: "8px",
            padding: "10px 12px",
            "border-top": `1px solid ${COLOURS.border}`,
          }}
          onSubmit={(e): void => {
            e.preventDefault();
            submit();
          }}
        >
          <input
            ref={inputRef}
            type="text"
            placeholder="Send a message"
            aria-label="Compose chat message"
            value={callState.composeText()}
            onInput={(e): void =>
              callActions.setComposeText(e.currentTarget.value)
            }
            style={{
              flex: "1",
              padding: "8px 10px",
              "background-color": COLOURS.background,
              color: COLOURS.textPrimary,
              border: `1px solid ${COLOURS.border}`,
              "border-radius": "8px",
              "font-size": "14px",
            }}
          />
          <button
            type="submit"
            disabled={callState.composeText().trim().length === 0}
            aria-label="Send chat message"
            style={{
              padding: "8px 14px",
              "background-color": COLOURS.accentBlue,
              color: "#fff",
              border: "none",
              "border-radius": "8px",
              cursor:
                callState.composeText().trim().length === 0
                  ? "not-allowed"
                  : "pointer",
              opacity:
                callState.composeText().trim().length === 0 ? "0.5" : "1",
              "font-size": "14px",
              "font-weight": "600",
            }}
          >
            Send
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
    <Show
      when={!isSystem()}
      fallback={
        <div
          style={{
            color: COLOURS.textSecondary,
            "font-size": "12px",
            "text-align": "center",
            "font-style": "italic",
          }}
        >
          {props.msg.text}
        </div>
      }
    >
      <div
        style={{
          display: "flex",
          "flex-direction": "column",
          gap: "2px",
        }}
      >
        <div
          style={{
            display: "flex",
            gap: "8px",
            "align-items": "baseline",
            "font-size": "12px",
            color: COLOURS.textSecondary,
          }}
        >
          <span style={{ color: COLOURS.accentBlue, "font-weight": "600" }}>
            {props.msg.senderName}
          </span>
          <span>{time()}</span>
        </div>
        <div
          style={{
            color: COLOURS.textPrimary,
            "font-size": "14px",
            "line-height": "1.4",
            "word-break": "break-word",
          }}
        >
          {props.msg.text}
        </div>
      </div>
    </Show>
  );
};

// ============================================================================
// Mock data — seeds the screen with placeholder participants until
// the WebRTC pod (3a-pod-followup) wires real ones in.
// ============================================================================

function seedMockParticipants(): void {
  // Mock E2EE — once the WebRTC pod (3a-pod-followup) wires the
  // real CallKey, it calls callActions.setE2ee with the actual
  // BLAKE3 fingerprint of the seed.
  callActions.setE2ee({
    active: true,
    shortFingerprint: "3F7A B2C1 9E0D",
  });
  // Mock connection metrics — quality sampler feeds real numbers
  // post-pod-landing.
  callActions.setConnectionDetail({
    rttMs: 45,
    downBps: 720_000,
    upBps: 380_000,
    connectionType: "direct",
  });
  // Mobile probe — production code calls
  // `navigator.mediaDevices.enumerateDevices()` and counts how
  // many videoinput kinds came back. Hard-coded false here so
  // the desktop preview doesn't show the switch-camera button.
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
  // Synthesise the "connected" transition the way the WebRTC pod
  // will once it lands — mock answers after a short delay.
  window.setTimeout((): void => {
    callActions.setPhase({ kind: "connected" });
  }, 600);
}

// ============================================================================
// Top-level route
// ============================================================================

/**
 * In-call screen.
 *
 * Two modes:
 *   * Standalone design route (`/call/:id`, no props) — seeds mock
 *     participants on Join; the reference UI for the design system.
 *   * Embedded over a live chat (G3-3-web-session-bind) — `embedded`
 *     is set and the parent (Chat.tsx) drives `callState` from the
 *     real `CallSession`. In this mode the screen skips the
 *     mount-time `reset()` (the parent owns lifecycle), routes
 *     end-call + the ended-overlay dismissal through `onExit`
 *     instead of navigating home, and hides the text E2EE pill
 *     (the 1×1 web path is P2P DTLS-SRTP, not SFrame — the pill
 *     would mislead, and a text chip during the call is poor UX).
 */
const Call: Component<{
  embedded?: boolean;
  onExit?: () => void;
  /** SFU group-call only: resolve a roster participant id to its live
   *  remote `MediaStream` so the tile can render real video. Omitted on
   *  the 1×1 and mock paths (tiles fall back to the avatar / placeholder). */
  remoteStreamFor?:
    | ((participantId: string) => MediaStream | undefined)
    | undefined;
  /** 1×1 P2P only: the local capture `MediaStream`, for the LOCAL
   *  tile's self-preview `<video>`. Omitted on the SFU / mock paths. */
  localStream?: (() => MediaStream | undefined) | undefined;
}> = (props) => {
  const navigate = useNavigate();

  // Sound cues — one controller per Call mount; disposed on
  // unmount so cached <audio> elements + their decoders are
  // released cleanly.
  const sounds = new CallSoundController();

  onMount(() => {
    // Embedded mode: the parent already drove `callState` to a
    // live phase before mounting us — resetting would wipe it.
    if (props.embedded !== true) {
      callActions.reset();
      // Note: `reset()` leaves us in `prejoin` phase. The mock
      // participants + E2EE state get seeded only after the user
      // clicks "Join" on the prejoin screen (handled by
      // `seedMockParticipants` inside the onJoin callback below).
    }
  });
  onCleanup(() => {
    sounds.dispose();
    // Embedded mode: the parent owns the call lifecycle and resets
    // `callState` when the session reaches `done`.
    if (props.embedded !== true) {
      callActions.reset();
    }
  });

  // Phase → sound mapping. Mirrors the same trigger map on
  // iOS / Android so the user-facing audio is identical
  // across platforms.
  //   prejoin     → silence
  //   connecting  → ringback-outgoing loop ("we are calling")
  //   connected   → stop ringback, play join one-shot
  //   reconnecting→ stop everything (visual banner conveys it)
  //   ended       → stop everything, play leave one-shot
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

  // Peer-arrival / peer-departure cues + chat system events.
  // Track the remote participant count + the id set; on each
  // delta fire the right audio cue AND post a system event
  // into the chat ("Alice joined", "Bob left"). Local
  // participant is excluded because they aren't a "peer"
  // from the user's perspective.
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
          // Display name has already been dropped from state by
          // the time we observe the deletion; the WebRTC pod
          // will preserve the last-known name in a future
          // refinement (out of scope for S-3-A-chat).
        }
      }
    }
    lastRemoteIds = remoteIds;
  });

  // Compute whether we're alone in the call (no remote yet).
  // The WaitingMessage overlay fires off this.
  const isAlone = (): boolean =>
    callState.participants().filter((p) => !p.isLocal).length === 0;

  // Auto-navigate back after the "Call ended" overlay's 3s
  // timer fires.
  createEffect(() => {
    if (callState.phase().kind === "ended") {
      // Effect runs once on transition — the overlay's own
      // onCleanup handles its setTimeout, this just wires the
      // navigation hook.
    }
  });

  return (
    <Show
      when={callState.phase().kind !== "prejoin"}
      fallback={
        <PrejoinScreen
          onJoin={(): void => {
            // Synthesise the "connected" transition the way
            // the WebRTC pod will once it lands.
            seedMockParticipants();
            window.setTimeout((): void => {
              callActions.setPhase({ kind: "connected" });
            }, 400);
          }}
        />
      }
    >
      <div
        style={{
          position: "fixed",
          inset: "0",
          "background-color": COLOURS.background,
          color: COLOURS.textPrimary,
          display: "flex",
          "flex-direction": "column",
        }}
      >
        <PhaseBanner />

        {/* Top status bar — E2EE indicator (left) +
            connection-quality dot with popover (right). The
            phase banner floats over the centre. */}
        <header
          style={{
            display: "flex",
            "justify-content": "space-between",
            "align-items": "center",
            padding: "10px 14px",
            "background-color": COLOURS.surface,
            "border-bottom": `1px solid ${COLOURS.border}`,
            "z-index": "5",
          }}
        >
          {/* G3-3-web-session-bind — the text E2EE pill is hidden in
              the embedded (live-call) view: the 1×1 web path is P2P
              DTLS-SRTP, not SFrame, so a "secured" text chip would
              mislead. It stays on the standalone design route. */}
          <Show when={props.embedded !== true} fallback={<span />}>
            <E2eePill />
          </Show>
          <QualityIndicator />
        </header>

        <main
          onClick={(): void => {
            // Tap anywhere on the main area → close the quality
            // popover. Standard "tap-outside-to-close" UX for
            // floating panels.
            if (callState.qualityPopoverOpen()) {
              callActions.setQualityPopoverOpen(false);
            }
          }}
          style={{
            flex: "1",
            position: "relative",
            display: "flex",
            "flex-direction": "column",
            gap: "12px",
            padding: "16px",
            "overflow-y": "auto",
          }}
        >
          <Show when={isAlone() && callState.phase().kind === "connected"}>
            <WaitingMessage />
          </Show>

          <Show when={activeSpeaker() !== null}>
            <div
              style={{
                "max-width": "1080px",
                margin: "0 auto",
                width: "100%",
              }}
            >
              <ParticipantTile
                participant={activeSpeaker() as Participant}
                large={true}
                onPin={(id): void =>
                  callActions.pin(id === "" ? null : id)
                }
                remoteStreamFor={props.remoteStreamFor}
                localStream={props.localStream}
              />
            </div>
          </Show>

          <div
            style={{
              display: "grid",
              "grid-template-columns":
                "repeat(auto-fill, minmax(180px, 1fr))",
              gap: "10px",
              "max-width": "1080px",
              margin: "0 auto",
              width: "100%",
            }}
          >
            <For each={galleryParticipants()}>
              {(p) => (
                <ParticipantTile
                  participant={p}
                  large={false}
                  onPin={(id): void =>
                    callActions.pin(id === "" ? null : id)
                  }
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
              // Embedded: hand control back to the parent (Chat
              // tears the overlay down + resets state). Standalone
              // design route: navigate home as before.
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
