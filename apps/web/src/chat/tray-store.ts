// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `docs/ux/media-messages.md` section 11 — the composer tray of one
 * conversation: the photos, videos and files one message will carry, each
 * uploaded the moment it is added.
 *
 * A tray belongs to the conversation's draft on this device, so it lives here,
 * at module level and keyed by the conversation, rather than inside the chat
 * view: leaving the conversation and coming back finds it as it was (the
 * uploaded items kept, the ones still uploading carried on). It lasts as long
 * as the app runs; nothing of it is written to disk.
 *
 * The rules (shape of an item, the limits, when Send runs) are
 * `compose-tray.ts`'s; this module holds the items and runs their uploads.
 */
import { createSignal, type Accessor } from "solid-js";

import { AttachmentSizeError, type AttachmentRefBytes } from "./attachments";
import { uploadTrayPart, type TrayPart, type TrayPartMeta } from "./tray-upload";
import { isNetworkSendFailure } from "./waiting-sends";
import { onConnectionPhaseChange } from "../net/connection-state";
import { sendRefusalText } from "./attachment-failure";
import {
  fitIntoTray,
  TRAY_LIMIT_NOTICE_MS,
  trayKindOf,
  type TrayKind,
  type TrayLimitNotice,
  type TraySource,
  type TrayStatus,
} from "./compose-tray";
import { readAudioTags } from "../attach/audio-tags";
import { reencodePhoto } from "../attach/photo-encode";
import { t } from "../l10n";
import { describeImage, describeVideo } from "../media/visual-thumb";
import { loadWasm } from "../wasm";
import type { StorageClient } from "../storage/client";

/** One item in the tray. Replaced, never mutated, on every change. */
export interface TrayItem {
  readonly id: number;
  readonly kind: TrayKind;
  readonly source: TraySource;
  /** The bytes that are sent: a photo after its re-encode, else the original. */
  readonly file: File;
  readonly name: string;
  readonly size: number;
  readonly status: TrayStatus;
  /** Upload progress, 0..1, while `status` is `uploading`. */
  readonly progress: number;
  /** Object URL of the 64 px tile's picture (the thumb), or `null`. */
  readonly thumbUrl: string | null;
  /** Object URL of the whole photo or video, for the viewer, or `null`. */
  readonly fullUrl: string | null;
  /** The thumb as sent, so the sender's own bubble can show it at once. */
  readonly thumbBlob: Blob | null;
  /** A video's length, for the tile's duration pill. */
  readonly durationMs?: number;
  /** The uploaded reference, once `status` is `ready`. */
  readonly ref: AttachmentRefBytes | null;
}

/** What a tray needs from the conversation that shows it. */
export interface TrayDeps {
  /** Where the blobs go (a channel's files carry the channel file life). */
  readonly storage: () => StorageClient;
  /** The core's allowlist answer for a MIME type (`hub.inlineType`). */
  readonly inlineType: (mime: string | undefined) => "image" | "audio" | "video" | null;
  /** Delete blobs uploaded for items that will never be sent (ADR-0065
   *  section 6). */
  readonly discard: (refs: readonly AttachmentRefBytes[]) => void;
  /** Show a one-line refusal under the conversation (a file over the cap). */
  readonly onError: (text: string) => void;
}

export interface TrayStore {
  readonly items: Accessor<readonly TrayItem[]>;
  /** The limit line under the strip, for 4 s after a limit dropped items. */
  readonly notice: Accessor<TrayLimitNotice>;
  /** Add files: they take the tray shape `source` gives them, up to the
   *  limits, and start uploading at once. */
  add(files: readonly File[], source: TraySource): void;
  remove(id: number): void;
  removeLast(): void;
  retry(id: number): void;
  /** Hand every item to a send and empty the tray. The items' object URLs
   *  are released by {@link release} once the caller has filed their bytes. */
  take(): readonly TrayItem[];
  /**
   * `docs/ux/media-messages.md` section 11, Send — hand every item to a send
   * and empty the tray, including items whose upload has not finished (still
   * running, or with no answer). Such an item's upload becomes part of the
   * message's send: {@link TakenTrayItem.part} gives the prepared bytes, and
   * an upload the tray had running is adopted through
   * {@link adoptTrayUpload} rather than run twice.
   */
  takeForSend(): readonly TakenTrayItem[];
  /** Update the conversation-side callbacks (the view that shows the tray
   *  may have been mounted anew). */
  bind(deps: TrayDeps): void;
  /** Empty the tray for good: items released, nothing more uploaded, no
   *  callback called again. */
  drop(): void;
}

