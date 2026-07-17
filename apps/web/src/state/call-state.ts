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

/** A single participant in the call. Local + remote share the
 *  same shape; the local one has `isLocal: true` and is filtered
 *  out of certain views (e.g. you don't appear in the gallery
 *  next to your own controls). */
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
  /** Raised-hand state. UI shows a glowing hand icon on the
   *  tile + a sort hint in the participant list. */
  readonly handRaised: boolean;
  /** Connection quality as last reported by the bandwidth
   *  controller. */
  readonly quality: QualityTier;
  /** `true` when this participant is the currently-loudest
   *  speaker (LiveKit metadata). The UI promotes them to the
   *  large active-speaker tile. */
  readonly speaking: boolean;
  /** Whether this participant is currently sharing their
   *  screen. Renders the screen-share preview in the place of
   *  their camera tile. */
  readonly screenSharing: boolean;
}

/** Call lifecycle phase.
 *
 *  `prejoin` is the new pre-call screen — user previews their own
 *  camera + microphone, sets a display name, then taps Join to
 *  enter the call. Mirror of the `preJoinScreen` flow in
 *  `lakko_me/public/web/room-template.ts`. */
export type CallPhase =
  | { readonly kind: "prejoin" }
  | { readonly kind: "connecting" }
  | { readonly kind: "connected" }
  | { readonly kind: "reconnecting"; readonly reason: string }
  | { readonly kind: "ended"; readonly endedAt: number };

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
 *  In Phase 3 the toggles affect state only — the real WebRTC
 *  pod (`S-3-A-iOS-pod` / `S-3-A-Android-pod`) wires them into
 *  the live PeerConnection. Storing them here keeps the UX
 *  parity work decoupled from the pod work.
 *
 *  `selected*` fields are `null` until the user picks a device
 *  explicitly; the OS-default device is used in the meantime. */
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
  /** Noise-suppression filter — `getUserMedia({ audio: {
   *  noiseSuppression: true } })` on web; equivalent
   *  AVAudioSession / AudioManager mode on the natives. */
  readonly noiseSuppression: boolean;
  /** Acoustic echo cancellation. */
  readonly echoCancellation: boolean;
  /** Automatic gain control. */
  readonly autoGain: boolean;
  /** Mirror the local preview tile horizontally — natural for
   *  front-facing cameras, distracting for back cameras. */
  readonly mirrorLocal: boolean;
  /** Low-data mode — clamps simulcast to the bottom layer
   *  (≈ 360p / 150 kbps) regardless of measured bandwidth.
   *  Useful on metered mobile connections. */
  readonly lowDataMode: boolean;
}

/** A single chat message inside the call. Two kinds:
 *
 *  - `"text"` — a user-authored message (the canonical case).
 *    Carries an MLS application-message round-trip ID
 *    `id` and the sender's participant id + display name.
 *  - `"system"` — auto-generated event ("Alice joined",
 *    "Bob raised hand", "Screen share started"). Rendered as
 *    a grey, centred line. `senderId` is `null` for system
 *    events.
 *
 *  Persistence: text messages are MLS application messages
 *  in the call's existing MLS group, so they survive the
 *  call ending (the group itself persists). System events
 *  are NOT persisted — they are derived from observed state
 *  transitions and would be regenerated on every replay. */
export interface ChatMessage {
  readonly id: string;
  readonly kind: "text" | "system";
  /** Author participant id, or `null` for system events. */
  readonly senderId: string | null;
  /** Display name resolved at send time (so a later rename
   *  does not retroactively rewrite the chat). */
  readonly senderName: string;
  readonly text: string;
  /** Unix milliseconds. */
  readonly ts: number;
}

/** Default `CallSettings` — every toggle on, no explicit
 *  device picks. */
const DEFAULT_SETTINGS: CallSettings = {
  selectedAudioInput: null,
  selectedVideoInput: null,
  selectedAudioOutput: null,
  availableDevices: [],
  noiseSuppression: true,
  echoCancellation: true,
  autoGain: true,
  mirrorLocal: true,
  lowDataMode: false,
};

/** Top-level UI state. Internal — components read via the
 *  accessors below. */
