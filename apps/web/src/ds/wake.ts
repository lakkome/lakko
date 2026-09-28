// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The `x-lakko-wake` header of a group-journal publish (ADR-0054 decision 1).
 *
 * A wake is a consequence of a signed publish: the client that wants the
 * recipients woken adds this header to its `POST /ds/group/<hex>/messages`,
 * and the DS forwards `(push_gid, message_id, kind)` to push-gw only after the
 * ADR-0053 proof gate passed and the row was appended. The client never talks
 * to push-gw to wake anybody.
 *
 * Wire form: `<push_gid>;<kind>`, where `<push_gid>` is the group's 32-byte
 * ADR-0042 push routing id as base64url WITHOUT padding (always 43 chars) and
 * `<kind>` is `message` or `call`. The DS answers 400 to a malformed header,
 * which would fail the publish itself, so the value is built here and nowhere
 * else, and a wrong-length id is refused before any request is made.
 *
 * No header is the silent publish: a control envelope (a beacon, a typing
 * signal, a routing-secret handshake), or a publish by a device that does not
 * hold the group's routing secret yet. The latter is the ADR-0042 defer - the
 * DS `group_id` is never put in the header in its place (invariant #5).
 */
import { base64UrlEncodeNoPad } from "../auth/account-id";

/** The header name, lowercase as sent. */
export const DS_WAKE_HEADER = "x-lakko-wake";

/** Length of an ADR-0042 `push_gid` in bytes. */
export const PUSH_GID_LEN = 32;

/**
 * The wake class. `message` is the ordinary message wake; `call` is a ring,
 * which push-gw delivers at the transport's highest priority with a short TTL
 * (`docs/ux/calls.md` section 7).
 */
export type DsWakeKind = "message" | "call";

/** What a publish asks the DS to wake. */
export interface DsWake {
  /** The group's 32-byte `push_gid` - never its DS `group_id`. */
  readonly pushGid: Uint8Array;
  readonly kind: DsWakeKind;
}

/** A wake that cannot be put on the wire (a wrong-length `push_gid`). */
export class DsWakeArgError extends Error {
  override name = "DsWakeArgError";
}

/**
 * The header value for `wake`: `<43-char base64url push_gid>;<kind>`.
 * Throws {@link DsWakeArgError} when the id is not 32 bytes or the kind is not
 * one the DS accepts.
 */
export function dsWakeHeaderValue(wake: DsWake): string {
  if (wake.pushGid.length !== PUSH_GID_LEN) {
    throw new DsWakeArgError(
      `push_gid must be ${PUSH_GID_LEN} bytes (got ${wake.pushGid.length})`,
    );
  }
  if (wake.kind !== "message" && wake.kind !== "call") {
    throw new DsWakeArgError(`unknown wake kind: ${String(wake.kind)}`);
  }
  return `${base64UrlEncodeNoPad(wake.pushGid)};${wake.kind}`;
}
