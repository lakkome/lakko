// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The outbox: every unsent message of the signed-in account on this device
 * (`docs/ux/connection-state.md` section 7.3).
 *
 * An entry is one unsent row, waiting or `Not sent`, with what it takes to
 * send it again with no screen open: its conversation, the row's `tempId`
 * (which is also its place in the order), the state and the automatic-send
 * count, the text, reply and forward snapshots, a content message's object,
 * and for a photo, video, voice message, video note or GIF either the
 * uploaded attachment references or, before the upload finished, the
 * prepared file (`unsent-files.ts`) and what the upload needs besides it.
 *
 * The store is the session's single record: the chat view and the app-level
 * runner (`unsent-runner.ts`) both read and change it. Every change is
 * mirrored to the message log's `outbox` list under the device key, and a
 * prepared file to its own record; a failed write never stops a send (the
 * entry then lives as long as the page, as before section 7.3).
 *
 * In memory a row whose send is running is `pending`: the pass never picks
 * it, which is what keeps a message from being sent twice from this device.
 * Stored, `pending` reads back as `waiting`: a page that went away mid-send
 * does not know whether it arrived, and section 7.2 already states the cost
 * of sending again.
 */

import { registerAccountMemory } from "../auth/account-memory";
import { getDeviceKey } from "../auth/key-store";
import type { DeviceCipher } from "../auth/device-cipher";
import type {
  AttachmentMediaMeta,
  AttachmentRefBytes,
  ForwardRef,
  ReplyRef,
  ThumbRefBytes,
} from "./attachments";
import { forwardRefFromPersisted, persistedFromForwardRef } from "./forward";
import {
  loadMessageLog,
  setOutboxInMessageLog,
  type PersistedAttachment,
  type PersistedForward,
  type PersistedReply,
} from "./message-log-store";
import {
  attachmentRefFromPersisted,
  b64UrlDecode,
  b64UrlEncode,
  persistedFromAttachmentRef,
  thumbFromPersisted,
} from "./persisted-attachment";
import type { TraySource } from "./compose-tray";
import type { TrayPart } from "./tray-upload";
import { persistedFromReplyRef, replyRefFromPersisted } from "./reply-snapshot";
import {
  deleteUnsentFile,
  getUnsentFile,
  putUnsentFile,
  type PreparedFile,
  type PreparedPartBytes,
} from "./unsent-files";
import { contentFromPersisted } from "../attach/preview";
import type { MessageContent } from "../../wasm/lakko_ffi_wasm";

export type { PreparedFile, PreparedPartBytes } from "./unsent-files";

/** An entry's state. `pending`: a send of it is running (memory only). */
export type UnsentState = "pending" | "waiting" | "failed";

/** A media message whose upload has not finished. */
export interface UnsentMedia {
  readonly name: string;
  readonly mime: string;
  /** The media metadata the upload carries (kind, duration, waveform, size). */
  readonly meta: AttachmentMediaMeta;
  /** A GIF whose full rendition was not fetched yet: the proxy path. */
  readonly gifUrl?: string;
  /** A video note's poster frame, when its bytes are in the prepared file. */
  readonly poster?: { readonly mime: string; readonly width: number; readonly height: number };
  /** The poster, once uploaded. */
  readonly thumb?: ThumbRefBytes;
}

/**
 * One item of a composer-tray message whose upload had not finished when Send
 * was pressed (`docs/ux/media-messages.md` section 11). Its bytes (and thumb
 * and cover) are in the prepared file, in the same slot.
 */
