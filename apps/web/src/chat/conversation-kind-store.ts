// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The persisted conversation kind (docs/ux/client-redesign.md sections 3 and
 * 5.1): one `chat` / `group` per conversation, keyed by account and group id,
 * so a group created with two people lists under Groups from the start and a
 * conversation never moves between the two lists on its own.
 *
 * A `localStorage` map per account (`lakko.conversation-kind.v1.<account>`)
 * holding public bookkeeping only: which list a conversation is shown in.
 * No name, no member, no message. The decision rules are in
 * `conversation-kind.ts`; this module applies them to what is stored and
 * tells same-page listeners when a kind changes so the two lists re-split.
 */
import {
  decideConversationKind,
  nextPersistedKind,
  type ConversationKind,
} from "./conversation-kind";

const KEY_PREFIX = "lakko.conversation-kind.v1.";

type KindMap = { [groupIdB64: string]: ConversationKind };

const listeners = new Set<(accountIdB64: string) => void>();

function storageKey(accountIdB64: string): string {
  return `${KEY_PREFIX}${accountIdB64}`;
}

function readMap(accountIdB64: string): KindMap {
  try {
    const raw = globalThis.localStorage?.getItem(storageKey(accountIdB64));
    if (raw === null || raw === undefined) return {};
    const parsed: unknown = JSON.parse(raw);
    if (parsed === null || typeof parsed !== "object" || Array.isArray(parsed)) return {};
    const out: KindMap = {};
    for (const [k, v] of Object.entries(parsed as Record<string, unknown>)) {
      if (v === "chat" || v === "group") out[k] = v;
    }
    return out;
  } catch {
    return {};
  }
}

function writeMap(accountIdB64: string, map: KindMap): void {
  try {
    globalThis.localStorage?.setItem(storageKey(accountIdB64), JSON.stringify(map));
  } catch {
    /* storage unavailable: the lists fall back to the people count */
  }
  for (const l of listeners) {
    try {
      l(accountIdB64);
    } catch {
      /* a failing listener must not break the write */
    }
  }
}

/** The persisted kind of one conversation, or `null` when never recorded. */
export function persistedConversationKind(
  accountIdB64: string,
  groupIdB64: string,
): ConversationKind | null {
  return readMap(accountIdB64)[groupIdB64] ?? null;
}

/**
 * A flow states the kind outright: the new-group form (`group`), a contact
 * accept or New chat (`chat`), adding a member to a 1x1 (`group`). A group
 * never becomes a chat again.
 */
export function recordConversationKind(
  accountIdB64: string,
  groupIdB64: string,
  kind: ConversationKind,
): void {
  const map = readMap(accountIdB64);
  const next = nextPersistedKind(map[groupIdB64] ?? null, kind);
  if (map[groupIdB64] === next) return;
  map[groupIdB64] = next;
  writeMap(accountIdB64, map);
}

/**
 * The kind to show for a conversation whose people count is `peopleCount`
 * right now. Derives and persists it at first sight (a Welcome, a restore),
 * and persists the one allowed flip, chat -> group, when more than two people
 * are observed. Reads only when nothing changes.
 */
export function observeConversationKind(
  accountIdB64: string,
  groupIdB64: string,
  peopleCount: number,
): ConversationKind {
  const map = readMap(accountIdB64);
  const current = map[groupIdB64] ?? null;
  const decided = decideConversationKind(current, peopleCount);
  // A torn roster (zero people) is not a first sight worth recording: the
  // next read with a real roster decides.
  if (decided !== current && (current !== null || peopleCount > 0)) {
    map[groupIdB64] = decided;
    writeMap(accountIdB64, map);
  }
  return decided;
}

/** Forget every kind of an account (`forgetDevice`, a deleted chat). */
export function clearConversationKinds(accountIdB64: string): void {
  try {
    globalThis.localStorage?.removeItem(storageKey(accountIdB64));
  } catch {
    /* nothing to clear */
  }
}

/** Forget one conversation's kind (the chat was deleted from this device). */
export function forgetConversationKind(accountIdB64: string, groupIdB64: string): void {
  const map = readMap(accountIdB64);
  if (!(groupIdB64 in map)) return;
  delete map[groupIdB64];
  writeMap(accountIdB64, map);
}

/** Same-page notification that some account's kinds changed. */
export function subscribeConversationKindChanged(
  listener: (accountIdB64: string) => void,
): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}
