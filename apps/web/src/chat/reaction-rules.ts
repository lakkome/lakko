// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The reaction presentation rules (`docs/ux/reactions-and-emoji.md` sections
 * 5 to 10), read from the core's `lakko_ui_rules::reaction` over the wasm
 * surface: the reaction row, the expand control, what a press does, and the
 * chips under a message. Nothing here decides a rule; it converts between the
 * shapes the views hold (base64 keys, plain strings) and the wasm calls, so
 * the three clients cannot disagree.
 *
 * Synchronous: called from render paths, after the wasm module is loaded (the
 * chat view loads it before a message can be drawn).
 */

import {
  AllowedReactions as WasmAllowedReactions,
  chipSourcesFromHeld,
  HeldReaction as WasmHeldReaction,
  maxReactionsPerMessage,
  quickReactionRow,
  reactionChips,
  reactionShown,
  reactionTapVerdict,
  rowHasExpand,
} from "../../wasm/lakko_ffi_wasm";
import { base64Decode, base64Encode } from "../auth/account-id";
import type { HeldReactionRecord, MessageOpState } from "./message-ops";

/** What a conversation takes (`AllowedReactions` in the core). A one-to-one
 *  always takes every reaction; a group's owner or admin chooses (section 9). */
export type AllowedSetting =
  | { readonly mode: "all" }
  | { readonly mode: "off" }
  | { readonly mode: "only"; readonly emoji: readonly string[] };

export const ALLOW_ALL: AllowedSetting = { mode: "all" };

/** Where the chips are drawn. */
export type ChipConversation = "one-to-one" | "group" | "channel";

/** What pressing an emoji does (`reactionTapVerdict`). */
export type TapVerdict = "add" | "remove" | "not-allowed" | "cap-reached" | "kind-limit";

/** One chip as drawn. `reactors` are base64 account keys (the avatars face). */
export interface ChipView {
  readonly emoji: string;
  readonly count: number;
  readonly mine: boolean;
  readonly face: "emoji" | "count" | "avatars";
  readonly reactors: readonly string[];
}

export interface ChipRowView {
  readonly chips: readonly ChipView[];
  /** How many more kinds fold into the last `+N` chip. */
  readonly overflow: number;
}

/** The wasm value of a setting, or `null` before the module is loaded. */
function wasmAllowed(a: AllowedSetting): WasmAllowedReactions | null {
  try {
    if (a.mode === "off") return WasmAllowedReactions.off();
    if (a.mode === "only") {
      try {
        return WasmAllowedReactions.only([...a.emoji]);
      } catch {
        return WasmAllowedReactions.off();
      }
    }
    return WasmAllowedReactions.all();
  } catch {
    return null;
  }
}

/** The most reactions one person gives one message (3). */
export function maxPerPerson(): number {
  try {
    return maxReactionsPerMessage();
  } catch {
    return 3;
  }
}

/** The kinds shown on a message now (for the row and the kind limit). */
export function shownKinds(state: MessageOpState | undefined, allowed: AllowedSetting): string[] {
  if (state === undefined) return [];
  const a = wasmAllowed(allowed);
  if (a === null) return [];
  try {
    const out: string[] = [];
    for (const emoji of state.reactions.keys()) if (reactionShown(emoji, a)) out.push(emoji);
    return out;
  } finally {
    a.free();
  }
}

/** The six emoji of the reaction row (section 5): the quick reaction, then the
 *  person's recent reactions, then the defaults, narrowed to what the
 *  conversation takes. Empty when it takes none. */
export function reactionRow(
  quick: string | null,
  recent: readonly string[],
  allowed: AllowedSetting,
  messageKinds: readonly string[],
): string[] {
  const a = wasmAllowed(allowed);
  if (a === null) return [];
  try {
    return quickReactionRow(quick, [...recent], a, [...messageKinds]);
  } catch {
    return [];
  } finally {
    a.free();
  }
}

/** Whether the row shows its trailing expand control. */
export function rowExpands(allowed: AllowedSetting): boolean {
  const a = wasmAllowed(allowed);
  if (a === null) return false;
  try {
    return rowHasExpand(a);
  } finally {
    a.free();
  }
}

/** Whether a conversation shows this reaction (allowed, and one emoji). */
export function shown(emoji: string, allowed: AllowedSetting): boolean {
  const a = wasmAllowed(allowed);
  if (a === null) return false;
  try {
    return reactionShown(emoji, a);
  } finally {
    a.free();
  }
}

/** What a row press, a chip tap or a double tap of `emoji` does. */
export function tapVerdict(
  emoji: string,
  own: readonly string[],
  allowed: AllowedSetting,
  messageKinds: readonly string[],
): TapVerdict {
  const a = wasmAllowed(allowed);
  if (a === null) return "not-allowed";
  try {
    const v = reactionTapVerdict(emoji, [...own], a, [...messageKinds]);
    return v === "add" || v === "remove" || v === "cap-reached" || v === "kind-limit" ? v : "not-allowed";
  } catch {
    return "not-allowed";
  } finally {
    a.free();
  }
}

function personBytes(b64: string): Uint8Array | null {
  try {
    const b = base64Decode(b64);
    return b.length === 32 ? b : null;
  } catch {
    return null;
  }
}

function splitKeys(flat: Uint8Array): string[] {
  const out: string[] = [];
  for (let i = 0; i + 32 <= flat.length; i += 32) out.push(base64Encode(flat.slice(i, i + 32)));
  return out;
}

/** The chips under a message (section 7): hidden when not taken, most people
 *  first, at most eleven, avatars in a group when three or fewer gave it. */
export function chipRow(
  held: readonly HeldReactionRecord[],
  ownKeyB64: string | null,
  conversation: ChipConversation,
  allowed: AllowedSetting,
): ChipRowView {
  if (held.length === 0) return { chips: [], overflow: 0 };
  const own = ownKeyB64 === null ? null : personBytes(ownKeyB64);
  const a = wasmAllowed(allowed);
  if (a === null) return { chips: [], overflow: 0 };
  try {
    const inputs: WasmHeldReaction[] = [];
    for (const h of held) {
      const p = personBytes(h.person);
      if (p === null) continue;
      inputs.push(new WasmHeldReaction(p, h.emoji, BigInt(Math.max(0, h.at))));
    }
    if (inputs.length === 0) return { chips: [], overflow: 0 };
    const sources = chipSourcesFromHeld(inputs, own ?? new Uint8Array(32));
    const row = reactionChips(sources, conversation, a);
    try {
      const chips: ChipView[] = row.chips.map((c) => {
        const face = c.face === "avatars" || c.face === "count" ? c.face : "emoji";
        const view: ChipView = {
          emoji: c.emoji,
          count: c.count,
          mine: own !== null && c.mine,
          face,
          reactors: face === "avatars" ? splitKeys(c.reactors) : [],
        };
        c.free();
        return view;
      });
      return { chips, overflow: row.overflowKinds };
    } finally {
      row.free();
    }
  } catch {
    return { chips: [], overflow: 0 };
  } finally {
    a.free();
  }
}

/** Every person who gave each emoji, for the reactions list (section 8.1):
 *  emoji in the chip order given, people in the order they gave it. */
export function reactorsByEmoji(
  held: readonly HeldReactionRecord[],
): Map<string, string[]> {
  const out = new Map<string, string[]>();
  for (const h of held) {
    const list = out.get(h.emoji) ?? [];
    if (!list.includes(h.person)) list.push(h.person);
    out.set(h.emoji, list);
  }
  return out;
}