export interface UnsentUploadPart {
  readonly kind: "upload";
  /** A stand-in blob id: the row draws the item and the media hub holds its
   *  local bytes under it until the upload lands. */
  readonly localId: Uint8Array;
  readonly name: string | undefined;
  readonly mime: string | undefined;
  readonly size: number;
  readonly source: TraySource;
  readonly dimensions?: { readonly width: number; readonly height: number } | null;
  /** The thumb's size; its bytes are in the prepared file. */
  readonly thumbSize?: { readonly width: number; readonly height: number };
  /** ADR-0047 section 6 — the micro thumbnail, on the message's carrier only. */
  readonly inline?: Uint8Array;
  readonly audio?: { readonly title: string; readonly artist?: string; readonly durationMs?: number };
  /** This page only, never stored: the tray upload that was running for it,
   *  adopted rather than run twice (`tray-store.ts` `adoptTrayUpload`). */
  readonly trayItemId?: number;
}

/** One item of a tray message: uploaded, or still to upload. */
export type UnsentPart = { readonly kind: "ref"; readonly ref: AttachmentRefBytes } | UnsentUploadPart;

/** The attachments a tray message is drawn with before it is sent: the
 *  uploaded references, and a stand-in for each item still to upload. */
export function attachmentsOfParts(parts: readonly UnsentPart[]): AttachmentRefBytes[] {
  return parts.map((p) =>
    p.kind === "ref"
      ? p.ref
      : {
          blobId: p.localId,
          key: new Uint8Array(0),
          nonce: new Uint8Array(0),
          mime: p.mime,
          size: p.size,
          name: p.name,
          ...(p.dimensions === undefined || p.dimensions === null
            ? {}
            : { width: p.dimensions.width, height: p.dimensions.height }),
          ...(p.source === "audio" && p.audio !== undefined ? { kind: "audio", title: p.audio.title } : {}),
        },
  );
}

/** The upload of one stored part, from its prepared bytes. */
export function trayPartOf(part: UnsentUploadPart, prepared: PreparedPartBytes): TrayPart {
  return {
    bytes: prepared.bytes,
    mime: part.mime,
    name: part.name,
    meta: {
      source: part.source,
      ...(part.dimensions === undefined ? {} : { size: part.dimensions }),
      ...(part.thumbSize === undefined || prepared.thumb === undefined
        ? {}
        : { thumb: { bytes: prepared.thumb, width: part.thumbSize.width, height: part.thumbSize.height } }),
      ...(part.inline === undefined ? {} : { inline: part.inline }),
      ...(part.audio === undefined
        ? {}
        : {
            audio: {
              ...part.audio,
              ...(prepared.cover === undefined ? {} : { cover: prepared.cover }),
            },
          }),
    },
  };
}

/** One unsent message. */
export interface UnsentEntry {
  readonly groupIdB64: string;
  /** The row's client id: unique on this device, growing with send order. */
  readonly tempId: number;
  readonly writtenAtMs: number;
  readonly state: UnsentState;
  /** Automatic sends spent (section 7.2, at most 5). */
  readonly autoSends: number;
  readonly text: string;
  /** Finished attachment references (a media message's, once uploaded). */
  readonly attachments: readonly AttachmentRefBytes[];
  readonly reply?: ReplyRef;
  readonly forward?: ForwardRef;
  readonly content?: MessageContent;
  /** Present while a media message's upload has not finished. */
  readonly media?: UnsentMedia;
  /** Present while a composer-tray message has an item still to upload;
   *  `attachments` is then empty and these are the message's items, in
   *  order. */
  readonly parts?: readonly UnsentPart[];
}

/** Whether an entry has bytes in the prepared-file store. */
export function hasPreparedFile(e: UnsentEntry): boolean {
  return e.media !== undefined || (e.parts?.some((p) => p.kind === "upload") ?? false);
}

/** What an entry is stored as in the message log. */
export interface PersistedUnsent {
  readonly groupIdB64: string;
  /** Stringified: the ids are above 2^32 (see {@link allocateUnsentTempId}). */
  readonly tempId: string;
  readonly writtenAtMs: number;
  readonly state: "waiting" | "failed";
  readonly autoSends: number;
  readonly text: string;
  readonly attachments: readonly PersistedAttachment[];
  readonly reply?: PersistedReply;
  readonly forward?: PersistedForward;
  readonly content?: unknown;
  readonly parts?: readonly PersistedPart[];
  readonly media?: {
    /** A placeholder attachment carrying name, type and the media metadata. */
    readonly ref: PersistedAttachment;
    readonly gifUrl?: string;
    readonly poster?: { readonly mime: string; readonly width: number; readonly height: number };
  };
}

