// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * In-call UI state — Solid signals (Phase 3c6-web).
 *
 * Pure data layer: holds the participants list, mic / camera /
 * raise-hand toggles, active-speaker pin, connection-quality
 * per-participant, and the call lifecycle phase. UI components
 * subscribe via `createSignal` accessors; mutations go through
 * the `actions` object.
 *
 * No WebRTC integration here — this is a pure data layer. The
 * live media path runs in the call sessions (`calls/call-session.ts`
 * for 1×1, `calls/sfu-call-session.ts` for the SFU); their adapters
 * (`Chat.tsx`, `calls/sfu-call-ui.ts`) drive this store through the
 * `actions` object. The standalone design route seeds the same store
 * with mock data (`Call.tsx::seedMockParticipants`) when no session is
 * bound.
 */

import { createSignal, createMemo, type Accessor } from "solid-js";
import {
  featuredParticipant as pickFeatured,
  galleryParticipants as pickGallery,
} from "../calls/call-screen-model";

import type { VideoPublishState } from "../calls/call-session";

// ============================================================================
// Types
// ============================================================================

/** Connection quality tier, ranked left → right. Mirrors the
 *  Rust-side [`lakko_calls::quality::QualityLevel`]; the UI
 *  collapses to three glyphs (excellent → green, good/fair →
 *  yellow, poor/bad → red). */
export type QualityTier = "excellent" | "good" | "fair" | "poor" | "bad";

/** Map a QualityTier to the three-colour glyph tier the UI shows. */
export function qualityColour(tier: QualityTier): "green" | "yellow" | "red" {
  switch (tier) {
    case "excellent":
      return "green";
    case "good":
    case "fair":
      return "yellow";
    case "poor":
    case "bad":
      return "red";
  }
}

/**
 * A single participant in the call. Local + remote share the same shape; the
 * local one has `isLocal: true` and is filtered out of certain views (e.g. you
 * don't appear in the gallery next to your own controls).
 *
 * `handRaised` and `screenSharing` used to live here and are gone with the
 * `docs/ux/calls.md` §4.1 control row: nothing on any platform ever set them to
 * `true` — the local toggles wrote a flag no peer could see and no remote path
 * reported one — so the tile badges that read them could never appear. §9 puts
 * screen sharing out of scope, and a control that only lies to its own device is
 * the "never pretend" rule of §1 broken in miniature.
 */
export interface Participant {
  /** Stable id — for local: random; for remote: MLS leaf index
   *  serialised as `"leaf-<n>"`. UI uses this as the `id` for
   *  Solid's For-loop key. */
  readonly id: string;
  /** Display name. Local can be empty (we show "You"); remote
   *  comes from the contact card lookup. */
  readonly displayName: string;
  /** Whether this is the local participant. */
  readonly isLocal: boolean;
  /** Mic enabled (`false` = muted). */
  readonly micOn: boolean;
  /** Camera enabled. `false` = camera-off (avatar tile shown
   *  instead of video). */
  readonly camOn: boolean;
  /** Connection quality as last reported by the bandwidth
   *  controller. */
  readonly quality: QualityTier;
  /** `true` when this participant is the currently-loudest
   *  speaker (LiveKit metadata). The UI promotes them to the
   *  large active-speaker tile. */
  readonly speaking: boolean;
}

/**
 * Call lifecycle phase, as `docs/ux/calls.md` §4's state table names them.
 *
 * `idle` is "no call on this device" — the call screen is not mounted, so it is
 * the reset value and never a rendered state. It replaces the old `prejoin`,
 * which was a *screen* masquerading as a phase: the pre-call surface now exists
 * only for a video call, only as §3.2's camera check, and that check runs
 * *before* any call object exists, so it has nothing to do with a call's
 * lifecycle.
 *
 * `calling` and `connecting` are separate because they are separate facts and
 * the store used to collapse them: a call ringing on the other person's phone
 * reported `Connecting...`, which described the network rather than the call.
 */
export type CallPhase =
  | { readonly kind: "idle" }
  | { readonly kind: "calling" }
  | { readonly kind: "connecting" }
  | { readonly kind: "connected" }
  | { readonly kind: "reconnecting"; readonly reason: string }
  | { readonly kind: "ended"; readonly endedAt: number };

