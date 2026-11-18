// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0090 — turning an editor's draft into signed content, and the ops a
 * person sends on a poll or checklist. The author signs with the key its
 * leaf in this group is bound to (the conversation key, ADR-0074 section
 * 3.5), which is what every receiver resolves the sender to.
 */
import { base64Decode, base64Encode } from "../auth/account-id";
import { loadWasm } from "../wasm";
import type {
  ChecklistBody,
  MessageContent,
  PollBody,
  SignedChecklist,
} from "../../wasm/lakko_ffi_wasm";
import type { ChecklistDraft, PollDraft } from "./types";
import { compareKeyB64 } from "./content-store";

function randomB64(n: number): string {
  const b = new Uint8Array(n);
  crypto.getRandomValues(b);
  return base64Encode(b);
}

/** The hash of a ring (standard base64): the distinct account keys, sorted
 *  bytewise, through the core's `pollRingHash`. */
function ringHashOf(wasm: Awaited<ReturnType<typeof loadWasm>>, rosterAccountKeysB64: readonly string[]): string {
  const keys = [...new Set(rosterAccountKeysB64)].sort(compareKeyB64);
  const flat = new Uint8Array(keys.length * 32);
  keys.forEach((k, i) => flat.set(base64Decode(k), i * 32));
  return base64Encode(wasm.pollRingHash(flat));
}

/**
 * Whether a failed content message may be sent again as it is (Retry). An
 * anonymous poll signs the hash of its first ring, the roster when it was
 * made; receivers recompute the ring at the epoch the message lands, so a
 * poll re-sent after the roster changed would read as unverifiable to
 * everyone. It is refused then (the person makes it again); anything else,
 * and an anonymous poll whose roster is unchanged, may be re-sent.
 */
export async function contentRetryAllowed(
  content: MessageContent | undefined,
  rosterAccountKeysB64: readonly string[],
): Promise<boolean> {
  if (content === undefined || content.kind !== "poll") return true;
  const anonymous = content.poll.body.anonymous;
  if (anonymous === null || anonymous === undefined) return true;
  return ringHashOf(await loadWasm(), rosterAccountKeysB64) === anonymous.ring0Hash;
}

/** The poll body of a draft. An anonymous poll carries a fresh secret and
 *  the hash of version 0 of its ring: the roster's distinct account keys
 *  now, sorted bytewise (every receiver recomputes it at the same epoch). */
export async function pollBodyOf(draft: PollDraft, rosterAccountKeysB64: readonly string[], nowMs: number): Promise<PollBody> {
  const wasm = await loadWasm();
  let anonymous: PollBody["anonymous"] = null;
  if (draft.anonymous) {
    anonymous = { secret: randomB64(32), ring0Hash: ringHashOf(wasm, rosterAccountKeysB64) };
  }
  return {
    id: randomB64(16),
    question: draft.question.trim(),
    options: draft.options.map((o) => o.trim()),
    multiple: draft.multiple,
    quiz: draft.quiz === undefined ? null : { correct: draft.quiz.correct, explanation: draft.quiz.explanation.trim() },
    closeAtMs: draft.closeAfterMs === undefined ? null : nowMs + draft.closeAfterMs,
    anonymous,
  };
}

/** A signed poll as message content. */
export async function pollContentOf(
  draft: PollDraft,
  groupId: Uint8Array,
  authorSeed32: Uint8Array,
  rosterAccountKeysB64: readonly string[],
  nowMs: number,
): Promise<MessageContent> {
  const wasm = await loadWasm();
  const body = await pollBodyOf(draft, rosterAccountKeysB64, nowMs);
  return { kind: "poll", poll: wasm.signPoll(body, groupId, authorSeed32) };
}

/**
 * The checklist body of a draft. An item that already exists keeps its id
 * (its ticks stay with it); a new item gets the next free id.
 */
export function checklistBodyOf(draft: ChecklistDraft): ChecklistBody {
  let next = 1 + Math.max(0, ...draft.items.map((i) => i.id ?? 0));
  const items = draft.items.map((i) => ({ id: i.id ?? next++, text: i.text.trim() }));
  return { title: draft.title.trim(), othersAdd: draft.othersAdd, othersMark: draft.othersMark, items };
}

export async function checklistContentOf(
  draft: ChecklistDraft,
  groupId: Uint8Array,
  authorSeed32: Uint8Array,
): Promise<{ kind: "checklist"; checklist: SignedChecklist }> {
  const wasm = await loadWasm();
  return { kind: "checklist", checklist: wasm.signChecklist(checklistBodyOf(draft), groupId, authorSeed32) };
}

/** The 32-byte Ed25519 seed of an identity seed (64 bytes). */
export async function ed25519SeedOf(identitySeed64: Uint8Array): Promise<Uint8Array> {
  const wasm = await loadWasm();
  const identity = wasm.Identity.fromSeed(identitySeed64);
  try {
    return identity.ed25519Seed;
  } finally {
    identity.free();
  }
}
