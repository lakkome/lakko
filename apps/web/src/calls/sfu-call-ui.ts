// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Chat-UI adapter for {@link SfuCallSession} — the multi-party SFU
 * analog of the 1×1 `syncCallUiFromState` mapping in `routes/Chat.tsx`.
 *
 * Where the 1×1 path bridges a `CallSession`'s `WebCallState` onto the
 * shared `callActions` store, this binds an `SfuCallSession`'s
 * callbacks (state / welcome / peer-joined / peer-left / remote-track)
 * onto the SAME store — so the existing `Call.tsx` in-call UI renders a
 * live SFU roster without per-call-type branching in the view.
 *
 * Two outputs:
 *   * The shared store is driven through `callActions` (phase + the
 *     participant roster + the E2EE indicator).
 *   * A reactive `remoteStreams` signal maps each remote MSID stream id
 *     to its `MediaStream`, so the view can mount one `<video>` /
 *     `<audio>` sink per remote track. (The store's `Participant`
 *     model carries no track handle by design — media lives here,
 *     metadata lives in the store.)
 *
 * The caller owns deriving the inputs (the JWT via `CallJwtClient`, the
 * seed via `Group.exportSecret("lakko/call/v1", callId, 32)`, the leaf
 * via `group.ownLeafIndex`, the SFU base URL from config) and passes
 * them in the {@link SfuCallConfig}; this adapter is pure glue + has no
 * opinion on where they come from.
 *
 * A remote stream id maps directly to a roster participant: the SFU
 * pins the downstream MSID stream id to the source peer id (PeerId
 * Display == wire peer_id == MSID), so `rt.streamId` IS the peer id.
 * `addStream` correlates the stream to its `peer-<id>` roster entry
 * (and flips that participant's camera flag on a video track).
 */

import { createSignal, type Accessor } from "solid-js";
import { captureFallbackNotice, type CaptureFallback } from "./capture-fallback";

import { callActions, callState, type Participant } from "../state/call-state";
import {
  SfuCallSession,
  type SfuCallConfig,
  type SfuCallState,
  type SfuRemoteTrack,
  type VideoPublishState,
} from "./sfu-call-session";
import type { SfuPeerSnapshot } from "./sfu-signaling-client";
import { t } from "../l10n";

/** Options that shape the roster presentation. */
export interface SfuCallUiOptions {
  /** Display name for the local participant tile. Defaults to "You". */
  readonly localDisplayName?: string;
  /** Resolve a peer to a human-readable name (contact-card lookup).
   *  `peerTag` is the ADR-0021 per-call tag (when the SFU emits one);
   *  the resolver matches it against the caller's contacts. Falls back
   *  to a short slice of the account pseudonym. */
  readonly resolveDisplayName?: (
    peerId: string,
    accountPseudonym: string,
    peerTag: string | undefined,
  ) => string;
  /** Canonical call safety number (`callSafetyNumber(seed)` from the
   *  wasm core) for the in-call E2EE pill — the same string every
   *  participant derives from the shared seed, read aloud out-of-band to
   *  detect a man-in-the-middle. Omitted → the pill shows no fingerprint. */
  readonly safetyNumber?: string | undefined;
  /** Every session lifecycle transition, after the store has been updated.
   *  The chat uses it to announce the call on the group journal: a
   *  `GroupCallJoin` once `connected`, a `GroupCallLeave` on `failed`. */
  readonly onSessionStateChange?: (state: SfuCallState) => void;
}

/** Handle returned by {@link createSfuCallUi}. */
export interface SfuCallUiHandle {
  /** The underlying session (for direct control if needed). */
  readonly session: SfuCallSession;
  /** Reactive map: remote MSID stream id → its `MediaStream`. The view
   *  mounts a sink per entry. */
  readonly remoteStreams: Accessor<ReadonlyMap<string, MediaStream>>;
  /** Reactive local capture stream (null until publish). The view binds it
   *  to the local self-preview tile (mirrored), the group analog of the
   *  1×1 `localStream`. */
  readonly localStream: Accessor<MediaStream | null>;
  /** Reactive mid-call camera-on lifecycle (piece B) — `off` / `starting` /
   *  `live` / `failed`. Drives the camera button spinner + the fail-safe
   *  revert in the view. */
  readonly videoPublishState: Accessor<VideoPublishState>;
  /** Open the SFU connection + publish (delegates to the session). */
  connect(): Promise<void>;
  /** Tear down + reset the call-UI store. Idempotent. */
  hangUp(): void;
  /** Mute / unmute the local mic (drives the real track + the store). */
  setMicrophoneEnabled(on: boolean): void;
  /** Enable / disable the local camera (drives the real track + the
   *  store). */
  setCameraEnabled(on: boolean): void;
  /** Turn the camera ON mid-call (piece B): acquire + add a video track +
   *  renegotiate when the call started audio-only; re-enable an existing
   *  sender otherwise. Throws on capture failure so the caller reverts the
   *  toggle + surfaces a message. */
  publishVideo(): Promise<void>;
  /** Switch the published mic to a specific `deviceId` mid-call (in-call
   *  device picker). `replaceTrack` on the live sender — SFrame stays armed. */
  switchAudioInput(deviceId: string): Promise<void>;
  /** Switch the published camera to a specific `deviceId` mid-call (a no-op
   *  while the group call is audio-only). */
  switchVideoInput(deviceId: string): Promise<void>;
  /** Rotate the call key on a mid-call MLS epoch change (G-18, ADR-0029).
   *  The caller re-derives the per-call seed at the new epoch and passes it;
   *  every attached SFrame transform retains its previous key for the overlap
   *  window so in-flight frames still open, then drops it. No-op if unchanged. */
  rekey(newSeed: Uint8Array): void;
  /** Re-run `resolveDisplayName` for every remote peer in the roster and
   *  patch the tiles whose name changed. The caller runs it when what the
   *  resolver reads has changed — a profile carrying an account id landing
   *  after the call started (ADR-0048 v2), a rename — so a tile that joined
   *  as a pseudonym is not stuck with it for the call's lifetime. */
  refreshNames(): void;
}

const PEER_PREFIX = "peer-";

/** Stable roster id for a remote peer. */
function peerParticipantId(peerId: string): string {
  return `${PEER_PREFIX}${peerId}`;
}

/** Build a store `Participant` for a remote peer. Camera starts off
 *  (no video track yet); `addStream` flips it on when a video track
 *  arrives, keyed by the SFU's MSID-stream-id-to-peer mapping. */
function remoteParticipant(displayName: string, peerId: string): Participant {
  return {
    id: peerParticipantId(peerId),
    displayName,
    isLocal: false,
    micOn: true,
    camOn: false,
    quality: "good",
    speaking: false,
  };
}

/**
 * Construct an {@link SfuCallSession} wired to the shared call-UI
 * store. Returns the {@link SfuCallUiHandle}; call `connect()` to join.
 */
export function createSfuCallUi(
  config: SfuCallConfig,
  options: SfuCallUiOptions = {},
): SfuCallUiHandle {
  const [remoteStreams, setRemoteStreams] = createSignal<
    ReadonlyMap<string, MediaStream>
  >(new Map());
  const [localStream, setLocalStream] = createSignal<MediaStream | null>(null);
  const [videoPublishState, setVideoPublishState] =
    createSignal<VideoPublishState>("off");

  // Sticky last-known remote camera state, keyed by the roster tile id
  // (`peer-<peerId>`). Closes the join-time reorder race: a newcomer
  // subscribing to an already camera-muted peer still receives that peer's
  // present-but-disabled video track, so `addStream` fires and would flip
  // `camOn` back on. `onRemoteMediaState` is authoritative and records the
  // true state here; the video-track-arrival flip DEFERS to it — a track
  // arrival for a peer we already know is muted keeps it off, an unknown peer
  // defaults on (they published video). Adapter-local (one adapter per call)
  // and confined to the same reactive scope the existing
  // `callActions.setParticipantCamera` calls already run on — no new
  // concurrency; the whole handle (and this map) is discarded on `hangUp`.
  const lastKnownCameraOn = new Map<string, boolean>();

  // The naming inputs of every remote peer in the roster, keyed by the
  // roster tile id. `refreshNames` replays them through the resolver; the
  // store's `Participant` deliberately carries neither.
  const namingInputs = new Map<string, { pseudonym: string; peerTag: string | undefined }>();

  const localName = options.localDisplayName ?? t("call.participant.you");
  /** What this device joined without (section 4.4), once capture ran. */
  let captureFallback: CaptureFallback | null = null;
  const nameOf = (
    peerId: string,
    pseudonym: string,
    peerTag: string | undefined,
  ): string =>
    options.resolveDisplayName?.(peerId, pseudonym, peerTag) ??
    (pseudonym.length > 0 ? pseudonym.slice(0, 8) : t("chat.peer.fallback_name"));

  const localParticipant = (): Participant => ({
    id: "local",
    displayName: localName,
    isLocal: true,
    // The camera check can be left muted (docs/ux/calls.md section 3.2), and
    // `media.audio` is always true on web (a call with no audio transceiver
    // could never be unmuted). So the mute state comes from the store, not
    // from the capture flags.
    micOn:
      captureFallback?.missing === "camera-and-microphone" ? false : callState.pendingMicOn(),
    camOn: config.media.video && captureFallback === null,
    quality: "good",
    speaking: false,
  });

  // The roster participant id for a peer is `peer-<peerId>`, and the SFU
  // sets the downstream MSID stream id to the source peer id (verified:
  // PeerId Display == the wire peer_id == the MSID), so `rt.streamId` IS
  // that peer id. This lets the tile correlate a stream to its roster
  // entry, and lets us flip the participant's camera flag when a video
  // track arrives.
  const addStream = (rt: SfuRemoteTrack): void => {
    setRemoteStreams((prev) => {
      const next = new Map(prev);
      next.set(rt.streamId, rt.stream);
      return next;
    });
    const participantId = peerParticipantId(rt.streamId);
    if (rt.track.kind === "video") {
      // Join-race guard: a track ARRIVAL for a peer we already know is
      // camera-muted (via a prior `onRemoteMediaState`) must NOT flip it back
      // on — the disabled track is still present on the SFU and its add can
      // land after the muted re-announce. Defer to the sticky last-known state;
      // an unknown peer defaults on (they published video). Native pure
      // decision `trackPresent ? (lastKnown ?? true) : false` — the track is
      // present here, and the `false`/removal case is the `ended` handler below.
      callActions.setParticipantCamera(
        participantId,
        lastKnownCameraOn.get(participantId) ?? true,
      );
    }
    // Prune when the SFU stops forwarding this track (peer left /
    // unpublished). The `ended` event is the reliable signal.
    rt.track.addEventListener("ended", () => {
      if (rt.track.kind === "video") {
        callActions.setParticipantCamera(participantId, false);
      }
      setRemoteStreams((prev) => {
        // Only drop the stream entry when no live tracks remain on it
        // (a peer that toggles camera off keeps its audio track + entry).
        if (rt.stream.getTracks().some((t) => t.readyState === "live")) {
          return prev;
        }
        if (!prev.has(rt.streamId)) return prev;
        const next = new Map(prev);
        next.delete(rt.streamId);
        return next;
      });
    });
  };

  const session = new SfuCallSession(config, {
    onStateChange: (state): void => {
      switch (state) {
        case "connecting":
          callActions.setPhase({ kind: "connecting" });
          break;
        case "connected":
          callActions.setPhase({ kind: "connected" });
          // SFrame keying is armed at connect (the seed is present), so
          // media is end-to-end encrypted the moment we are connected.
          // The safety number (derived from the same shared seed) lets
          // participants verify the E2EE root key out-of-band.
          callActions.setE2ee({
            active: true,
            shortFingerprint: options.safetyNumber ?? null,
          });
          break;
        case "failed":
        case "closed":
          callActions.setPhase({ kind: "ended", endedAt: Date.now() });
          break;
        case "idle":
          break;
      }
      options.onSessionStateChange?.(state);
    },
    onWelcome: (selfPeerId, roster): void => {
      namingInputs.clear();
      const remotes = roster
        .filter((p: SfuPeerSnapshot) => p.peer_id !== selfPeerId)
        .map((p: SfuPeerSnapshot) => {
          namingInputs.set(peerParticipantId(p.peer_id), {
            pseudonym: p.account_pseudonym,
            peerTag: p.peer_tag,
          });
          return remoteParticipant(
            nameOf(p.peer_id, p.account_pseudonym, p.peer_tag),
            p.peer_id,
          );
        });
      callActions.setParticipants([localParticipant(), ...remotes]);
    },
    onPeerJoined: (peerId, accountPseudonym, peerTag): void => {
      namingInputs.set(peerParticipantId(peerId), { pseudonym: accountPseudonym, peerTag });
      callActions.participantJoined(
        remoteParticipant(nameOf(peerId, accountPseudonym, peerTag), peerId),
      );
    },
    onPeerLeft: (peerId): void => {
      const id = peerParticipantId(peerId);
      // Forget the departed peer's sticky camera state so a later rejoin under
      // the same id starts from a clean slate (join-race guard).
      lastKnownCameraOn.delete(id);
      namingInputs.delete(id);
      callActions.participantLeft(id);
    },
    onRemoteTrack: (rt): void => {
      addStream(rt);
    },
    onRemoteMediaState: (peerId, cameraOn): void => {
      // camera-off-cleanup: a peer broadcast its camera on/off state. Flip the
      // matching roster tile's `camOn` deterministically — a mute fires no
      // track `ended` event, so without this the tile would keep painting a
      // frozen frame. Complementary to the `ended` pruning in `addStream`
      // (which handles the genuine track-gone / unpublish / leave case).
      const id = peerParticipantId(peerId);
      // Media-state is authoritative — record it so a later (racy) track
      // arrival for a muted peer defers to it (the join-race guard in
      // `addStream`).
      lastKnownCameraOn.set(id, cameraOn);
      callActions.setParticipantCamera(id, cameraOn);
    },
    onLocalStream: (stream): void => {
      setLocalStream(stream);
    },
    onVideoPublishState: (state): void => {
      setVideoPublishState(state);
    },
    onCaptureFallback: (fallback): void => {
      // Section 4.4: the device lacked what the call asked for. Turn the
      // missing controls off on the own tile and say so once.
      captureFallback = fallback;
      callActions.setCamera(false);
      if (fallback.missing === "camera-and-microphone") callActions.setMic(false);
      callActions.showNotice(captureFallbackNotice(fallback));
    },
  });

  return {
    session,
    remoteStreams,
    localStream,
    videoPublishState,
    connect: (): Promise<void> => session.connect(),
    hangUp: (): void => {
      session.hangUp();
      namingInputs.clear();
      setRemoteStreams(new Map());
      setLocalStream(null);
      setVideoPublishState("off");
      callActions.reset();
    },
    // Track-only gates — the store's local-participant micOn / camOn is
    // the source of truth (driven by the Call.tsx buttons), so these do
    // NOT write back to the store (that would loop the Chat.tsx effect
    // that reads the store and calls these). Mirrors the 1×1 path.
    setMicrophoneEnabled: (on): void => session.setMicrophoneEnabled(on),
    setCameraEnabled: (on): void => session.setCameraEnabled(on),
    publishVideo: (): Promise<void> => session.publishVideo(),
    switchAudioInput: (deviceId): Promise<void> =>
      session.switchAudioInput(deviceId),
    switchVideoInput: (deviceId): Promise<void> =>
      session.switchVideoInput(deviceId),
    rekey: (newSeed): void => session.rekey(newSeed),
    refreshNames: (): void => {
      for (const [id, inputs] of namingInputs) {
        callActions.setParticipantName(
          id,
          nameOf(id.slice(PEER_PREFIX.length), inputs.pseudonym, inputs.peerTag),
        );
      }
    },
  };
}