/** Who is speaking, as the group stage reads it. */
export interface CallSpeech {
  /** The one person holding the floor now, or `null`. */
  readonly activeSpeakerId: string | null;
  /** When each person last held the floor, by participant id. */
  readonly lastSpokeAtMs: Readonly<Record<string, number>>;
}

/** End-to-end-encryption indicator state. Mirror of
 *  `e2eeIndicator` / `e2eeFingerprint` / `e2eeWarning` in
 *  `lakko_me`. Lakko's MLS exporter produces a per-call
 *  fingerprint via `call_key_fingerprint(&CallKey)` (see
 *  `lakko-calls::sframe`); the platform displays it so the user
 *  can verify out-of-band against the same fingerprint on the
 *  other side. */
export interface E2eeState {
  /** Is the call cryptographically established? `false` during
   *  prejoin / connecting (no MLS group yet). */
  readonly active: boolean;
  /** First 12 hex chars of `call_key_fingerprint` (BLAKE3 of the
   *  CallKey seed). Same digits both sides see — Signal-style
   *  short safety number for OOB verification. `null` while
   *  `active === false`. */
  readonly shortFingerprint: string | null;
}

/** Connection-detail panel content (expanded quality popover).
 *  Mirror of the `connectionQualityIndicator` expansion in
 *  `lakko_me`. */
export interface ConnectionDetail {
  /** Round-trip time in milliseconds. `null` when unavailable. */
  readonly rttMs: number | null;
  /** Downstream bitrate in bits per second. */
  readonly downBps: number | null;
  /** Upstream bitrate. */
  readonly upBps: number | null;
  /** `direct` = peer-to-peer over STUN-discovered candidates,
   *  `relay` = falling back through TURN. Important for the
   *  user to see — relay means higher latency + observable to
   *  the TURN operator. */
  readonly connectionType: "direct" | "relay" | "unknown";
}

/** A capture / playback device the OS exposes to the call.
 *  Mirrors the relevant subset of MediaDeviceInfo (web),
 *  AVCaptureDevice / AVAudioSessionPortDescription (iOS), and
 *  AudioDeviceInfo / CameraManager device entries (Android).
 *  Same shape on all three platforms so the picker UI can be
 *  expressed once per platform without per-OS branching. */
export interface CallDevice {
  /** Stable platform-specific id. Opaque to the UI. */
  readonly id: string;
  /** Human-readable label ("MacBook Pro Microphone",
   *  "Back camera", "AirPods Pro"). May be empty when the OS
   *  has not granted device-label permission yet (web's
   *  enumerateDevices returns blanks before getUserMedia
   *  succeeds). */
  readonly label: string;
  /** Device class — audio capture / video capture / audio
   *  playback. */
  readonly kind: "audioinput" | "videoinput" | "audiooutput";
}

/** In-call audio / video settings (S-3-A-settings).
 *
 *  Drives:
 *  - the gear-menu device pickers (one per `kind`), and
 *  - the processing toggles applied to the local capture chain
 *    (noise suppression, echo cancellation, auto-gain) — which
 *    in WebRTC land are constraint flags on getUserMedia.
 *
 *  `selected*` fields are `null` until the user picks a device
 *  explicitly; the OS-default device is used in the meantime.
 *
 *  The capture-processing toggles (noise suppression, echo cancellation,
 *  automatic gain, preview mirroring, low-data mode) are gone with the
 *  `docs/ux/calls.md` §4.1 redesign. Two reasons, and the second is the one
 *  that matters: they were rendered as a settings list of checkboxes inside a
 *  live call, which §4.1 forbids outright — and no code on any platform ever
 *  read them, so every one of those checkboxes was a preference the user could
 *  set and nothing would honour. */
