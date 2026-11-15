// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0082 section 5 — what this device keeps of translations, transcripts
 * and the per-conversation choices.
 *
 * On this device only, encrypted under the device root key, behind the
 * ADR-0077 account store gate (`result-idb.ts`), and wiped by Forget device.
 * Never written to the backup, the conversation vault or the DeviceGroup
 * synced state (UX open question 1, decided "no" for v1): nothing outside
 * this module and its backend reads it.
 *
 * A result is filed under the message it describes and the slot it fills (a
 * translation of the text, a transcript, a translation of the transcript).
 * Whether a stored result still describes the message is the shared rule
 * `storedResultIsCurrent` (an edit moves the message's last op, a different
 * target language is a different translation); a stale one is ignored and
 * replaced on the next request, and a deleted message takes all three slots
 * with it.
 *
 * The row id is a digest of (account, slot, conversation, message), so the
 * database names neither a conversation nor a message. Each row also carries
 * a keyed tag of its conversation (`result-idb.ts`), so deleting a chat
 * deletes everything made from it.
 */

import { registerAccountMemory } from "../auth/account-memory";

/** A result's slot within one message. */
export type ResultSlot = "text" | "transcript" | "transcript-translation";

/** A translation of a message's text or of its transcript. */
export interface StoredTranslation {
  readonly kind: "translation";
  readonly lastOpId: number;
  readonly source: string;
  readonly target: string;
  readonly text: string;
}

/** A transcript of a voice or video message. */
export interface StoredTranscript {
  readonly kind: "transcript";
  readonly lastOpId: number;
  readonly language: string;
  readonly text: string;
  readonly complete: boolean;
}

export type StoredResult = StoredTranslation | StoredTranscript;

/** A language picked for a conversation (`Not X?` or the picker), and when. */
export interface LanguagePick {
  readonly tag: string;
  readonly atMs: number;
}

/** One message's confidently detected language. */
export interface MessageLanguage {
  readonly messageId: number;
  readonly tag: string;
}

/**
 * What this device knows of a conversation's languages (ADR-0082 note of
 * 2026-11-15), in the canonical bounded form the shared rule
 * (`ConversationLanguages`) keeps; changed only through it.
 */
export interface ConversationLanguagesRecord {
  readonly picks: readonly LanguagePick[];
  readonly detections: readonly MessageLanguage[];
}

export const NO_LANGUAGES: ConversationLanguagesRecord = { picks: [], detections: [] };

/** The per-conversation choices (UX section 8). */
export interface ConversationPrefs {
  readonly autoTranslate: boolean;
  /** The languages picked and confidently detected here. */
  readonly languages: ConversationLanguagesRecord;
  /** The language last chosen for a transcript. */
  readonly transcriptionLanguage: string | null;
  /** Languages for which the conversation bar was dismissed. */
  readonly dismissed: readonly string[];
}

export const DEFAULT_PREFS: ConversationPrefs = {
  autoTranslate: false,
  languages: NO_LANGUAGES,
  transcriptionLanguage: null,
  dismissed: [],
};

/** Whether a row is a result (counted, deleted with "Delete all") or prefs. */
export type RowClass = "result" | "prefs";

/** The encrypted rows, by id. Implemented over IndexedDB in production. */
export interface ResultBackend {
  get(accountId: string, id: string): Promise<Uint8Array | null>;
  put(accountId: string, id: string, cls: RowClass, plaintext: Uint8Array, conversation: string): Promise<void>;
  /** Delete every row of one conversation; the number deleted. */
  deleteConversation(accountId: string, conversation: string): Promise<number>;
  delete(accountId: string, ids: readonly string[]): Promise<void>;
  /** Delete every row of `cls` for the account; the number deleted. */
  deleteClass(accountId: string, cls: RowClass): Promise<number>;
  count(accountId: string, cls: RowClass): Promise<number>;
}

const ID_DOMAIN = "lakko/translation/row/v1";

/** The digest a row is filed under. */
export async function rowId(
  accountId: string,
  slot: ResultSlot | "prefs",
  conversation: string,
  messageId: number,
): Promise<string> {
  const material = new TextEncoder().encode(
    [ID_DOMAIN, accountId, slot, conversation, String(messageId)].join("\u0000"),
  );
  const digest = new Uint8Array(await crypto.subtle.digest("SHA-256", material));
  let hex = "";
  for (const b of digest) hex += b.toString(16).padStart(2, "0");
  return hex;
}

