// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0091 phase 2 section 3, route 3 - every member of a group holds the
 * group's knock PUBLIC key, so a member that fell behind can seal its request
 * to come back to the owner and the admins (who hold the knock seeds).
 *
 * - **The owner** sends `(knock_version, knock_pk)` in-group as a
 *   `lakko-contacts::knock_key` envelope (60 bytes), on a version change and
 *   when the group grew since the last send, like the group name
 *   ({@link sendKnockKeyIfDue}).
 * - **A member** keeps one only from the owner or an admin resolved at receipt
 *   (C-68), and only when it is strictly newer (`knockKeyIsNewer`)
 *   ({@link consumeKnockKeyRow}). It is never shown.
 */

import { base64Decode, base64Encode } from "../auth/account-id";
import {
  decodeKnockKeyEnvelope as wasmDecodeKnockKeyEnvelope,
  knockKeyIsNewer as wasmKnockKeyIsNewer,
  type Group as WasmGroup,
} from "../../wasm/lakko_ffi_wasm";
import { heldKnockKey, knockKeySendDue, knockKeySentMark, setKnockKeySentMark, storeKnockKey } from "./reentry-store";

/** The envelope's one length (`lakko-contacts::knock_key`). */
export const KNOCK_KEY_ENVELOPE_LEN = 60;

/** The part of a processed message this reads. */
export interface ProcessedForKnockKey {
  readonly kind: string;
  readonly applicationData?: Uint8Array | undefined;
  readonly senderLeaf?: number | undefined;
}

/** The decoders, injectable for the tests. */
export interface KnockKeyCodec {
  decode(bytes: Uint8Array): { readonly version: number; readonly publicKey: Uint8Array } | null;
  isNewer(incoming: number, stored: number | undefined): boolean;
}

const moduleCodec: KnockKeyCodec = {
  decode(bytes) {
    try {
      const k = wasmDecodeKnockKeyEnvelope(bytes);
      try {
        return { version: k.knockVersion, publicKey: k.knockPk };
      } finally {
        k.free();
      }
    } catch {
      return null;
    }
  },
  isNewer(incoming, stored) {
    try {
      return wasmKnockKeyIsNewer(incoming, stored ?? undefined);
    } catch {
      return false;
    }
  },
};

function sameBytes(a: Uint8Array, b: Uint8Array): boolean {
  return a.length === b.length && a.every((x, i) => x === b[i]);
}

/** Whether `accountKey` is the owner or an admin of `group` now. */
export function isOwnerOrAdmin(group: Pick<WasmGroup, "roleTableOwner" | "roleTableAdmins">, accountKey: Uint8Array): boolean {
  try {
    const owner = group.roleTableOwner();
    if (owner !== undefined && sameBytes(owner, accountKey)) return true;
    const flat = group.roleTableAdmins();
    for (let i = 0; i + 32 <= flat.length; i += 32) if (sameBytes(flat.slice(i, i + 32), accountKey)) return true;
  } catch {
    return false;
  }
  return false;
}

/**
 * Consume a knock-key envelope at receipt. Returns `true` when the row was
 * one (kept or not); `false` for any other row. `resolveSender` maps the
 * sender leaf to its account key (standard base64) against the roster NOW.
 */
export async function consumeKnockKeyRow(
  group: WasmGroup,
  processed: ProcessedForKnockKey,
  resolveSender: (leaf: number) => string | undefined,
  codec: KnockKeyCodec = moduleCodec,
): Promise<boolean> {
  const bytes = processed.applicationData;
  if (processed.kind !== "Application" || bytes === undefined || bytes.length !== KNOCK_KEY_ENVELOPE_LEN) return false;
  const decoded = codec.decode(bytes);
  if (decoded === null) return false;
  let groupIdB64: string;
  try {
    groupIdB64 = base64Encode(group.groupId);
  } catch {
    return true;
  }
  const senderB64 = processed.senderLeaf === undefined ? undefined : resolveSender(processed.senderLeaf);
  if (senderB64 === undefined) return true;
  let sender: Uint8Array;
  try {
    sender = base64Decode(senderB64);
  } catch {
    return true;
  }
  if (!isOwnerOrAdmin(group, sender)) return true;
  const held = heldKnockKey(groupIdB64);
  if (!codec.isNewer(decoded.version, held?.version)) return true;
  storeKnockKey(groupIdB64, { version: decoded.version, publicKeyB64: base64Encode(decoded.publicKey) });
  return true;
}

/** What the owner's send needs. */
export interface KnockKeySendPorts {
  /** The group's current knock key as the owner holds it; `null` when this
   *  device is not the owner or holds none. */
  ownerKnockKey(group: WasmGroup): Promise<{ readonly version: number; readonly publicKey: Uint8Array } | null>;
  memberCount(group: WasmGroup): number;
  compose(version: number, publicKey: Uint8Array): Uint8Array;
  broadcast(group: WasmGroup, envelope: Uint8Array): Promise<void>;
}

/**
 * The owner sends the knock key when it changed version or the group grew
 * since the last send. Never throws; a failed send is tried at the next poll.
 */
export async function sendKnockKeyIfDue(group: WasmGroup, ports: KnockKeySendPorts): Promise<"sent" | "not-due" | "not-owner" | "failed"> {
  let groupIdB64: string;
  try {
    groupIdB64 = base64Encode(group.groupId);
  } catch {
    return "failed";
  }
  try {
    const key = await ports.ownerKnockKey(group);
    if (key === null) return "not-owner";
    const members = ports.memberCount(group);
    if (!knockKeySendDue(knockKeySentMark(groupIdB64), key.version, members)) return "not-due";
    await ports.broadcast(group, ports.compose(key.version, key.publicKey));
    setKnockKeySentMark(groupIdB64, `${key.version}:${members}`);
    return "sent";
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn(`chat knock key not sent: ${err instanceof Error ? err.name : "error"}`);
    return "failed";
  }
}
