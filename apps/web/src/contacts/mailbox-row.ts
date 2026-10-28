// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0078 phase 1 - every row in a person's mailbox is a SEALED row.
 *
 * A row is `version || eph_x25519_pk || AEAD(kind || body_len || body ||
 * padding)`, sealed by the core (`sealMailboxRow`) to the X25519 conversion of
 * the owner's MAILBOX key and padded to a size bucket. The kind (contact
 * request, Welcome, contact ack, Welcome rendezvous) is inside the seal: the
 * clear kind byte the web client used to prepend (`contacts/envelope.ts`) is
 * gone, and the Delivery Service sees the mailbox, the size bucket and the
 * reach proof only.
 *
 * A writer seals to a mailbox key it verified against the account key it
 * holds pinned for the person (`verifyMailboxBinding` /
 * `ReachGrant.mailboxPkVerified`, in `reach/mailbox-write.ts`); the owner
 * opens every row with its own {@link MailboxKey} and dispatches by kind. A
 * row that does not open (garbage, a row for another mailbox, an unknown
 * kind) is DROPPED and never surfaced.
 *
 * The bodies inside are unchanged: a signed `ContactRequest` or `ContactAck`
 * wire, a raw MLS Welcome, or a `WelcomeRendezvous`.
 */
import type { MailboxKey, MailboxKind as WasmMailboxKind } from "../../wasm/lakko_ffi_wasm";

/** The kinds a mailbox row carries, by name. Mirrors the frozen core values. */
export type MailboxRowKind =
  | "contact-request"
  | "welcome"
  | "contact-ack"
  | "welcome-rendezvous"
  /** ADR-0079 amendment of 2026-10-19: the inviter refused a request whose
   *  invite link no longer counts. */
  | "link-refusal";

/** Where one write goes: the person's verified mailbox key and its address. */
export interface MailboxTarget {
  /** The 32-byte DS address (`mailboxAddress(mailboxPk)`). */
  readonly address: Uint8Array;
  /** The 32-byte mailbox public key the row is sealed to. */
  readonly mailboxPk: Uint8Array;
}

/** The part of the wasm surface this module needs, structurally. */
export interface MailboxRowWasm {
  readonly MailboxKind: {
    readonly ContactRequest: WasmMailboxKind | number;
    readonly Welcome: WasmMailboxKind | number;
    readonly ContactAck: WasmMailboxKind | number;
    readonly WelcomeRendezvous: WasmMailboxKind | number;
    readonly LinkRefusal: WasmMailboxKind | number;
  };
  sealMailboxRow(mailboxPk: Uint8Array, kind: WasmMailboxKind, body: Uint8Array): Uint8Array;
  openMailboxRow(key: MailboxKey, row: Uint8Array): {
    readonly kind: WasmMailboxKind | number;
    readonly body: Uint8Array;
    free(): void;
  };
}

/** The core kind value of a named kind. */
export function wasmKindOf(wasm: MailboxRowWasm, kind: MailboxRowKind): WasmMailboxKind {
  switch (kind) {
    case "contact-request":
      return wasm.MailboxKind.ContactRequest as WasmMailboxKind;
    case "welcome":
      return wasm.MailboxKind.Welcome as WasmMailboxKind;
    case "contact-ack":
      return wasm.MailboxKind.ContactAck as WasmMailboxKind;
    case "welcome-rendezvous":
      return wasm.MailboxKind.WelcomeRendezvous as WasmMailboxKind;
    case "link-refusal":
      return wasm.MailboxKind.LinkRefusal as WasmMailboxKind;
  }
}

function nameOf(wasm: MailboxRowWasm, kind: number): MailboxRowKind | null {
  if (kind === wasm.MailboxKind.ContactRequest) return "contact-request";
  if (kind === wasm.MailboxKind.Welcome) return "welcome";
  if (kind === wasm.MailboxKind.ContactAck) return "contact-ack";
  if (kind === wasm.MailboxKind.WelcomeRendezvous) return "welcome-rendezvous";
  if (kind === wasm.MailboxKind.LinkRefusal) return "link-refusal";
  return null;
}

/** Seal `body` as one row of `kind` to `target`. Throws on a bad key or a
 *  body over the core's cap. */
export function sealRowFor(
  wasm: MailboxRowWasm,
  target: MailboxTarget,
  kind: MailboxRowKind,
  body: Uint8Array,
): Uint8Array {
  return wasm.sealMailboxRow(target.mailboxPk, wasmKindOf(wasm, kind), body);
}

/** One row of the own mailbox, opened. */
export interface OpenedRow {
  readonly kind: MailboxRowKind;
  readonly body: Uint8Array;
}

/**
 * Open one row of the own mailbox with `key`, or `null` when it does not
 * open: an unknown version, a length that is not a bucket, a row sealed for
 * another mailbox, a forged or damaged row, an unknown kind. The caller drops
 * a `null` row (the cursor still advances). Never throws, never logs.
 */
export function openOwnRow(wasm: MailboxRowWasm, key: MailboxKey, row: Uint8Array): OpenedRow | null {
  let opened: ReturnType<MailboxRowWasm["openMailboxRow"]>;
  try {
    opened = wasm.openMailboxRow(key, row);
  } catch {
    return null;
  }
  try {
    const kind = nameOf(wasm, Number(opened.kind));
    if (kind === null) return null;
    return { kind, body: opened.body };
  } finally {
    try {
      opened.free();
    } catch {
      /* already freed */
    }
  }
}