/** Decode a stored result, or `null` for anything that is not one. */
export function decodeResult(bytes: Uint8Array): StoredResult | null {
  let v: unknown;
  try {
    v = JSON.parse(new TextDecoder().decode(bytes));
  } catch {
    return null;
  }
  if (typeof v !== "object" || v === null) return null;
  const o = v as Record<string, unknown>;
  if (typeof o.lastOpId !== "number" || typeof o.text !== "string") return null;
  if (o.kind === "translation" && typeof o.source === "string" && typeof o.target === "string") {
    return { kind: "translation", lastOpId: o.lastOpId, source: o.source, target: o.target, text: o.text };
  }
  if (o.kind === "transcript" && typeof o.language === "string" && typeof o.complete === "boolean") {
    return {
      kind: "transcript",
      lastOpId: o.lastOpId,
      language: o.language,
      text: o.text,
      complete: o.complete,
    };
  }
  return null;
}

function decodeLanguages(v: unknown): ConversationLanguagesRecord {
  if (typeof v !== "object" || v === null) return NO_LANGUAGES;
  const o = v as Record<string, unknown>;
  const picks = Array.isArray(o.picks)
    ? o.picks.flatMap((p: unknown) => {
        const q = p as Record<string, unknown> | null;
        return q !== null && typeof q.tag === "string" && typeof q.atMs === "number" ? [{ tag: q.tag, atMs: q.atMs }] : [];
      })
    : [];
  const detections = Array.isArray(o.detections)
    ? o.detections.flatMap((d: unknown) => {
        const q = d as Record<string, unknown> | null;
        return q !== null && typeof q.tag === "string" && typeof q.messageId === "number"
          ? [{ messageId: q.messageId, tag: q.tag }]
          : [];
      })
    : [];
  return { picks, detections };
}

/** Decode the per-conversation choices; unknown fields fall back to defaults. */
export function decodePrefs(bytes: Uint8Array): ConversationPrefs {
  try {
    const o = JSON.parse(new TextDecoder().decode(bytes)) as Record<string, unknown>;
    return {
      autoTranslate: o.autoTranslate === true,
      languages: decodeLanguages(o.languages),
      transcriptionLanguage: typeof o.transcriptionLanguage === "string" ? o.transcriptionLanguage : null,
      dismissed: Array.isArray(o.dismissed) ? o.dismissed.filter((d): d is string => typeof d === "string") : [],
    };
  } catch {
    return DEFAULT_PREFS;
  }
}

function encode(value: unknown): Uint8Array {
  return new TextEncoder().encode(JSON.stringify(value));
}

/** The per-account store, with an in-memory cache Forget device drops. */
export class ResultStore {
  /** id -> decoded value (`null` = known absent). Keyed by account first. */
  private readonly cache = new Map<string, Map<string, StoredResult | ConversationPrefs | null>>();
  private readonly unregister: () => void;

  constructor(private readonly backend: ResultBackend) {
    this.unregister = registerAccountMemory("translation-results", (ids) => {
      for (const id of ids) this.cache.delete(id);
    });
  }

  dispose(): void {
    this.unregister();
  }

  private bucket(accountId: string): Map<string, StoredResult | ConversationPrefs | null> {
    let b = this.cache.get(accountId);
    if (b === undefined) {
      b = new Map();
      this.cache.set(accountId, b);
    }
    return b;
  }

  async result(
    accountId: string,
    slot: ResultSlot,
    conversation: string,
    messageId: number,
  ): Promise<StoredResult | null> {
    const id = await rowId(accountId, slot, conversation, messageId);
    const bucket = this.bucket(accountId);
    if (bucket.has(id)) return (bucket.get(id) as StoredResult | null) ?? null;
    let value: StoredResult | null = null;
    try {
      const bytes = await this.backend.get(accountId, id);
      value = bytes === null ? null : decodeResult(bytes);
    } catch {
      value = null;
    }
    bucket.set(id, value);
    return value;
  }