/** One item handed to a send. */
export interface TakenTrayItem {
  readonly item: TrayItem;
  /** The uploaded reference of an item that was ready, else `null`. */
  readonly ref: AttachmentRefBytes | null;
  /** An unfinished item: its prepared part (waits for the re-encode). */
  readonly part: (() => Promise<TrayPart>) | null;
  /** An unfinished item whose upload the tray had running: the id to adopt
   *  it by ({@link adoptTrayUpload}). */
  readonly adoptionId: number | null;
}

// ----- uploads handed over to a send -----------------------------------------

interface Adoption {
  readonly result: Promise<AttachmentRefBytes | null>;
  readonly resolve: (ref: AttachmentRefBytes | null) => void;
  settled: AttachmentRefBytes | null | undefined;
  abandoned: boolean;
  consumed: boolean;
  discard: (refs: readonly AttachmentRefBytes[]) => void;
}

const adoptions = new Map<number, Adoption>();

function openAdoption(id: number, discard: (refs: readonly AttachmentRefBytes[]) => void): void {
  let resolve: (ref: AttachmentRefBytes | null) => void = () => {};
  const result = new Promise<AttachmentRefBytes | null>((r) => {
    resolve = r;
  });
  adoptions.set(id, { result, resolve, settled: undefined, abandoned: false, consumed: false, discard });
}

function settleAdoption(id: number, ref: AttachmentRefBytes | null): void {
  const a = adoptions.get(id);
  if (a === undefined) {
    return;
  }
  a.settled = ref;
  a.resolve(ref);
  if (a.abandoned) {
    adoptions.delete(id);
    if (ref !== null) a.discard([ref]);
  } else if (a.consumed) {
    adoptions.delete(id);
  }
}

/**
 * The tray's upload of a taken item, for the send that took it: resolves with
 * the uploaded reference, or `null` when that upload failed (the send then
 * uploads from the prepared bytes). `null` itself when there is nothing to
 * adopt (no upload was running, or it was adopted already). One use.
 */
export function adoptTrayUpload(id: number): Promise<AttachmentRefBytes | null> | null {
  const a = adoptions.get(id);
  if (a === undefined || a.consumed || a.abandoned) return null;
  a.consumed = true;
  if (a.settled !== undefined) adoptions.delete(id);
  return a.result;
}

/** The message carrying a taken item was deleted: whatever its upload
 *  produces is deleted (ADR-0065 section 6). */
export function abandonTrayUpload(id: number): void {
  const a = adoptions.get(id);
  if (a === undefined || a.consumed) return;
  if (a.settled !== undefined) {
    adoptions.delete(id);
    if (a.settled !== null) a.discard([a.settled]);
    return;
  }
  a.abandoned = true;
}

const trays = new Map<string, TrayStore>();

/** The tray of one conversation, created on first use. `key` names the
 *  account and the conversation. */
export function trayFor(key: string, deps: TrayDeps): TrayStore {
  const existing = trays.get(key);
  if (existing !== undefined) {
    existing.bind(deps);
    return existing;
  }
  const store = createTrayStore(deps);
  trays.set(key, store);
  return store;
}

/** Drop every tray: their uploads are abandoned and their object URLs
 *  released. */
export function dropAllTrays(): void {
  for (const store of trays.values()) store.drop();
  trays.clear();
}

/**
 * Drop the trays of one account (Forget device, Delete account and every
 * other wipe, `auth/wipe-account.ts`): the files staged for its next
 * messages leave memory with the rest of its local data. An upload still
 * running finishes into nothing and asks for no delete: the account's stores
 * are sealed by then (ADR-0077), and its blobs expire on the server.
 */
export function dropTraysForAccount(accountIdB64: string): void {
  const prefix = `${accountIdB64}:`;
  for (const [key, store] of trays) {
    if (!key.startsWith(prefix)) continue;
    store.drop();
    trays.delete(key);
  }
}

/** The keys of the trays that hold an item now. Read-only; what the wipe's
 *  completeness test inventories beside the stores on disk. */
export function trayKeysHeld(): string[] {
  return [...trays].filter(([, store]) => store.items().length > 0).map(([key]) => key);
}

/** Release an item's object URLs. */
export function release(item: TrayItem): void {
  if (item.thumbUrl !== null) URL.revokeObjectURL(item.thumbUrl);
  if (item.fullUrl !== null) URL.revokeObjectURL(item.fullUrl);
}

/** Uploads running at once per tray. Chosen: enough to keep a link busy,
 *  few enough that ten large videos do not open ten uploads together. */
