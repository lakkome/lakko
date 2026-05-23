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
 * No WebRTC integration here — the live media path lands in
 * the platform's WebRTC pod (3a-pod-followup). Until then,
 * UI consumes mock data + the BandwidthController (#39) for
 * the connection-quality glyph.
 */

import { createSignal, createMemo, type Accessor } from "solid-js";

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
};