export interface CallSettings {
  /** Currently selected microphone (`null` → OS default). */
  readonly selectedAudioInput: string | null;
  /** Currently selected camera (`null` → OS default). */
  readonly selectedVideoInput: string | null;
  /** Currently selected speaker / headset (`null` → OS
   *  default). On iOS this is a routing hint rather than a
   *  hard pick — AVAudioSession overrides for BT / wired. */
  readonly selectedAudioOutput: string | null;
  /** Enumerated devices the OS exposes. Populated by the call
   *  settings panel from a real
   *  `navigator.mediaDevices.enumerateDevices()` pull (on open
   *  + on `devicechange`); empty until the panel first runs it.
   *  The 1×1 `CallSession` honours a `selected*` pick at capture
   *  time + switches the live sender mid-call
   *  (`switchAudioInput`/`switchVideoInput`); the SFU multi-party
   *  path drives capture through its own session. */
  readonly availableDevices: readonly CallDevice[];
}

/** Default `CallSettings` — no explicit device picks, nothing enumerated. */
const DEFAULT_SETTINGS: CallSettings = {
  selectedAudioInput: null,
  selectedVideoInput: null,
  selectedAudioOutput: null,
  availableDevices: [],
};

/** Top-level UI state. Internal — components read via the
 *  accessors below. */
interface CallStateInternal {
  readonly phase: Accessor<CallPhase>;
  /**
   * When media first flowed, in unix milliseconds, or `null` before it has.
   *
   * The call screen's one status line is the running time once connected
   * (`docs/ux/calls.md` §4), so the duration has to start from a moment the
   * store owns rather than from when a component happened to mount. Set once
   * per call and NOT reset by a `reconnecting` round trip — a call that
   * stumbles for four seconds is still the same call, and a timer that restarts
   * would tell the user otherwise. Survives into `ended` so the ended line can
   * state how long the call was.
   */
  readonly connectedAt: Accessor<number | null>;
  /**
   * Whether the microphone should be live when the call's local participant is
   * first built.
   *
   * §3.2's camera check can be left muted, and the call has to start that way.
   * Every `localParticipant()` constructor used to hardcode `micOn: true`, so a
   * muted start was silently unmuted the moment the session reported its first
   * state.
   */
  readonly pendingMicOn: Accessor<boolean>;
  readonly participants: Accessor<Participant[]>;
  /** Id of the pinned participant. `null` = follow active
   *  speaker. */
  readonly pinnedId: Accessor<string | null>;
  /** The group layout the person chose for this call, or `null` for the
   *  default (`docs/ux/group-call-grid.md` section 3). */
  readonly layoutChoice: Accessor<"grid" | "focus" | null>;
  /** Who holds the floor and when each person last did, worked out on this
   *  device from the audio it plays (section 7.3). */
  readonly speech: Accessor<CallSpeech>;
  /** Drawer state — participant list visible / hidden. */
  readonly participantListOpen: Accessor<boolean>;
  /** E2EE indicator state — pulled from the call's MLS exporter
   *  fingerprint. UI surfaces a small pill + fingerprint digits. */
  readonly e2ee: Accessor<E2eeState>;
  /** Detailed connection metrics for the quality-dot expansion
   *  popover. */
  readonly connectionDetail: Accessor<ConnectionDetail>;
  /** Quality-dot popover open / closed. The dot itself stays
   *  visible; tap toggles the expanded card. */
  readonly qualityPopoverOpen: Accessor<boolean>;
  /** Whether the user is on a touch-mobile form factor with
   *  multiple cameras — drives the "switch camera" button's
   *  visibility. */
  readonly hasMultipleCameras: Accessor<boolean>;
  /** Selected devices. */
  readonly settings: Accessor<CallSettings>;
  readonly notice: Accessor<string | null>;
  /** Mid-call camera-on lifecycle (piece B) — `off` / `starting` /
   *  `live` / `failed`. Driven by the active call session's
   *  `videoPublishState` (the 1×1 `CallSession` or the SFU group
   *  handle); stays `off` on the standalone mock design route. The
   *  camera button reads this to show a spinner + disable while
   *  `starting`, and the local self-tile shows a "Starting camera…"
   *  hint. */
  readonly videoPublishState: Accessor<VideoPublishState>;
}

// ============================================================================
// Internal signals + actions
// ============================================================================