const PARALLEL_UPLOADS = 2;

let nextId = 1;

/** Exposed for tests: a tray with the given dependencies. */
export function createTrayStore(initial: TrayDeps): TrayStore {
  let deps = initial;
  const [items, setItems] = createSignal<readonly TrayItem[]>([]);
  const [notice, setNotice] = createSignal<TrayLimitNotice>(null);
  let noticeTimer: ReturnType<typeof setTimeout> | null = null;
  let dropped = false;
  const queue: number[] = [];
  let running = 0;
  /** Items handed to a send whose upload was running: they finish here. */
  const taken = new Map<number, TrayItem>();
  /** Items whose upload is running now. */
  const runningIds = new Set<number>();
  /** Preparations under way, so a send and the upload share one. */
  const preparing = new Map<number, Promise<TrayItem | undefined>>();

  const find = (id: number): TrayItem | undefined => items().find((i) => i.id === id) ?? taken.get(id);
  const patch = (id: number, change: Partial<TrayItem>): void => {
    const t = taken.get(id);
    if (t !== undefined) {
      taken.set(id, { ...t, ...change });
      return;
    }
    setItems(items().map((i) => (i.id === id ? { ...i, ...change } : i)));
  };

  /** The item once prepared (re-encode, thumb, tags); one preparation per
   *  item whoever asks. `undefined` when the item is gone. */
  function ensurePrepared(id: number): Promise<TrayItem | undefined> {
    const item = find(id);
    if (item === undefined) return Promise.resolve(undefined);
    if (item.status !== "preparing" || metaOf.has(id)) return Promise.resolve(item);
    const running = preparing.get(id);
    if (running !== undefined) return running;
    const p = (async (): Promise<TrayItem | undefined> => {
      const prepared = await prepare(item);
      if (find(id) === undefined) return undefined;
      patch(id, prepared);
      return find(id);
    })().finally(() => preparing.delete(id));
    preparing.set(id, p);
    return p;
  }

  /** The part an item uploads as. */
  async function partOf(item: TrayItem): Promise<TrayPart> {
    const meta = metaOf.get(item.id);
    const thumbBytes =
      item.thumbBlob !== null && meta?.thumbSize !== undefined
        ? new Uint8Array(await item.thumbBlob.arrayBuffer())
        : undefined;
    const cover =
      item.source === "audio" && meta?.audio?.cover !== undefined
        ? new Uint8Array(await meta.audio.cover.arrayBuffer())
        : undefined;
    const partMeta: TrayPartMeta = {
      source: item.source,
      ...(meta?.size === undefined ? {} : { size: meta.size }),
      ...(thumbBytes === undefined || meta?.thumbSize === undefined
        ? {}
        : { thumb: { bytes: thumbBytes, width: meta.thumbSize.width, height: meta.thumbSize.height } }),
      ...(meta?.inline === undefined ? {} : { inline: meta.inline }),
      ...(item.source === "audio" && meta?.audio !== undefined
        ? {
            audio: {
              title: meta.audio.title,
              ...(meta.audio.artist === undefined ? {} : { artist: meta.audio.artist }),
              ...(meta.audio.durationMs === undefined ? {} : { durationMs: meta.audio.durationMs }),
              ...(cover === undefined ? {} : { cover }),
            },
          }
        : {}),
    };
    return {
      bytes: new Uint8Array(await item.file.arrayBuffer()),
      mime: item.file.type.length > 0 ? item.file.type : undefined,
      name: item.file.name.length > 0 ? item.file.name : undefined,
      meta: partMeta,
    };
  }

  function showNotice(n: TrayLimitNotice): void {
    if (n === null) return;
    setNotice(n);
    if (noticeTimer !== null) clearTimeout(noticeTimer);
    noticeTimer = setTimeout(() => {
      noticeTimer = null;
      setNotice(null);
    }, TRAY_LIMIT_NOTICE_MS);
  }

  function pump(): void {
    while (running < PARALLEL_UPLOADS && queue.length > 0) {
      const id = queue.shift();
      if (id === undefined) break;
      if (find(id) === undefined) continue;
      running += 1;
      runningIds.add(id);
      void upload(id).finally(() => {
        running -= 1;
        runningIds.delete(id);
        pump();
      });
    }
  }

  async function upload(id: number): Promise<void> {
    let item = find(id);
    if (item === undefined) return;
    try {
      if (item.status === "preparing") {
        item = await ensurePrepared(id);
        if (item === undefined) return;
      }
      patch(id, { status: "uploading", progress: 0 });
      item = find(id);
      if (item === undefined) return;
      const part = await partOf(item);
      const ref = await uploadTrayPart(part, deps.storage(), (fraction) => {
        if (find(id) !== undefined) patch(id, { progress: Math.max(0, Math.min(1, fraction)) });
      });
      if (taken.has(id)) {
        // Handed to a send meanwhile: the send adopts it.
        taken.delete(id);
        settleAdoption(id, ref);
        return;
      }
      if (find(id) === undefined) {
        // Removed while uploading: the blob was never going to be sent.
        if (!dropped) deps.discard([ref]);
        return;
      }
      patch(id, { status: "ready", progress: 1, ref });
    } catch (err) {
      if (taken.has(id)) {
        taken.delete(id);
        settleAdoption(id, null);
        return;
      }
      if (find(id) === undefined) return;
      if (err instanceof AttachmentSizeError) {
        deps.onError(t("attach.preview.file_too_big"));
      } else if (sendRefusalText(err) !== null) {
        // UX 4.2 and docs/ux/storage-full.md.
        deps.onError(sendRefusalText(err) ?? "");
      } else {
        console.warn("[tray] upload failed", err instanceof Error ? err.name : "unknown");
      }
      // Section 11 — no answer does not stop Send; a refusal does.
      patch(id, { status: isNetworkSendFailure(err) ? "waiting" : "failed" });
    }
  }

  const metaOf = new Map<number, PreparedMeta>();

  async function prepare(item: TrayItem): Promise<Partial<TrayItem>> {
    const prepared = await prepareItem(item, deps.inlineType);
    metaOf.set(item.id, prepared.meta);
    return prepared.patch;
  }

  // docs/ux/connection-state.md section 7, as Android's tray does: an item
  // whose upload got no answer is uploaded again when the servers answer
  // again (the state entering Online), once per return. An item Send took is
  // no longer in the tray, so it is never uploaded a second time here.
  const stopOnline = onConnectionPhaseChange((now, before) => {
    if (dropped || now !== "online" || before === "online") return;
    for (const item of items()) if (item.status === "waiting") store.retry(item.id);
  });

  const store: TrayStore = {
    items,
    notice,
    add(files, source) {
      if (dropped) return;
      const shaped: { file: File; kind: TrayKind }[] = [];
      let tooBig = false;
      for (const file of files) {
        const kind = trayKindOf(file, source, deps.inlineType(file.type.length > 0 ? file.type : undefined));
        if (kind === "too-big") {
          tooBig = true;
          continue;
        }
        shaped.push({ file, kind });
      }
      if (tooBig) deps.onError(t("attach.preview.file_too_big"));
      const fit = fitIntoTray(
        items().map((i) => i.kind),
        shaped.map((s) => s.kind),
      );
      showNotice(fit.notice);
      const added: TrayItem[] = [];
      for (const index of fit.accepted) {
        const s = shaped[index];
        if (s === undefined) continue;
        const id = nextId++;
        added.push({
          id,
          kind: s.kind,
          source,
          file: s.file,
          name: s.file.name,
          size: s.file.size,
          status: "preparing",
          progress: 0,
          thumbUrl: null,
          fullUrl: null,
          thumbBlob: null,
          ref: null,
        });
        queue.push(id);
      }
      if (added.length === 0) return;
      setItems([...items(), ...added]);
      pump();
    },
    remove(id) {
      const item = find(id);
      if (item === undefined) return;
      setItems(items().filter((i) => i.id !== id));
      metaOf.delete(id);
      release(item);
      if (item.ref !== null) deps.discard([item.ref]);
    },
    removeLast() {
      const last = items().at(-1);
      if (last !== undefined) store.remove(last.id);
    },
    retry(id) {
      const item = items().find((i) => i.id === id);
      if (item === undefined || (item.status !== "failed" && item.status !== "waiting")) return;
      patch(id, { status: metaOf.has(id) ? "uploading" : "preparing", progress: 0 });
      queue.push(id);
      pump();
    },
    take() {
      const all = items();
      setItems([]);
      for (const i of all) metaOf.delete(i.id);
      return all;
    },
    takeForSend() {
      const all = items();
      setItems([]);
      const out: TakenTrayItem[] = [];
      for (const item of all) {
        if (item.status === "ready" && item.ref !== null) {
          metaOf.delete(item.id);
          out.push({ item, ref: item.ref, part: null, adoptionId: null });
          continue;
        }
        // Not started yet: the send uploads it, in the message's order.
        const queued = queue.indexOf(item.id);
        if (queued >= 0) queue.splice(queued, 1);
        const runningNow = runningIds.has(item.id);
        let adoptionId: number | null = null;
        if (runningNow) {
          taken.set(item.id, item);
          openAdoption(item.id, (refs) => deps.discard(refs));
          adoptionId = item.id;
        }
        const id = item.id;
        const snapshot = item;
        out.push({
          item,
          ref: null,
          adoptionId,
          part: async () => {
            if (!taken.has(id)) taken.set(id, snapshot);
            const prepared = (await ensurePrepared(id)) ?? find(id) ?? snapshot;
            const part = await partOf(prepared);
            if (!runningIds.has(id)) {
              // No upload of the tray is left running for it.
              taken.delete(id);
              metaOf.delete(id);
            }
            return part;
          },
        });
      }
      return out;
    },
    bind(next) {
      deps = next;
    },
    drop() {
      dropped = true;
      stopOnline();
      queue.length = 0;
      taken.clear();
      if (noticeTimer !== null) clearTimeout(noticeTimer);
      noticeTimer = null;
      setNotice(null);
      for (const item of store.take()) release(item);
    },
  };
  return store;
}

