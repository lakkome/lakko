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
import type { VideoQuality } from "./video-wants";
import { splitStreamId, withLightStream, withoutLightStream } from "./light-stream";
import { t } from "../l10n";
import { GroupScreenShare } from "./group-screen-share";
import { screenShareActions } from "./screen-share-store";
import { SCREEN_SHARE_NOTICE_MS } from "./screen-share-capability";

/** Options that shape the roster presentation. */
export interface SfuCallUiOptions {
  /** Display name for the local participant tile. Defaults to "You". */
  readonly localDisplayName?: string;
  /** The name of the person behind a peer (base64 account key, from the
   *  peer's verified announcement, ADR-0078 section 2), or `null` while it
   *  cannot be named (`calls/tile-name.ts`). */
  readonly resolveDisplayName?: (personKeyB64: string) => string | null;
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
  /** Reactive map: remote peer id -> that peer's light companion stream
   *  (`light-stream.ts`). A second picture of a peer already in
   *  `remoteStreams`: the peer's tile shows it while the main one stands
   *  still. Never a participant, a tile or an audio sink of its own. */
  readonly remoteLightStreams: Accessor<ReadonlyMap<string, MediaStream>>;
  /** Reactive map: remote peer id -> that peer's middle stream
   *  (`light-stream.ts`, ADR-0070 step 5): the picture a grid tile watches in
   *  place of the main one. Kept by the rules of the light one: never a
   *  participant, a tile or an audio sink of its own. */
  readonly remoteMiddleStreams: Accessor<ReadonlyMap<string, MediaStream>>;
  /** Reactive map: remote peer id -> the picture the SFU says it gives this
   *  device of them (`video-given.ts`, ADR-0070 step 6). A peer absent from
   *  it is given what is asked for; empty with downlink control off. */
  readonly remoteVideoGiven: Accessor<ReadonlyMap<string, VideoQuality>>;
  /** A tile says which picture of a peer it shows, or `null` when it stops
   *  (`video-wants.ts`); the session asks the SFU accordingly. */
  reportTileVideoQuality(tile: object, peerId: string, quality: VideoQuality | null): void;
  /** The receiver that decodes a peer's audio, for the sound level the call
   *  screen reads on this device (`audio-level.ts`); `undefined` before
   *  their audio arrived. */
  remoteAudioReceiverFor(peerId: string): RTCRtpReceiver | undefined;
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
   *  resolver reads has changed — a profile landing after the call started,
   *  a rename — so a tile that joined unnamed is not stuck that way. */
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
  const [remoteLightStreams, setRemoteLightStreams] = createSignal<
    ReadonlyMap<string, MediaStream>
  >(new Map());
  const [remoteMiddleStreams, setRemoteMiddleStreams] = createSignal<
    ReadonlyMap<string, MediaStream>
  >(new Map());
  const [remoteVideoGiven, setRemoteVideoGiven] = createSignal<
    ReadonlyMap<string, VideoQuality>
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

  // The person (base64 account key) behind every remote peer whose sealed
  // announcement verified (ADR-0078 section 2), keyed by the roster tile id.
  // `refreshNames` replays them through the resolver; the store's
  // `Participant` deliberately carries neither.
  const peerPersons = new Map<string, string | null>();

  const localName = options.localDisplayName ?? t("call.participant.you");
  /** What this device joined without (section 4.4), once capture ran. */
  let captureFallback: CaptureFallback | null = null;
  const nameOf = (personKeyB64: string | null | undefined): string =>
    // An empty name is the call screen's unnamed state, for the moment
    // before the peer's announcement reaches this device
    // (`calls/tile-name.ts`).
    (personKeyB64 === null || personKeyB64 === undefined
      ? null
      : options.resolveDisplayName?.(personKeyB64)) ?? "";

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
  //
  // A stream id ending in `LIGHT_STREAM_SUFFIX` is the light companion of the
  // peer named before it, one ending in `MIDDLE_STREAM_SUFFIX` that peer's
  // middle track. Each goes into its own map and touches nothing else: no
  // roster entry, no camera flag, no entry in `remoteStreams` (so no audio
  // sink and no tile). It is pruned on its track's `ended` and with its peer.
  const addCopyStream = (role: "light" | "middle", peerId: string, rt: SfuRemoteTrack): void => {
    if (rt.track.kind !== "video") return;
    const set = role === "light" ? setRemoteLightStreams : setRemoteMiddleStreams;
    set((prev) => withLightStream(prev, peerId, rt.stream));
    rt.track.addEventListener("ended", () => {
      set((prev) => withoutLightStream(prev, peerId, rt.stream));
    });
  };