const [phase, setPhase] = createSignal<CallPhase>({ kind: "idle" });
const [connectedAt, setConnectedAt] = createSignal<number | null>(null);
const [pendingMicOn, setPendingMicOn] = createSignal(true);
const [participants, setParticipants] = createSignal<Participant[]>([]);
const [pinnedId, setPinnedId] = createSignal<string | null>(null);
const [layoutChoice, setLayoutChoice] = createSignal<"grid" | "focus" | null>(null);
const NO_SPEECH: CallSpeech = { activeSpeakerId: null, lastSpokeAtMs: {} };
const [speech, setSpeech] = createSignal<CallSpeech>(NO_SPEECH);
const [participantListOpen, setParticipantListOpen] = createSignal(false);
const [e2ee, setE2ee] = createSignal<E2eeState>({
  active: false,
  shortFingerprint: null,
});
const [connectionDetail, setConnectionDetail] = createSignal<ConnectionDetail>({
  rttMs: null,
  downBps: null,
  upBps: null,
  connectionType: "unknown",
});
const [qualityPopoverOpen, setQualityPopoverOpen] = createSignal(false);
const [hasMultipleCameras, setHasMultipleCameras] = createSignal(false);
const [settings, setSettings] = createSignal<CallSettings>(DEFAULT_SETTINGS);
/** One line under the status row, shown for a few seconds: what the call did
 *  on the person's behalf (joined without a camera, section 4.4). `null` when
 *  nothing is up. */
const [notice, setNotice] = createSignal<string | null>(null);
let noticeTimer: ReturnType<typeof setTimeout> | null = null;
/** How long a notice stays on screen. */
export const CALL_NOTICE_MS = 8_000;
const [videoPublishState, setVideoPublishState] =
  createSignal<VideoPublishState>("off");

/** Helpers — replace one participant by id without mutating the
 *  underlying array (Solid's createSignal compares by identity
 *  for arrays; in-place mutation would not trigger
 *  recomputation). */
function patchParticipant(
  id: string,
  patch: Partial<Omit<Participant, "id" | "isLocal">>,
): void {
  setParticipants((list) =>
    list.map((p) => (p.id === id ? { ...p, ...patch } : p)),
  );
}

/** Get the local participant, if there is one. Used by the
 *  controls bar to know what to toggle. */
function localParticipant(): Participant | null {
  return participants().find((p) => p.isLocal) ?? null;
}

// ============================================================================
// Public state surface
// ============================================================================

/** Read-only state accessors. Components destructure these in
 *  their reactive props, not in plain const-binding (Solid's
 *  reactivity tracks the call site of `.()`). */
export const callState: CallStateInternal = {
  phase,
  connectedAt,
  pendingMicOn,
  participants,
  pinnedId,
  layoutChoice,
  speech,
  participantListOpen,
  e2ee,
  connectionDetail,
  qualityPopoverOpen,
  hasMultipleCameras,
  settings,
  notice,
  videoPublishState,
};

/** Derived: the tile the group stage draws large, or `null` for an equal grid.
 *  The rule is `calls/call-screen-model.ts` `featuredParticipant` (§4.4):
 *  pinned, else speaking with video, else first with video, else nobody; the
 *  local tile only when alone. */
export const featuredParticipant = createMemo<Participant | null>(() =>
  pickFeatured(participants(), pinnedId()),
);

/** Derived: the grid — everyone but the featured tile in roster order, or
 *  everyone when there is no featured tile. The local participant always sits
 *  in it unless they are the featured one. */
export const galleryParticipants = createMemo<Participant[]>(() =>
  pickGallery(participants(), featuredParticipant()),
);

/** Mutating actions exposed to UI event handlers. Each is a
 *  free function so a button's `onClick={callActions.toggleMic}`
 *  binds cleanly without extra closures. */