interface PreparedMeta {
  readonly size?: { readonly width: number; readonly height: number } | null;
  readonly thumbSize?: { readonly width: number; readonly height: number };
  /** ADR-0047 section 6 — the micro thumbnail, from the same decode. */
  readonly inline?: Uint8Array;
  readonly audio?: {
    readonly title: string;
    readonly artist?: string;
    readonly durationMs?: number;
    readonly cover?: Blob;
  };
}

/**
 * Read what the sender attaches before the upload: a photo re-encoded (longest
 * side at most the core's constant, metadata dropped; a GIF keeps its
 * animation), a photo's or video's size and thumb, an audio file's tags. A
 * failure here sends the item without the extras, never stops it (section 10:
 * "never block a send on a preview"); a photo that cannot be read at all goes
 * as a file chip (attachments section 3).
 */
async function prepareItem(
  item: TrayItem,
  inlineType: TrayDeps["inlineType"],
): Promise<{ patch: Partial<TrayItem>; meta: PreparedMeta }> {
  if (item.kind === "file") {
    if (item.source !== "audio") {
      // ADR-0047 section 6 — a file chip whose type shows inline as an image
      // or a video can be the message's carrier of the micro thumbnail (the
      // core picks the first such ref by type), so it gets one too; the chip
      // itself stays a chip.
      const inline = inlineType(item.file.type.length > 0 ? item.file.type : undefined);
      if (inline !== "image" && inline !== "video") return { patch: {}, meta: {} };
      try {
        const preview = inline === "image" ? await describeImage(item.file) : await describeVideo(item.file);
        return { patch: {}, meta: preview.inline === undefined ? {} : { inline: preview.inline } };
      } catch {
        return { patch: {}, meta: {} };
      }
    }
    try {
      const tags = await readAudioTags(item.file);
      return {
        patch: {},
        meta: {
          audio: {
            title: tags.title,
            ...(tags.artist === undefined ? {} : { artist: tags.artist }),
            ...(tags.durationMs === undefined ? {} : { durationMs: tags.durationMs }),
            ...(tags.cover === undefined ? {} : { cover: tags.cover }),
          },
        },
      };
    } catch {
      return { patch: {}, meta: {} };
    }
  }
  let file = item.file;
  if (item.kind === "photo" && file.type !== "image/gif") {
    try {
      const wasm = await loadWasm();
      file = await reencodePhoto(file, wasm.photoMaxLongestSidePx());
    } catch {
      // Not a picture this browser can read: it goes as the file it is.
      return { patch: { kind: "file" }, meta: {} };
    }
  }
  const preview = item.kind === "photo" ? await describeImage(file) : await describeVideo(file);
  const thumbUrl = preview.thumb === null ? null : URL.createObjectURL(preview.thumb.blob);
  const fullUrl = URL.createObjectURL(file);
  return {
    patch: {
      file,
      size: file.size,
      thumbUrl: thumbUrl ?? (item.kind === "photo" ? URL.createObjectURL(file) : null),
      fullUrl,
      thumbBlob: preview.thumb?.blob ?? null,
      ...(preview.durationMs === undefined ? {} : { durationMs: preview.durationMs }),
    },
    meta: {
      size: preview.size,
      ...(preview.thumb === null ? {} : { thumbSize: { width: preview.thumb.width, height: preview.thumb.height } }),
      ...(preview.inline === undefined ? {} : { inline: preview.inline }),
    },
  };
}