/** A stored tray part. */
type PersistedPart =
  | { readonly kind: "ref"; readonly ref: PersistedAttachment }
  | {
      readonly kind: "upload";
      readonly localIdB64: string;
      readonly name: string | null;
      readonly mime: string | null;
      readonly size: number;
      readonly source: TraySource;
      readonly dimensions?: { readonly width: number; readonly height: number } | null;
      readonly thumbSize?: { readonly width: number; readonly height: number };
      readonly inlineB64?: string;
      readonly audio?: { readonly title: string; readonly artist?: string; readonly durationMs?: number };
    };

function persistedPart(p: UnsentPart): PersistedPart {
  if (p.kind === "ref") return { kind: "ref", ref: persistedFromAttachmentRef(p.ref) };
  return {
    kind: "upload",
    localIdB64: b64UrlEncode(p.localId),
    name: p.name ?? null,
    mime: p.mime ?? null,
    size: p.size,
    source: p.source,
    ...(p.dimensions === undefined ? {} : { dimensions: p.dimensions }),
    ...(p.thumbSize === undefined ? {} : { thumbSize: p.thumbSize }),
    ...(p.inline === undefined ? {} : { inlineB64: b64UrlEncode(p.inline) }),
    ...(p.audio === undefined ? {} : { audio: p.audio }),
  };
}

function isWh(v: unknown): v is { width: number; height: number } {
  return (
    typeof v === "object" &&
    v !== null &&
    typeof (v as { width?: unknown }).width === "number" &&
    typeof (v as { height?: unknown }).height === "number"
  );
}

function partFromPersisted(raw: unknown): UnsentPart | null {
  if (typeof raw !== "object" || raw === null) return null;
  const r = raw as Record<string, unknown>;
  if (r["kind"] === "ref") {
    try {
      return { kind: "ref", ref: attachmentRefFromPersisted(r["ref"] as PersistedAttachment).ref };
    } catch {
      return null;
    }
  }
  if (r["kind"] !== "upload") return null;
  let localId: Uint8Array;
  try {
    localId = b64UrlDecode(String(r["localIdB64"]));
  } catch {
    return null;
  }
  const source = r["source"];
  if (localId.length !== 32 || (source !== "media" && source !== "file" && source !== "audio")) return null;
  const size = r["size"];
  if (typeof size !== "number" || !Number.isInteger(size) || size < 0) return null;
  let inline: Uint8Array | undefined;
  if (typeof r["inlineB64"] === "string") {
    try {
      inline = b64UrlDecode(r["inlineB64"]);
    } catch {
      inline = undefined;
    }
  }
  const audioRaw = r["audio"] as Record<string, unknown> | undefined;
  const audio =
    typeof audioRaw === "object" && audioRaw !== null && typeof audioRaw["title"] === "string"
      ? {
          title: audioRaw["title"],
          ...(typeof audioRaw["artist"] === "string" ? { artist: audioRaw["artist"] } : {}),
          ...(typeof audioRaw["durationMs"] === "number" ? { durationMs: audioRaw["durationMs"] } : {}),
        }
      : undefined;
  return {
    kind: "upload",
    localId,
    name: typeof r["name"] === "string" ? r["name"] : undefined,
    mime: typeof r["mime"] === "string" ? r["mime"] : undefined,
    size,
    source,
    ...(r["dimensions"] === null ? { dimensions: null } : isWh(r["dimensions"]) ? { dimensions: r["dimensions"] } : {}),
    ...(isWh(r["thumbSize"]) ? { thumbSize: r["thumbSize"] } : {}),
    ...(inline === undefined ? {} : { inline }),
    ...(audio === undefined ? {} : { audio }),
  };
}

