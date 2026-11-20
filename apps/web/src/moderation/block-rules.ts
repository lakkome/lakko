// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0098 section 6 - what a block does on each path, asked of the ONE
 * shared rule (`lakko_ui_rules::block`, wasm `blockVerdict`). No path on this
 * client decides it itself: three hand-written copies of one table drift.
 *
 * The adapter only maps this client's question ("is this sender blocked, on
 * this path") to the rule's answer, and fails toward SHOWING: a core that
 * cannot answer (not loaded, an unknown path) never hides a message.
 */

import { isBlockedKeyB64 } from "./block-registry";

/** The paths the rule knows (kebab names on the wasm surface). */
export type BlockPath =
  | "one-to-one-message"
  | "group-message"
  | "contact-request"
  | "incoming-call"
  | "call-tile"
  | "notification"
  | "outgoing-signal"
  | "incoming-signal";

/** What the rule answers. */
export type BlockVerdict = "show" | "drop" | "collapse" | "decline-silently" | "label" | "withhold";

/** The wasm function this module asks, structurally (tests pass a fake). */
export interface BlockRuleWasm {
  blockVerdict(path: string, blocked: boolean): string;
}

const VERDICTS: readonly BlockVerdict[] = ["show", "drop", "collapse", "decline-silently", "label", "withhold"];

/** The rule's answer for `path` when the person is (or is not) blocked.
 *  `show` when the rule cannot be asked. */
export function blockVerdictFor(wasm: BlockRuleWasm | null, path: BlockPath, blocked: boolean): BlockVerdict {
  if (!blocked) return "show";
  if (wasm === null) return "show";
  try {
    const v = wasm.blockVerdict(path, blocked);
    return (VERDICTS as readonly string[]).includes(v) ? (v as BlockVerdict) : "show";
  } catch {
    return "show";
  }
}

/** The rule's answer for a sender, read off the register. */
export function blockVerdictForSender(
  wasm: BlockRuleWasm | null,
  path: BlockPath,
  senderKeyB64: string | null | undefined,
  isBlocked: (keyB64: string | null | undefined) => boolean = isBlockedKeyB64,
): BlockVerdict {
  return blockVerdictFor(wasm, path, isBlocked(senderKeyB64));
}

/**
 * The receive decision for one decrypted application event of `kind` from a
 * sender in a conversation of `conversation`, before it is stored or shown:
 *
 * - `drop`: not stored, not shown, no notification (a one-to-one message, and
 *   their typing, read receipts and profile updates in the one-to-one);
 * - `keep`: as before (a group message keeps its place and is collapsed when
 *   drawn; anything from someone not blocked).
 */
export function receiveDecision(
  wasm: BlockRuleWasm | null,
  conversation: "one-to-one" | "group",
  kind: "message" | "signal",
  blocked: boolean,
): "drop" | "keep" {
  if (!blocked) return "keep";
  const path: BlockPath =
    conversation === "one-to-one"
      ? kind === "message"
        ? "one-to-one-message"
        : "incoming-signal"
      : "group-message";
  if (conversation === "group" && kind === "signal") return "keep";
  return blockVerdictFor(wasm, path, blocked) === "drop" ? "drop" : "keep";
}

/** The event kinds a block can drop at receipt, and what each one is to the
 *  rule: a message (a chat message, an edit, a delete, a reaction) or a
 *  signal (typing, a read receipt, a profile, a profile request). */
export function receiveKindOf(
  eventKind: string,
  op?: string,
): "message" | "signal" | null {
  if (eventKind === "application") return "message";
  if (eventKind === "op") return op === "typing" || op === "read-receipt" ? "signal" : "message";
  if (eventKind === "profile" || eventKind === "profileRequest") return "signal";
  return null;
}

/**
 * Whether one received event is dropped before it is stored, shown or
 * notified: anything of a blocked person's in the one-to-one with them. Pure;
 * the caller hands in the conversation's kind and the register's answer.
 */
export function dropsAtReceipt(
  wasm: BlockRuleWasm | null,
  conversation: "one-to-one" | "group",
  event: { readonly kind: string; readonly op?: string; readonly senderKey?: string | undefined },
  isBlocked: (keyB64: string | null | undefined) => boolean = isBlockedKeyB64,
): boolean {
  const kind = receiveKindOf(event.kind, event.op);
  if (kind === null) return false;
  return receiveDecision(wasm, conversation, kind, isBlocked(event.senderKey)) === "drop";
}

/**
 * Whether this device withholds a signal (typing, a read receipt, a profile
 * update) from a conversation: the one-to-one with a blocked person. The
 * conversation's kind and its other person are the caller's reads; `null`
 * for a person who cannot be read is never withheld from.
 */
export function withholdsSignal(
  wasm: BlockRuleWasm | null,
  conversation: "one-to-one" | "group",
  otherPersonKeyB64: string | null,
  isBlocked: (keyB64: string | null | undefined) => boolean = isBlockedKeyB64,
): boolean {
  if (conversation !== "one-to-one" || otherPersonKeyB64 === null) return false;
  return blockVerdictFor(wasm, "outgoing-signal", isBlocked(otherPersonKeyB64)) === "withhold";
}
