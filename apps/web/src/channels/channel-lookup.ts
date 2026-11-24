// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * A public channel found by its exact `@name` (ADR-0087 sections 4 and 6):
 * one OPRF round and one lookup, the record opened with the name's own OPRF
 * output. What it answers is what subscribing needs: the id and the read key
 * the record carries, with the name and description to title the channel.
 * `null` when no public channel holds the name.
 */
import { lookupHandle } from "../discovery/handle-lookup";
import { namePair } from "../discovery/handle-check";
import { avatarFromWasm, type AvatarData } from "../profile/avatar-model";
import { loadWasm } from "../wasm";
import { bytesToHex } from "./composer";

export interface FoundChannel {
  readonly idHex: string;
  readonly readKey: Uint8Array;
  readonly handle: string;
  readonly name: string;
  readonly description: string;
  /** The channel's photo as the record carries it. */
  readonly avatar: AvatarData | null;
}

export async function lookupPublicChannel(handle: string): Promise<FoundChannel | null> {
  const wasm = await loadWasm();
  const pair = await namePair(handle);
  const record = await lookupHandle(pair.lookupKey);
  if (record === null) return null;
  let opened;
  try {
    opened = wasm.openSearchRecord(record, pair.output);
  } catch {
    return null;
  }
  if (opened === undefined) return null;
  try {
    const c = opened.conversation;
    if (c === undefined) return null;
    try {
      if (c.kind !== "channel" || c.handle !== pair.normalized) return null;
      return {
        idHex: bytesToHex(c.id),
        readKey: c.key,
        handle: c.handle,
        name: c.name,
        description: c.description,
        avatar: avatarFromWasm(c.avatar),
      };
    } finally {
      c.free();
    }
  } finally {
    opened.free();
  }
}