interface CallStateInternal {
  readonly phase: Accessor<CallPhase>;
  readonly participants: Accessor<Participant[]>;
  /** Id of the pinned participant. `null` = follow active
   *  speaker. */
  readonly pinnedId: Accessor<string | null>;
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
  /** Settings modal open / closed. */
  readonly settingsOpen: Accessor<boolean>;
  /** In-call audio / video settings + selected devices. */
  readonly settings: Accessor<CallSettings>;
  /** Chat panel open / closed. */
  readonly chatOpen: Accessor<boolean>;
  /** Chat-message log — oldest first. */
  readonly chatMessages: Accessor<readonly ChatMessage[]>;
  /** Live in-progress text-box content (held in state so it
   *  survives panel close / reopen during a call). */
  readonly composeText: Accessor<string>;
  /** Number of unread text messages since the user last
   *  opened the chat panel. System events do not count
   *  towards unread — they are background context, not a
   *  reason to draw the user's attention. */
  readonly chatUnreadCount: Accessor<number>;
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

const [phase, setPhase] = createSignal<CallPhase>({ kind: "prejoin" });
const [participants, setParticipants] = createSignal<Participant[]>([]);
const [pinnedId, setPinnedId] = createSignal<string | null>(null);
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
const [settingsOpen, setSettingsOpen] = createSignal(false);
const [settings, setSettings] = createSignal<CallSettings>(DEFAULT_SETTINGS);
const [chatOpen, setChatOpen] = createSignal(false);
const [chatMessages, setChatMessages] = createSignal<readonly ChatMessage[]>([]);
const [composeText, setComposeText] = createSignal("");
const [chatUnreadCount, setChatUnreadCount] = createSignal(0);
const [videoPublishState, setVideoPublishState] =
  createSignal<VideoPublishState>("off");

/** Monotone counter for client-side chat-message ids when
 *  the message has not yet been ack'd by the MLS application-
 *  message round-trip. Each id is namespaced with the call
 *  start timestamp so a fresh call cannot collide with the
 *  previous one's mock ids. */
let chatIdCounter = 0;
function nextChatId(): string {
  chatIdCounter += 1;
  return `local-${Date.now()}-${chatIdCounter}`;
}

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
  participants,
  pinnedId,
  participantListOpen,
  e2ee,
  connectionDetail,
  qualityPopoverOpen,
  hasMultipleCameras,
  settingsOpen,
  settings,
  chatOpen,
  chatMessages,
  composeText,
  chatUnreadCount,
  videoPublishState,
};

/** Derived: the participant whose tile should be in the big
 *  "active speaker" slot. Picks (in order): the pinned one,
 *  the currently-speaking remote, otherwise the first remote,
 *  otherwise local. */
export const activeSpeaker = createMemo<Participant | null>(() => {
  const pin = pinnedId();
  const all = participants();
  if (pin !== null) {
    const found = all.find((p) => p.id === pin);
    if (found !== undefined) return found;
  }
  const speakingRemote = all.find((p) => p.speaking && !p.isLocal);
  if (speakingRemote !== undefined) return speakingRemote;
  const firstRemote = all.find((p) => !p.isLocal);
  if (firstRemote !== undefined) return firstRemote;
  return all[0] ?? null;
});

/** Derived: gallery slice — everyone except the active
 *  speaker. The local participant always appears in the
 *  gallery (in a small pip-style tile), unless they ARE the
 *  active speaker. */
export const galleryParticipants = createMemo<Participant[]>(() => {
  const active = activeSpeaker();
  if (active === null) return [];
  return participants().filter((p) => p.id !== active.id);
});

/** Mutating actions exposed to UI event handlers. Each is a
 *  free function so a button's `onClick={callActions.toggleMic}`
 *  binds cleanly without extra closures. */