// ----- the row ids ---------------------------------------------------------

let lastTempId = 0;

/**
 * A new row id for an outgoing message: unique on this device for the
 * account's whole outbox and growing with send order, across mounts of a
 * chat view and across reloads. Microseconds of the clock, bumped past the
 * last id handed out or read back, so a clock that went backwards still
 * orders a new message after every unsent one. Below 2^53 until the year
 * 2255.
 */
export function allocateUnsentTempId(nowMs: number = Date.now()): number {
  lastTempId = Math.max(lastTempId + 1, Math.floor(nowMs) * 1000);
  return lastTempId;
}

/** Never hand out an id at or below `id` (an entry read back). */
export function noteUnsentTempId(id: number): void {
  if (Number.isSafeInteger(id) && id > lastTempId) lastTempId = id;
}

// ----- the stored form -----------------------------------------------------

/** A placeholder attachment: name, type and media metadata, no blob. */
export function placeholderAttachment(media: UnsentMedia, size: number): AttachmentRefBytes {
  return {
    blobId: new Uint8Array(0),
    key: new Uint8Array(0),
    nonce: new Uint8Array(0),
    mime: media.mime,
    size,
    name: media.name,
    ...media.meta,
    ...(media.thumb === undefined ? {} : { thumb: media.thumb }),
  };
}

/** The stored form of an entry. Pure. */
export function unsentToPersisted(e: UnsentEntry): PersistedUnsent {
  return {
    groupIdB64: e.groupIdB64,
    tempId: String(e.tempId),
    writtenAtMs: e.writtenAtMs,
    state: e.state === "failed" ? "failed" : "waiting",
    autoSends: e.autoSends,
    text: e.text,
    attachments: e.attachments.map((a) => persistedFromAttachmentRef(a)),
    ...(e.reply === undefined ? {} : { reply: persistedFromReplyRef(e.reply) }),
    ...(e.forward === undefined ? {} : { forward: persistedFromForwardRef(e.forward) }),
    ...(e.content === undefined ? {} : { content: e.content }),
    ...(e.parts === undefined ? {} : { parts: e.parts.map(persistedPart) }),
    ...(e.media === undefined
      ? {}
      : {
          media: {
            ref: persistedFromAttachmentRef(placeholderAttachment(e.media, 0)),
            ...(e.media.gifUrl === undefined ? {} : { gifUrl: e.media.gifUrl }),
            ...(e.media.poster === undefined ? {} : { poster: e.media.poster }),
          },
        }),
  };
}

/** The entry a stored row reads back as, or `null` for anything that is not
 *  one (a store is not a wire: junk is dropped, never sent). Pure. */
