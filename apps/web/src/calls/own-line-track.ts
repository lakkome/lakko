// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * A remote track that is nobody: the receiver of one of this device's own
 * send lines.
 *
 * `addTrack` makes a `sendrecv` transceiver, and the SFU answers a peer's own
 * offer with the inverted direction, so each of this device's send lines is
 * answered `sendrecv` with a random msid (`services/sfu/src/media.rs`,
 * "lifecycle point 0"). The browser then raises `ontrack` for a receiver the
 * SFU never points any media at. Its stream id is that random msid, which the
 * call took for a peer: the media counters printed a `sync peer=<uuid>` line
 * that never flowed in a call with nobody else in it (seen 2026-11-07 on the
 * desktop app). Every real picture and voice of another person arrives on a
 * line the SFU offered, and a line created by a remote offer has the
 * direction `recvonly` on this side; a line this device created to send on
 * keeps `sendrecv` or `sendonly`.
 *
 * Both tests must hold before a track is set aside: the line is one this
 * device sends on, AND its stream id is not a peer's. A peer's stream id is
 * the SFU peer id (32 lower-case hex, ADR-0078) with at most one of the role
 * suffixes (`light-stream.ts`), while the phantom's is the SFU's random msid.
 * The second test keeps a real picture even if a browser hands it over on a
 * line `addTrack` reused for sending.
 */
export function isOwnLinePhantom(direction: string | undefined | null, peerIdOfStream: string): boolean {
  return isOwnSendLine(direction) && !SFU_PEER_ID.test(peerIdOfStream);
}

/** A line this device created to send on. */
export function isOwnSendLine(direction: string | undefined | null): boolean {
  return direction === "sendrecv" || direction === "sendonly";
}

const SFU_PEER_ID = /^[0-9a-f]{32}$/;