export const callActions = {
  /** Set the initial participant set (e.g. once the call
   *  starts and the in-chat group membership is known). */
  setParticipants(list: Participant[]): void {
    setParticipants(list);
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
  /** Toggle local raise-hand. */
  setHandRaised(raised: boolean): void {
    const local = localParticipant();
    if (local !== null) patchParticipant(local.id, { handRaised: raised });
  },
  /** Toggle the local screen-share. */
  setScreenSharing(on: boolean): void {
    const local = localParticipant();
    if (local !== null) patchParticipant(local.id, { screenSharing: on });
  },
  /** Pin a remote participant to the big tile. Pass `null` to
   *  resume "follow active speaker". */
  pin(id: string | null): void {
    setPinnedId(id);
  },
  /** Show / hide the participant-list drawer. */
  setParticipantListOpen(open: boolean): void {
    setParticipantListOpen(open);
  },
  /** Transition the call lifecycle phase. */
  setPhase(next: CallPhase): void {
    setPhase(next);
  },
  /** Hangup — transitions to `ended`. The route component
   *  watches for this and navigates away after a 3s overlay. */
  endCall(): void {
    setPhase({ kind: "ended", endedAt: Date.now() });
  },
  /** Reset to initial state. Used on route mount + unmount so
   *  re-entering a call screen starts fresh. */
  reset(): void {
    setPhase({ kind: "prejoin" });
    setParticipants([]);
    setPinnedId(null);
    setParticipantListOpen(false);
    setE2ee({ active: false, shortFingerprint: null });
    setConnectionDetail({
      rttMs: null,
      downBps: null,
      upBps: null,
      connectionType: "unknown",
    });
    setQualityPopoverOpen(false);
    setSettingsOpen(false);
    // Devices list cleared — preferences (toggles + selected
    // device ids) survive a reset so the user does not have to
    // re-enable NS/AEC every time they enter a call screen.
    setSettings((s) => ({ ...s, availableDevices: [] }));
    // Chat is a per-call surface — wipe on reset. The
    // production MLS-backed transport persists text messages
    // in the group history independently; what we clear here
    // is the in-memory view + the unread counter.
    setChatOpen(false);
    setChatMessages([]);
    setComposeText("");
    setChatUnreadCount(0);
    // Mid-call camera-on lifecycle — back to `off` so a fresh call
    // screen starts without a stale spinner.
    setVideoPublishState("off");
  },
  /** Transition from prejoin → connecting. Called when the user
   *  clicks Join on the prejoin screen. The WebRTC pod takes
   *  over from here, drives the SDP exchange, and flips the
   *  phase to `connected` when ICE pairs. */
  join(): void {
    setPhase({ kind: "connecting" });
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
  /** Open / close the in-call settings modal. */
  setSettingsOpen(open: boolean): void {
    setSettingsOpen(open);
  },
  /** Replace the enumerated-devices snapshot. Called by the
   *  call settings panel after it runs
   *  `navigator.mediaDevices.enumerateDevices()` (labels are
   *  populated once getUserMedia has been granted, i.e. during
   *  a live call). */
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
  /** Pick a speaker / headset. `null` = revert to OS default. */
  setSelectedAudioOutput(id: string | null): void {
    setSettings((s) => ({ ...s, selectedAudioOutput: id }));
  },
  /** Toggle the noise-suppression filter. */
  setNoiseSuppression(on: boolean): void {
    setSettings((s) => ({ ...s, noiseSuppression: on }));
  },
  /** Toggle acoustic echo cancellation. */
  setEchoCancellation(on: boolean): void {
    setSettings((s) => ({ ...s, echoCancellation: on }));
  },
  /** Toggle automatic gain control. */
  setAutoGain(on: boolean): void {
    setSettings((s) => ({ ...s, autoGain: on }));
  },
  /** Toggle local-preview horizontal mirroring. */
  setMirrorLocal(on: boolean): void {
    setSettings((s) => ({ ...s, mirrorLocal: on }));
  },
  /** Toggle low-data mode (clamps simulcast to bottom layer). */
  setLowDataMode(on: boolean): void {
    setSettings((s) => ({ ...s, lowDataMode: on }));
  },
  /** Open / close the chat panel. Opening clears the unread
   *  badge — the user has seen the new messages. */
  setChatOpen(open: boolean): void {
    setChatOpen(open);
    if (open) setChatUnreadCount(0);
  },
  /** Update the in-progress text-box content. Held in state
   *  so the panel can close + reopen without losing what the
   *  user typed. */
  setComposeText(text: string): void {
    setComposeText(text);
  },
  /** Send a chat message from the local participant. Returns
   *  the constructed `ChatMessage` so the WebRTC pod can hand
   *  it to MLS application-message encryption.
   *
   *  Local-echo: the message appears in `chatMessages`
   *  immediately. The MLS round-trip is fire-and-forget — if
   *  the network drops, the WebRTC pod is responsible for
   *  re-queueing. UI does not currently surface delivery
   *  state per message (`ChatMessage` has room to add a
   *  `status` field if that becomes important). */
  sendChatMessage(text: string): ChatMessage | null {
    const trimmed = text.trim();
    if (trimmed.length === 0) return null;
    const local = localParticipant();
    const senderId = local?.id ?? "local";
    const senderName = local?.displayName ?? "You";
    const msg: ChatMessage = {
      id: nextChatId(),
      kind: "text",
      senderId,
      senderName,
      text: trimmed,
      ts: Date.now(),
    };
    setChatMessages((prev) => [...prev, msg]);
    setComposeText("");
    return msg;
  },
  /** Receive a chat message from a remote participant
   *  (decrypted MLS application message). If the chat panel
   *  is closed, the unread badge increments. */
  receiveChatMessage(msg: ChatMessage): void {
    setChatMessages((prev) => [...prev, msg]);
    if (!chatOpen() && msg.kind === "text") {
      setChatUnreadCount((n) => n + 1);
    }
  },
  /** Append a system event (peer joined, hand raised, screen
   *  share started). Does NOT bump the unread counter — these
   *  are background context. */
  addSystemEvent(text: string): void {
    const msg: ChatMessage = {
      id: nextChatId(),
      kind: "system",
      senderId: null,
      senderName: "",
      text,
      ts: Date.now(),
    };
    setChatMessages((prev) => [...prev, msg]);
  },
  /** Manually clear the unread badge. The chat-open action
   *  already does this; this is the explicit knob for cases
   *  like "mark all read without opening". */
  clearChatUnread(): void {
    setChatUnreadCount(0);
  },
};