export function unsentFromPersisted(row: unknown): UnsentEntry | null {
  if (typeof row !== "object" || row === null) return null;
  const r = row as Partial<PersistedUnsent>;
  if (typeof r.groupIdB64 !== "string" || r.groupIdB64.length === 0) return null;
  const tempId = Number(r.tempId);
  if (!Number.isSafeInteger(tempId) || tempId <= 0) return null;
  if (typeof r.writtenAtMs !== "number" || !Number.isFinite(r.writtenAtMs)) return null;
  if (r.state !== "waiting" && r.state !== "failed") return null;
  const autoSends =
    typeof r.autoSends === "number" && Number.isInteger(r.autoSends) && r.autoSends >= 0 ? r.autoSends : 0;
  if (typeof r.text !== "string" || !Array.isArray(r.attachments)) return null;
  let attachments: AttachmentRefBytes[];
  try {
    attachments = r.attachments.map((a) => attachmentRefFromPersisted(a).ref);
  } catch {
    return null;
  }
  let media: UnsentMedia | undefined;
  if (r.media !== undefined) {
    const m = r.media as Partial<NonNullable<PersistedUnsent["media"]>>;
    if (typeof m.ref !== "object" || m.ref === null) return null;
    let ref: AttachmentRefBytes;
    try {
      ref = attachmentRefFromPersisted(m.ref).ref;
    } catch {
      return null;
    }
    const thumb = thumbFromPersisted((m.ref as PersistedAttachment).thumb);
    const poster =
      typeof m.poster === "object" &&
      m.poster !== null &&
      typeof m.poster.mime === "string" &&
      typeof m.poster.width === "number" &&
      typeof m.poster.height === "number"
        ? { mime: m.poster.mime, width: m.poster.width, height: m.poster.height }
        : undefined;
    media = {
      name: ref.name ?? "file",
      mime: ref.mime ?? "application/octet-stream",
      meta: {
        ...(ref.kind === undefined ? {} : { kind: ref.kind }),
        ...(ref.durationMs === undefined ? {} : { durationMs: ref.durationMs }),
        ...(ref.waveform === undefined ? {} : { waveform: ref.waveform }),
        ...(ref.width === undefined ? {} : { width: ref.width }),
        ...(ref.height === undefined ? {} : { height: ref.height }),
      },
      ...(typeof m.gifUrl === "string" && m.gifUrl.length > 0 ? { gifUrl: m.gifUrl } : {}),
      ...(poster === undefined ? {} : { poster }),
      ...(thumb === undefined ? {} : { thumb }),
    };
  }
  let parts: UnsentPart[] | undefined;
  if (Array.isArray(r.parts)) {
    parts = [];
    for (const raw of r.parts) {
      const part = partFromPersisted(raw);
      if (part === null) return null;
      parts.push(part);
    }
    if (parts.length === 0) parts = undefined;
  }
  const content = contentFromPersisted(r.content);
  if (
    r.text.length === 0 &&
    attachments.length === 0 &&
    content === undefined &&
    media === undefined &&
    parts === undefined
  ) {
    return null;
  }
  const reply = replyRefFromPersisted(r.reply);
  const forward = forwardRefFromPersisted(r.forward);
  return {
    groupIdB64: r.groupIdB64,
    tempId,
    writtenAtMs: r.writtenAtMs,
    state: r.state,
    autoSends,
    text: r.text,
    attachments,
    ...(reply === undefined ? {} : { reply }),
    ...(forward === undefined ? {} : { forward }),
    ...(content === undefined ? {} : { content }),
    ...(media === undefined ? {} : { media }),
    ...(parts === undefined ? {} : { parts }),
  };
}

// ----- persistence ---------------------------------------------------------

/** Where the outbox is kept. The default is the message log and
 *  `unsent-files.ts`, under the account's device key. */
export interface UnsentPersistence {
  save(accountIdB64: string, rows: readonly PersistedUnsent[]): Promise<void>;
  load(accountIdB64: string): Promise<readonly unknown[]>;
  putFile(accountIdB64: string, groupIdB64: string, tempId: number, file: PreparedFile): Promise<void>;
  getFile(accountIdB64: string, groupIdB64: string, tempId: number): Promise<PreparedFile | null>;
  deleteFile(accountIdB64: string, groupIdB64: string, tempId: number): Promise<void>;
}

/** The message log + file store, the device key read per call through
 *  `deviceKeyOf` (an absent key writes nothing and reads nothing). */
