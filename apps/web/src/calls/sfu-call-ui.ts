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

import { callActions, type Participant } from "../state/call-state";
import {
  SfuCallSession,
  type SfuCallConfig,
  type SfuRemoteTrack,
} from "./sfu-call-session";
import type { SfuPeerSnapshot } from "./sfu-signaling-client";

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
  /** Open the SFU connection + publish (delegates to the session). */
  connect(): Promise<void>;
  /** Tear down + reset the call-UI store. Idempotent. */
  hangUp(): void;
  /** Mute / unmute the local mic (drives the real track + the store). */
  setMicrophoneEnabled(on: boolean): void;
  /** Enable / disable the local camera (drives the real track + the
   *  store). */
  setCameraEnabled(on: boolean): void;
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
    handRaised: false,
    quality: "good",
    speaking: false,
    screenSharing: false,
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

  const localName = options.localDisplayName ?? "You";
  const nameOf = (
    peerId: string,
    pseudonym: string,
    peerTag: string | undefined,
  ): string =>
    options.resolveDisplayName?.(peerId, pseudonym, peerTag) ??
    (pseudonym.length > 0 ? pseudonym.slice(0, 8) : "Peer");

  const localParticipant = (): Participant => ({
    id: "local",
    displayName: localName,
    isLocal: true,
    micOn: config.media.audio,
    camOn: config.media.video,
    handRaised: false,
    quality: "good",
    speaking: false,
    screenSharing: false,
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
      callActions.setParticipantCamera(participantId, true);
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
    },
    onWelcome: (selfPeerId, roster): void => {
      const remotes = roster
        .filter((p: SfuPeerSnapshot) => p.peer_id !== selfPeerId)
        .map((p: SfuPeerSnapshot) =>
          remoteParticipant(
            nameOf(p.peer_id, p.account_pseudonym, p.peer_tag),
            p.peer_id,
          ),
        );
      callActions.setParticipants([localParticipant(), ...remotes]);
    },
    onPeerJoined: (peerId, accountPseudonym, peerTag): void => {
      callActions.participantJoined(
        remoteParticipant(nameOf(peerId, accountPseudonym, peerTag), peerId),
      );
    },
    onPeerLeft: (peerId): void => {
      callActions.participantLeft(peerParticipantId(peerId));
    },
    onRemoteTrack: (rt): void => {
      addStream(rt);
    },
    onLocalStream: (stream): void => {
      setLocalStream(stream);
    },
  });

  return {
    session,
    remoteStreams,
    localStream,
    connect: (): Promise<void> => session.connect(),
    hangUp: (): void => {
      session.hangUp();
      setRemoteStreams(new Map());
      setLocalStream(null);
      callActions.reset();
    },
    // Track-only gates — the store's local-participant micOn / camOn is
    // the source of truth (driven by the Call.tsx buttons), so these do
    // NOT write back to the store (that would loop the Chat.tsx effect
    // that reads the store and calls these). Mirrors the 1×1 path.
    setMicrophoneEnabled: (on): void => session.setMicrophoneEnabled(on),
    setCameraEnabled: (on): void => session.setCameraEnabled(on),
    switchAudioInput: (deviceId): Promise<void> =>
      session.switchAudioInput(deviceId),
    switchVideoInput: (deviceId): Promise<void> =>
      session.switchVideoInput(deviceId),
    rekey: (newSeed): void => session.rekey(newSeed),
  };
}