  const addStream = (rt: SfuRemoteTrack): void => {
    const id = splitStreamId(rt.streamId);
    if (id.role !== "main") {
      addCopyStream(id.role, id.peerId, rt);
      return;
    }
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

  // The screen share (ADR-0083): its fold, the button, the received share's
  // stream. Built before the session so every callback below can reach it;
  // the session is read only when the share calls it.
  const share = new GroupScreenShare({
    announcer: config.peerAnnouncer,
    session: {
      sendSealedRelay: (plaintext) => session.sendSealedRelay(plaintext),
      attachScreenShare: (capture) => session.attachScreenShare(capture),
      detachScreenShare: () => session.detachScreenShare(),
      getSelfPeerId: () => session.getSelfPeerId(),
    },
    nameOf: (personKeyB64) => nameOf(personKeyB64),
    notice: (text) => callActions.showNotice(text, SCREEN_SHARE_NOTICE_MS),
    store: screenShareActions,
  });
  // The fold is built from the core the session loaded at connect: by the
  // Welcome it is there. Without it nothing is shared or folded.
  const initShare = (): void => {
    const wasm = session.loadedWasm?.();
    if (wasm !== null && wasm !== undefined) share.init(wasm);
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
          initShare();
          share.onConnected();
          break;
        case "failed":
        case "closed":
          share.dispose();
          screenShareActions.reset();
          callActions.setPhase({ kind: "ended", endedAt: Date.now() });
          break;
        case "idle":
          break;
      }
      options.onSessionStateChange?.(state);
    },
    onWelcome: (selfPeerId, roster): void => {
      initShare();
      peerPersons.clear();
      const remotes = roster
        .filter((p: SfuPeerSnapshot) => p.peer_id !== selfPeerId)
        .map((p: SfuPeerSnapshot) => {
          peerPersons.set(peerParticipantId(p.peer_id), null);
          return remoteParticipant(nameOf(null), p.peer_id);
        });
      callActions.setParticipants([localParticipant(), ...remotes]);
    },
    onPeerJoined: (peerId): void => {
      share.onPeerJoined();
      peerPersons.set(peerParticipantId(peerId), null);
      callActions.participantJoined(remoteParticipant(nameOf(null), peerId));
    },
    onPeerIdentified: (peerId, identity): void => {
      // ADR-0078 section 2: the peer's sealed announcement verified against
      // a leaf of the call's group, so the tile names that person.
      share.onPeerIdentified(peerId, identity.memberKeyHex);
      const id = peerParticipantId(peerId);
      if (!peerPersons.has(id)) return;
      if (peerPersons.get(id) === identity.personKeyB64) return;
      peerPersons.set(id, identity.personKeyB64);
      callActions.setParticipantName(id, nameOf(identity.personKeyB64));
    },
    onPeerLeft: (peerId): void => {
      const id = peerParticipantId(peerId);
      // Forget the departed peer's sticky camera state so a later rejoin under
      // the same id starts from a clean slate (join-race guard).
      lastKnownCameraOn.delete(id);
      peerPersons.delete(id);
      share.onPeerLeft(peerId);
      // The companion and the middle track go with their peer.
      setRemoteLightStreams((prev) => withoutLightStream(prev, peerId));
      setRemoteMiddleStreams((prev) => withoutLightStream(prev, peerId));
      callActions.participantLeft(id);
    },
    onRemoteTrack: (rt): void => {
      // A shared screen is not a participant (ADR-0083).
      if (share.onRemoteTrack(rt.streamId, rt.track, rt.stream)) return;
      addStream(rt);
    },
    onRemoteScreenShareState: (peerId, state): void => {
      share.onState(peerId, state);
    },
    onVideoGiven: (given): void => {
      setRemoteVideoGiven(given);
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
    remoteLightStreams,
    remoteMiddleStreams,
    remoteVideoGiven,
    reportTileVideoQuality: (tile, peerId, quality): void =>
      session.reportTileVideoQuality(tile, peerId, quality),
    remoteAudioReceiverFor: (peerId): RTCRtpReceiver | undefined =>
      session.audioReceiverFor(peerId),
    localStream,
    videoPublishState,
    connect: (): Promise<void> => session.connect(),
    hangUp: (): void => {
      share.dispose();
      screenShareActions.reset();
      session.hangUp();
      peerPersons.clear();
      setRemoteStreams(new Map());
      setRemoteLightStreams(new Map());
      setRemoteMiddleStreams(new Map());
      setRemoteVideoGiven(new Map());
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
      for (const [id, person] of peerPersons) {
        callActions.setParticipantName(id, nameOf(person));
      }
      share.refreshNames();
    },
  };
}