export function deviceStorePersistence(
  deviceKeyOf: (accountIdB64: string) => Promise<DeviceCipher | null> = getDeviceKey,
): UnsentPersistence {
  return {
    async save(account, rows) {
      const key = await deviceKeyOf(account);
      if (key === null) throw new Error("no device key");
      await setOutboxInMessageLog(account, key, rows);
    },
    async load(account) {
      const key = await deviceKeyOf(account);
      if (key === null) return [];
      const log = await loadMessageLog(account, key);
      return log?.outbox ?? [];
    },
    async putFile(account, groupIdB64, tempId, file) {
      const key = await deviceKeyOf(account);
      if (key === null) throw new Error("no device key");
      await putUnsentFile(account, key, groupIdB64, tempId, file);
    },
    async getFile(account, groupIdB64, tempId) {
      const key = await deviceKeyOf(account);
      if (key === null) return null;
      return getUnsentFile(account, key, groupIdB64, tempId);
    },
    deleteFile: (account, groupIdB64, tempId) => deleteUnsentFile(account, groupIdB64, tempId),
  };
}

// ----- the store -----------------------------------------------------------

/** A change of an entry. `endMedia`: the upload finished, so the entry's
 *  media fields and its prepared file go. */
export interface UnsentPatch {
  readonly state?: UnsentState;
  readonly autoSends?: number;
  readonly attachments?: readonly AttachmentRefBytes[];
  readonly media?: UnsentMedia;
  readonly endMedia?: boolean;
  readonly parts?: readonly UnsentPart[];
  /** Every tray item is uploaded: `parts` and the prepared file go. */
  readonly endParts?: boolean;
}

/** A change of the store, for a chat view mirroring its rows. */
export interface UnsentEvent {
  readonly kind: "put" | "update" | "remove";
  readonly groupIdB64: string;
  readonly tempId: number;
}

function keyOf(groupIdB64: string, tempId: number): string {
  return `${groupIdB64}|${tempId}`;
}

function logFailure(what: string, err: unknown): void {
  // eslint-disable-next-line no-console
  console.warn(`[outbox] ${what}`, err instanceof Error ? err.name : "");
}

/** The account's outbox on this device. */
export class UnsentOutbox {
  private account: string | null = null;
  private restoredFor: string | null = null;
  private restoring: Promise<void> | null = null;
  private readonly entriesByKey = new Map<string, UnsentEntry>();
  /** Prepared files of this session, so a send needs no read of the store
   *  (and works when its write failed). */
  private readonly files = new Map<string, PreparedFile>();
  private readonly listeners = new Set<(e: UnsentEvent) => void>();
  private writeChain: Promise<void> = Promise.resolve();

  constructor(private readonly persistence: UnsentPersistence = deviceStorePersistence()) {}

  /** The account the store is for; another account empties it. */
  bind(accountIdB64: string): void {
    if (this.account === accountIdB64) return;
    this.clearMemory();
    this.account = accountIdB64;
  }

  /** The bound account, or `null`. */
  get accountIdB64(): string | null {
    return this.account;
  }

  /**
   * Read the stored entries back (the session landing: a page load, a
   * restart, a sign-in). Once per account; entries this page already holds
   * win over what is on disk. Never throws.
   */
  restore(accountIdB64: string): Promise<void> {
    this.bind(accountIdB64);
    if (this.restoredFor === accountIdB64 && this.restoring !== null) return this.restoring;
    this.restoredFor = accountIdB64;
    this.restoring = (async (): Promise<void> => {
      try {
        const rows = await this.persistence.load(accountIdB64);
        if (this.account !== accountIdB64) return;
        for (const row of rows) {
          const e = unsentFromPersisted(row);
          if (e === null) continue;
          noteUnsentTempId(e.tempId);
          const k = keyOf(e.groupIdB64, e.tempId);
          if (this.entriesByKey.has(k)) continue;
          this.entriesByKey.set(k, e);
          this.emit({ kind: "put", groupIdB64: e.groupIdB64, tempId: e.tempId });
        }
      } catch (err) {
        logFailure("the outbox was not read back", err);
      }
    })();
    return this.restoring;
  }

  /** Whether the stored entries were read back for `accountIdB64`. */
  restoredPromise(accountIdB64: string): Promise<void> | null {
    return this.restoredFor === accountIdB64 ? this.restoring : null;
  }

