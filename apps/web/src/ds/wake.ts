// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The `x-lakko-wake` header of a group-journal publish (ADR-0054 decision 1,
 * ADR-0078 section 3).
 *
 * A wake is a consequence of a signed publish: the client that wants the
 * recipients woken adds this header to its `POST /ds/group/<hex>/messages`,
 * and the DS forwards it to push-gw only after the ADR-0053 proof gate passed
 * and the row was appended. The client never talks to push-gw to wake anybody.
 *
 * Wire form (ADR-0078): a wake **sealed to push-gw's X25519 key** — the
 * group's `push_gid`, the kind, a nonce, a timestamp, the group's unlock key
 * and a signature by the group's wake key — as base64url WITHOUT padding,
 * exactly {@link DS_WAKE_HEADER_LEN} characters. It is built by the wasm core
 * (`push/group-routing.ts` `sealedWakeForGroup`); the DS checks its shape and
 * forwards it unread, so it no longer learns which group is woken or whether
 * the wake is a call. A header of the wrong shape is answered 400, which
 * would fail the publish itself, so the length is checked here first.
 *
 * No header is the silent publish: a control envelope (a beacon, a typing
 * signal, a routing-secret handshake), or a publish by a device that does not
 * hold the group's routing secret yet (the ADR-0042 defer).
 */

/** The header name, lowercase as sent. */
export const DS_WAKE_HEADER = "x-lakko-wake";

/** Length of the header value: base64url of a 234-byte sealed wake (mirror
 *  of `lakko_contacts::push_wake::PUSH_WAKE_HEADER_LEN`). */
export const DS_WAKE_HEADER_LEN = 312;

/**
 * The wake class. `message` is the ordinary message wake; `call` is a ring,
 * which push-gw delivers at the transport's highest priority with a short TTL
 * (`docs/ux/calls.md` section 7). It is sealed inside the header.
 */
export type DsWakeKind = "message" | "call";

/** What a publish asks the DS to forward: a sealed wake. */
export interface DsWake {
  /** The header value, as `sealedWakeForGroup` built it. */
  readonly headerValue: string;
}

/** A wake that cannot be put on the wire (the wrong shape). */
export class DsWakeArgError extends Error {
  override name = "DsWakeArgError";
}

/**
 * The header value for `wake`. Throws {@link DsWakeArgError} when it is not
 * {@link DS_WAKE_HEADER_LEN} base64url characters.
 */
export function dsWakeHeaderValue(wake: DsWake): string {
  const v = wake.headerValue;
  if (v.length !== DS_WAKE_HEADER_LEN || !/^[A-Za-z0-9_-]+$/.test(v)) {
    throw new DsWakeArgError(
      `a sealed wake must be ${DS_WAKE_HEADER_LEN} base64url characters (got ${v.length})`,
    );
  }
  return v;
}