export const callActions = {
  /** Set the initial participant set (e.g. once the call
   *  starts and the in-chat group membership is known). */
  setParticipants(list: Participant[]): void {
    setParticipants(list);
    // A pin names a person; with the person gone it names nobody
    // (`docs/ux/group-call-grid.md` section 6.2).
    const pinned = pinnedId();
    if (pinned !== null && !list.some((p) => p.id === pinned)) setPinnedId(null);
  },
  /** Append a participant (Add MLS commit observed). */
  participantJoined(p: Participant): void {
    setParticipants((list) =>
      list.some((existing) => existing.id === p.id) ? list : [...list, p],
    );
  },
  /** Drop a participant (Remove MLS commit observed). */
  participantLeft(id: string): void {
    setParticipants((list) => list.filter((p) => p.id !== id));
    if (pinnedId() === id) setPinnedId(null);
  },
  /** Update the per-participant connection-quality tier. The
   *  caller (BandwidthController consumer) computes this from
   *  the bandwidth.rs LayerSwitch output mapped through
   *  `qualityFromLevel`. */
  setQuality(id: string, tier: QualityTier): void {
    patchParticipant(id, { quality: tier });
  },
  /** Rename a participant in place. The SFU adapter re-runs its name
   *  resolution when a profile lands mid-call (ADR-0048 v2 carries the
   *  account id the ADR-0021 peer tag is derived from), so a tile that
   *  joined as a pseudonym picks up the person's name. No-op on unknown id
   *  or an unchanged name. */
  setParticipantName(id: string, displayName: string): void {
    setParticipants((list) =>
      list.some((p) => p.id === id && p.displayName !== displayName)
        ? list.map((p) => (p.id === id ? { ...p, displayName } : p))
        : list,
    );
  },
  /** Set a specific participant's camera flag. Used for REMOTE
   *  participants when a video track arrives / ends (the SFU adapter
   *  flips it so the tile switches between the video surface and the
   *  avatar). The LOCAL camera uses `setCamera`. No-op on unknown id. */
  setParticipantCamera(id: string, on: boolean): void {
    patchParticipant(id, { camOn: on });
  },
  /** Set the currently-speaking participant. Pass `null` to
   *  clear (no one speaking). The UI promotes the speaker to
   *  the big tile unless a manual pin is active. */
  setSpeaking(id: string | null): void {
    setParticipants((list) =>
      list.map((p) => ({ ...p, speaking: p.id === id })),
    );
  },
  /**
   * What the sound levels say (`calls/speaking.ts`): whose ring is on, who
   * holds the floor, and when each person last did. Several people can be
   * speaking at once; only one holds the floor.
   *
   * Writes the roster only when a ring actually changed: this runs ten times
   * a second, and a new array on every reading would re-render every tile.
   */
  setSpeech(speakingIds: ReadonlySet<string>, next: CallSpeech): void {
    if (participants().some((p) => p.speaking !== speakingIds.has(p.id))) {
      setParticipants((list) =>
        list.map((p) =>
          p.speaking === speakingIds.has(p.id) ? p : { ...p, speaking: speakingIds.has(p.id) },
        ),
      );
    }
    const now = speech();
    if (
      now.activeSpeakerId !== next.activeSpeakerId ||
      // The floor's own timestamp moves on every reading; nothing reads it
      // while the same person holds the floor, so only a change of holder (or
      // of who has ever spoken) is worth a write.
      Object.keys(now.lastSpokeAtMs).length !== Object.keys(next.lastSpokeAtMs).length
    ) {
      setSpeech(next);
    }
  },
  /** The person's own choice of group layout for this call. */
  setLayoutChoice(choice: "grid" | "focus" | null): void {
    setLayoutChoice(choice);
  },
  /** Toggle the local mic. Idempotent on `state` argument. */
  setMic(on: boolean): void {
    const local = localParticipant();
    if (local !== null) patchParticipant(local.id, { micOn: on });
  },
  /** Toggle the local camera. */
  setCamera(on: boolean): void {
    const local = localParticipant();
    if (local !== null) patchParticipant(local.id, { camOn: on });
  },
  /** Reflect the active call session's mid-call camera-on lifecycle
   *  (piece B). Called by the Chat.tsx effect that mirrors the live
   *  session's `videoPublishState` (the 1×1 `CallSession` or the SFU
   *  group handle). Stays `off` on the standalone mock design route. */
  setVideoPublishState(state: VideoPublishState): void {
    setVideoPublishState(state);
  },
  /** Pin a remote participant to the big tile. Pass `null` to
   *  resume "follow active speaker". */
  pin(id: string | null): void {
    // Only someone else who is in the call can be pinned; anything else is
    // "nobody pinned", so a stale or empty id never sits in the store.
    const person = id === null ? undefined : participants().find((p) => p.id === id);
    setPinnedId(person !== undefined && !person.isLocal ? person.id : null);
  },
  /** Show / hide the participant-list drawer. */
  setParticipantListOpen(open: boolean): void {
    setParticipantListOpen(open);
  },
  /**
   * Transition the call lifecycle phase.
   *
   * Starts the duration clock the first time the call reaches `connected` and
   * never restarts it: a `connected -> reconnecting -> connected` round trip is
   * one call, and a timer that went back to `0:00` would say it was two.
   */
  setPhase(next: CallPhase): void {
    if (next.kind === "connected" && connectedAt() === null) setConnectedAt(Date.now());
    setPhase(next);
  },
  /** Ask for the call's local participant to start muted — the camera check's
   *  microphone toggle, applied before any session reports a state. */
  setPendingMicOn(on: boolean): void {
    setPendingMicOn(on);
  },
  /** Hangup — transitions to `ended`. The route component
   *  watches for this and navigates away after a 3s overlay. */
  endCall(): void {
    setPhase({ kind: "ended", endedAt: Date.now() });
  },
  /** Reset to initial state. Used on route mount + unmount so
   *  re-entering a call screen starts fresh. */
  reset(): void {
    setPhase({ kind: "idle" });
    setConnectedAt(null);
    setPendingMicOn(true);
    setParticipants([]);
    setPinnedId(null);
    setLayoutChoice(null);
    setSpeech(NO_SPEECH);
    setParticipantListOpen(false);
    setE2ee({ active: false, shortFingerprint: null });
    setConnectionDetail({
      rttMs: null,
      downBps: null,
      upBps: null,
      connectionType: "unknown",
    });
    setQualityPopoverOpen(false);
    // Devices list cleared — the selected device ids survive a reset so a
    // headset picked on the last call is still picked on the next one.
    setSettings((s) => ({ ...s, availableDevices: [] }));
    if (noticeTimer !== null) clearTimeout(noticeTimer);
    noticeTimer = null;
    setNotice(null);
    // Mid-call camera-on lifecycle — back to `off` so a fresh call
    // screen starts without a stale spinner.
    setVideoPublishState("off");
  },
  /** Update the E2EE indicator. Called by the WebRTC pod once
   *  the MLS exporter has produced a CallKey and `call_key_
   *  fingerprint` was computed. */
  setE2ee(state: E2eeState): void {
    setE2ee(state);
  },
  /** Update detailed connection metrics — called periodically
   *  by the quality sampler. */
  setConnectionDetail(detail: ConnectionDetail): void {
    setConnectionDetail(detail);
  },
  /** Toggle the quality-dot expansion popover. */
  setQualityPopoverOpen(open: boolean): void {
    setQualityPopoverOpen(open);
  },
  /** Reflect the platform's camera enumeration result — drives
   *  the visibility of the mobile front/back switch button. */
  setHasMultipleCameras(many: boolean): void {
    setHasMultipleCameras(many);
  },
  /** Replace the enumerated-devices snapshot. Called by the call screen after
   *  it runs `navigator.mediaDevices.enumerateDevices()` (labels are populated
   *  once getUserMedia has been granted, i.e. during a live call), and on
   *  `devicechange` when a headset is plugged in mid-call. */
  setAvailableDevices(devices: readonly CallDevice[]): void {
    setSettings((s) => ({ ...s, availableDevices: devices }));
  },
  /** Pick a microphone. `null` = revert to OS default. */
  setSelectedAudioInput(id: string | null): void {
    setSettings((s) => ({ ...s, selectedAudioInput: id }));
  },
  /** Pick a camera. `null` = revert to OS default. */
  setSelectedVideoInput(id: string | null): void {
    setSettings((s) => ({ ...s, selectedVideoInput: id }));
  },
  /** Pick a speaker / headset. `null` = revert to OS default. Applied to every
   *  remote `<audio>` sink through `setSinkId` by the call host. */
  setSelectedAudioOutput(id: string | null): void {
    setSettings((s) => ({ ...s, selectedAudioOutput: id }));
  },
  /** Show one line under the status row for `CALL_NOTICE_MS`, replacing any
   *  line already up. */
  showNotice(text: string): void {
    setNotice(text);
    if (noticeTimer !== null) clearTimeout(noticeTimer);
    noticeTimer = setTimeout(() => {
      noticeTimer = null;
      setNotice(null);
    }, CALL_NOTICE_MS);
  },
};