  has(groupIdB64: string, tempId: number): boolean {
    return this.entriesByKey.has(keyOf(groupIdB64, tempId));
  }

  get(groupIdB64: string, tempId: number): UnsentEntry | undefined {
    return this.entriesByKey.get(keyOf(groupIdB64, tempId));
  }

  /** A conversation's entries, oldest first. */
  entries(groupIdB64: string): UnsentEntry[] {
    return [...this.entriesByKey.values()]
      .filter((e) => e.groupIdB64 === groupIdB64)
      .sort((a, b) => a.tempId - b.tempId);
  }

  /** A conversation's waiting entries' ids, oldest first. */
  waitingIds(groupIdB64: string): number[] {
    return this.entries(groupIdB64)
      .filter((e) => e.state === "waiting")
      .map((e) => e.tempId);
  }

  /** Every conversation with a waiting entry. */
  groupsWithWaiting(): string[] {
    const out = new Set<string>();
    for (const e of this.entriesByKey.values()) if (e.state === "waiting") out.add(e.groupIdB64);
    return [...out];
  }

  /** Every entry, oldest first. */
  all(): UnsentEntry[] {
    return [...this.entriesByKey.values()].sort((a, b) => a.tempId - b.tempId);
  }

  /**
   * Add (or replace) an entry, with its prepared file when its upload has not
   * finished. Resolves once the write was tried; never rejects.
   */
  async put(entry: UnsentEntry, file?: PreparedFile): Promise<void> {
    const k = keyOf(entry.groupIdB64, entry.tempId);
    noteUnsentTempId(entry.tempId);
    this.entriesByKey.set(k, entry);
    if (file !== undefined) this.files.set(k, file);
    this.emit({ kind: "put", groupIdB64: entry.groupIdB64, tempId: entry.tempId });
    const account = this.account;
    if (account === null) return;
    if (file !== undefined && hasPreparedFile(entry)) {
      try {
        await this.persistence.putFile(account, entry.groupIdB64, entry.tempId, file);
      } catch (err) {
        logFailure("a prepared file was not stored", err);
      }
    }
    await this.persist();
  }

  /**
   * Change an entry. A patch that ends its media (the upload finished)
   * deletes its prepared file. No-op for an entry the store does not hold.
   */
  async update(groupIdB64: string, tempId: number, patch: UnsentPatch): Promise<void> {
    const k = keyOf(groupIdB64, tempId);
    const before = this.entriesByKey.get(k);
    if (before === undefined) return;
    const { endMedia, endParts, ...fields } = patch;
    const next: UnsentEntry = { ...before, ...fields };
    const mediaEnded = before.media !== undefined && endMedia === true;
    const partsEnded = before.parts !== undefined && endParts === true;
    let clean: UnsentEntry = mediaEnded ? withoutMedia(next) : next;
    if (partsEnded) clean = withoutParts(clean);
    const fileEnded = hasPreparedFile(before) && !hasPreparedFile(clean);
    this.entriesByKey.set(k, clean);
    this.emit({ kind: "update", groupIdB64, tempId });
    // A flip to `pending` and back changes nothing on disk.
    const stored = JSON.stringify(unsentToPersisted(before));
    if (fileEnded) {
      this.files.delete(k);
      await this.deleteFile(groupIdB64, tempId);
    }
    if (stored !== JSON.stringify(unsentToPersisted(clean))) await this.persist();
  }

  /** One tray item of an entry landed: its slot holds the reference. */
  async partUploaded(groupIdB64: string, tempId: number, index: number, ref: AttachmentRefBytes): Promise<void> {
    const e = this.get(groupIdB64, tempId);
    if (e?.parts === undefined) return;
    const parts: UnsentPart[] = e.parts.map((p, i) => (i === index ? { kind: "ref", ref } : p));
    await this.update(groupIdB64, tempId, { parts });
  }

