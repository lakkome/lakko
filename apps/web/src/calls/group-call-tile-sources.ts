// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * What the call screen asks a chat-group call about a person's pictures.
 *
 * The call screen takes six functions keyed by a roster participant id
 * (`peer-<peer id>`): the person's main stream, their light companion, their
 * middle track, what the SFU says it gives of them, a way for a tile to
 * say which picture it shows, and the receiver that decodes their audio. The host answers them from the live call's
 * handle.
 *
 * They are built here, over "the handle now, or none", for one reason: the
 * screen calls them while it is being taken down. A tile that is disposed
 * tells the call it shows no picture any more, and a picture re-reads its
 * streams on the way out. Written inline in the host's view they read the
 * handle through the accessor a `<Show>` hands its child, which throws once
 * the handle is gone (`calls/held.ts`), and the handle is gone at exactly
 * that moment: the host clears it first and closes the session second. With
 * these, a call that is over answers "nothing" and takes no report.
 *
 * Pure: no DOM, no Solid of its own; the accessors it is handed are.
 */

import type { LevelReceiver } from "./audio-level";
import type { VideoQuality } from "./video-wants";

/** The part of a group call's handle the tiles read
 *  (`SfuCallUiHandle` satisfies it). */
export interface TileSourceHandle {
  readonly remoteStreams: () => ReadonlyMap<string, MediaStream>;
  readonly remoteLightStreams: () => ReadonlyMap<string, MediaStream>;
  readonly remoteMiddleStreams: () => ReadonlyMap<string, MediaStream>;
  readonly remoteVideoGiven: () => ReadonlyMap<string, VideoQuality>;
  readonly localStream: () => MediaStream | null;
  reportTileVideoQuality(tile: object, peerId: string, quality: VideoQuality | null): void;
  /** The receiver that decodes a peer's audio, when the handle has one. */
  remoteAudioReceiverFor?(peerId: string): LevelReceiver | undefined;
}

/** The five functions the call screen takes, and the local stream. */
export interface GroupCallTileSources {
  readonly remoteStreamFor: (participantId: string) => MediaStream | undefined;
  readonly remoteLightStreamFor: (participantId: string) => MediaStream | undefined;
  readonly remoteMiddleStreamFor: (participantId: string) => MediaStream | undefined;
  readonly remoteVideoGivenFor: (participantId: string) => VideoQuality | undefined;
  readonly reportTileVideoQuality: (
    tile: object,
    participantId: string,
    quality: VideoQuality | null,
  ) => void;
  readonly localStream: () => MediaStream | undefined;
  /** The receiver that decodes a person's audio, for their sound level. */
  readonly remoteAudioReceiverFor: (participantId: string) => LevelReceiver | undefined;
  /** Every remote stream, for the audio sinks. */
  readonly remoteStreamList: () => MediaStream[];
}

const PEER_PREFIX = /^peer-/;

/** The SFU peer id of a roster participant id: the roster id is
 *  `peer-<peer id>`, and the SFU sets the downstream MSID stream id to that
 *  same peer id. */
export function peerIdOfRosterId(participantId: string): string {
  return participantId.replace(PEER_PREFIX, "");
}

/**
 * The tile sources of the call `handle` names now. With no call (`null`)
 * every lookup answers `undefined` and a report is dropped.
 */
export function groupCallTileSources(handle: () => TileSourceHandle | null): GroupCallTileSources {
  return {
    remoteStreamFor: (id) => handle()?.remoteStreams().get(peerIdOfRosterId(id)),
    // A publisher's light companion, keyed by the same peer id
    // (`calls/light-stream.ts`). Video only: it never gets an audio sink.
    remoteLightStreamFor: (id) => handle()?.remoteLightStreams().get(peerIdOfRosterId(id)),
    // Their middle track (ADR-0070 step 5), keyed the same way.
    remoteMiddleStreamFor: (id) => handle()?.remoteMiddleStreams().get(peerIdOfRosterId(id)),
    // What the SFU says it gives of them (ADR-0070 step 6,
    // `calls/video-given.ts`); nothing with downlink control off.
    remoteVideoGivenFor: (id) => handle()?.remoteVideoGiven().get(peerIdOfRosterId(id)),
    // What a tile shows of a person, for the SFU (`calls/video-wants.ts`).
    reportTileVideoQuality: (tile, id, quality) => {
      handle()?.reportTileVideoQuality(tile, peerIdOfRosterId(id), quality);
    },
    localStream: () => handle()?.localStream() ?? undefined,
    // Read on this device only, for the speaking ring and the large tile
    // (`calls/speaking-monitor.ts`).
    remoteAudioReceiverFor: (id) => handle()?.remoteAudioReceiverFor?.(peerIdOfRosterId(id)),
    remoteStreamList: () => {
      const h = handle();
      return h === null ? [] : [...h.remoteStreams().values()];
    },
  };
}
