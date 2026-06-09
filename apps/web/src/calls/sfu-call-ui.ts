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
   *  Falls back to a short slice of the account pseudonym. */
  readonly resolveDisplayName?: (
    peerId: string,
    accountPseudonym: string,
  ) => string;
}

/** Handle returned by {@link createSfuCallUi}. */
export interface SfuCallUiHandle {
  /** The underlying session (for direct control if needed). */
  readonly session: SfuCallSession;
  /** Reactive map: remote MSID stream id → its `MediaStream`. The view
   *  mounts a sink per entry. */
  readonly remoteStreams: Accessor<ReadonlyMap<string, MediaStream>>;
  /** Open the SFU connection + publish (delegates to the session). */
  connect(): Promise<void>;
  /** Tear down + reset the call-UI store. Idempotent. */
  hangUp(): void;
  /** Mute / unmute the local mic (drives the real track + the store). */
  setMicrophoneEnabled(on: boolean): void;
  /** Enable / disable the local camera (drives the real track + the
   *  store). */
  setCameraEnabled(on: boolean): void;
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

  const localName = options.localDisplayName ?? "You";
  const nameOf = (peerId: string, pseudonym: string): string =>
    options.resolveDisplayName?.(peerId, pseudonym) ??
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
          // SFrame keying is armed at connect (the seed is present),
          // so media is end-to-end encrypted the moment we are
          // connected. Fingerprint string is a deferred follow-up.
          callActions.setE2ee({ active: true, shortFingerprint: null });
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
          remoteParticipant(nameOf(p.peer_id, p.account_pseudonym), p.peer_id),
        );
      callActions.setParticipants([localParticipant(), ...remotes]);
    },
    onPeerJoined: (peerId, accountPseudonym): void => {
      callActions.participantJoined(
        remoteParticipant(nameOf(peerId, accountPseudonym), peerId),
      );
    },
    onPeerLeft: (peerId): void => {
      callActions.participantLeft(peerParticipantId(peerId));
    },
    onRemoteTrack: (rt): void => {
      addStream(rt);
    },
  });

  return {
    session,
    remoteStreams,
    connect: (): Promise<void> => session.connect(),
    hangUp: (): void => {
      session.hangUp();
      setRemoteStreams(new Map());
      callActions.reset();
    },
    // Track-only gates — the store's local-participant micOn / camOn is
    // the source of truth (driven by the Call.tsx buttons), so these do
    // NOT write back to the store (that would loop the Chat.tsx effect
    // that reads the store and calls these). Mirrors the 1×1 path.
    setMicrophoneEnabled: (on): void => session.setMicrophoneEnabled(on),
    setCameraEnabled: (on): void => session.setCameraEnabled(on),
  };
}