  /** Every tray item landed: the entry carries the references. */
  async partsDone(groupIdB64: string, tempId: number, attachments: readonly AttachmentRefBytes[]): Promise<void> {
    await this.update(groupIdB64, tempId, { attachments, endParts: true });
  }

  /** Drop an entry and its prepared file (sent, deleted, or its
   *  conversation is gone). Never rejects. */
  async remove(groupIdB64: string, tempId: number): Promise<void> {
    const k = keyOf(groupIdB64, tempId);
    const before = this.entriesByKey.get(k);
    if (before === undefined) return;
    this.entriesByKey.delete(k);
    this.files.delete(k);
    this.emit({ kind: "remove", groupIdB64, tempId });
    if (hasPreparedFile(before)) await this.deleteFile(groupIdB64, tempId);
    await this.persist();
  }

  /** Drop every entry of a conversation this device no longer holds. */
  async removeGroup(groupIdB64: string): Promise<void> {
    for (const e of this.entries(groupIdB64)) await this.remove(groupIdB64, e.tempId);
  }

  /** An entry's prepared file: this session's copy, else the stored one. */
  async preparedFile(groupIdB64: string, tempId: number): Promise<PreparedFile | null> {
    const k = keyOf(groupIdB64, tempId);
    const held = this.files.get(k);
    if (held !== undefined) return held;
    const account = this.account;
    if (account === null) return null;
    try {
      const read = await this.persistence.getFile(account, groupIdB64, tempId);
      if (read !== null && this.entriesByKey.has(k)) this.files.set(k, read);
      return read;
    } catch (err) {
      logFailure("a prepared file was not read", err);
      return null;
    }
  }

  /** Hear every change. */
  subscribe(listener: (e: UnsentEvent) => void): () => void {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  }

  /** Resolves once every write started so far has finished (tests). */
  flushed(): Promise<void> {
    return this.writeChain;
  }

  /** Sign-out, an account switch or the wipe: nothing of this account's
   *  outbox stays in memory (the stored rows are the account's own). */
  reset(): void {
    this.clearMemory();
    this.account = null;
  }

  private clearMemory(): void {
    this.entriesByKey.clear();
    this.files.clear();
    this.restoredFor = null;
    this.restoring = null;
  }

  private emit(e: UnsentEvent): void {
    for (const l of [...this.listeners]) {
      try {
        l(e);
      } catch {
        /* a listener's failure is its own */
      }
    }
  }

  private async deleteFile(groupIdB64: string, tempId: number): Promise<void> {
    const account = this.account;
    if (account === null) return;
    try {
      await this.persistence.deleteFile(account, groupIdB64, tempId);
    } catch (err) {
      logFailure("a prepared file was not deleted", err);
    }
  }

  /** Mirror every entry to the log, one write after the other, each writing
   *  the state at its turn. */
  private persist(): Promise<void> {
    const account = this.account;
    if (account === null) return Promise.resolve();
    const run = this.writeChain.then(async () => {
      if (this.account !== account) return;
      try {
        await this.persistence.save(account, this.all().map(unsentToPersisted));
      } catch (err) {
        logFailure("the outbox was not stored", err);
      }
    });
    this.writeChain = run;
    return run;
  }
}

function withoutParts(e: UnsentEntry): UnsentEntry {
  const { parts: _parts, ...rest } = e;
  return rest;
}

function withoutMedia(e: UnsentEntry): UnsentEntry {
  const { media: _media, ...rest } = e;
  return rest;
}

/** The page's outbox (one per tab, bound to the signed-in account). */
export const unsentOutbox = new UnsentOutbox();

// Forget device: the store holds the wiped account's unsent messages, and a
// write after the wipe would put them back. Sign-out drops them too: a later
// sign-in reads them back from the account's own log.
registerAccountMemory(
  "unsent-outbox",
  (ids) => {
    const account = unsentOutbox.accountIdB64;
    if (account === null || ids.includes(account)) unsentOutbox.reset();
  },
  { signOut: true },
);