  async saveResult(
    accountId: string,
    slot: ResultSlot,
    conversation: string,
    messageId: number,
    value: StoredResult,
  ): Promise<void> {
    const id = await rowId(accountId, slot, conversation, messageId);
    this.bucket(accountId).set(id, value);
    await this.backend.put(accountId, id, "result", encode(value), conversation);
  }

  /** A message was deleted: every result describing it goes too. */
  async forgetMessage(accountId: string, conversation: string, messageId: number): Promise<void> {
    const slots: ResultSlot[] = ["text", "transcript", "transcript-translation"];
    const ids = await Promise.all(slots.map((s) => rowId(accountId, s, conversation, messageId)));
    const bucket = this.bucket(accountId);
    for (const id of ids) bucket.set(id, null);
    await this.backend.delete(accountId, ids);
  }

  async prefs(accountId: string, conversation: string): Promise<ConversationPrefs> {
    const id = await rowId(accountId, "prefs", conversation, 0);
    const bucket = this.bucket(accountId);
    const cached = bucket.get(id);
    if (cached !== undefined && cached !== null) return cached as ConversationPrefs;
    let value = DEFAULT_PREFS;
    try {
      const bytes = await this.backend.get(accountId, id);
      if (bytes !== null) value = decodePrefs(bytes);
    } catch {
      value = DEFAULT_PREFS;
    }
    bucket.set(id, value);
    return value;
  }

  async savePrefs(accountId: string, conversation: string, value: ConversationPrefs): Promise<void> {
    const id = await rowId(accountId, "prefs", conversation, 0);
    this.bucket(accountId).set(id, value);
    await this.backend.put(accountId, id, "prefs", encode(value), conversation);
  }

  /** A conversation was deleted from this device: its results and its
   *  choices go. The cache of the account is dropped whole (it is keyed by
   *  digest, so a conversation's entries cannot be picked out of it). */
  async forgetConversation(accountId: string, conversation: string): Promise<void> {
    this.cache.delete(accountId);
    await this.backend.deleteConversation(accountId, conversation);
  }

  /** How many translations and transcripts this device holds for the account. */
  count(accountId: string): Promise<number> {
    return this.backend.count(accountId, "result");
  }

  /** Settings "Delete all translations and transcripts". */
  async deleteAll(accountId: string): Promise<number> {
    const bucket = this.cache.get(accountId);
    if (bucket !== undefined) {
      for (const [id, v] of bucket) {
        if (v === null || "kind" in v) bucket.delete(id);
      }
    }
    return this.backend.deleteClass(accountId, "result");
  }
}

/** A backend in memory, for tests and for a browser with no IndexedDB. */
export class MemoryResultBackend implements ResultBackend {
  readonly rows = new Map<
    string,
    { accountId: string; cls: RowClass; plaintext: Uint8Array; conversation: string }
  >();

  get(_accountId: string, id: string): Promise<Uint8Array | null> {
    return Promise.resolve(this.rows.get(id)?.plaintext ?? null);
  }
  put(accountId: string, id: string, cls: RowClass, plaintext: Uint8Array, conversation: string): Promise<void> {
    this.rows.set(id, { accountId, cls, plaintext, conversation });
    return Promise.resolve();
  }
  deleteConversation(accountId: string, conversation: string): Promise<number> {
    let n = 0;
    for (const [id, r] of this.rows) {
      if (r.accountId === accountId && r.conversation === conversation) {
        this.rows.delete(id);
        n++;
      }
    }
    return Promise.resolve(n);
  }
  delete(_accountId: string, ids: readonly string[]): Promise<void> {
    for (const id of ids) this.rows.delete(id);
    return Promise.resolve();
  }
  deleteClass(accountId: string, cls: RowClass): Promise<number> {
    let n = 0;
    for (const [id, r] of this.rows) {
      if (r.accountId === accountId && r.cls === cls) {
        this.rows.delete(id);
        n++;
      }
    }
    return Promise.resolve(n);
  }
  count(accountId: string, cls: RowClass): Promise<number> {
    let n = 0;
    for (const r of this.rows.values()) if (r.accountId === accountId && r.cls === cls) n++;
    return Promise.resolve(n);
  }
}
