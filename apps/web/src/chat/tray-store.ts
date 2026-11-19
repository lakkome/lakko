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

import {
  AttachmentSizeError,
  encryptAndUploadBytes,
  type AttachmentRefBytes,
  type ThumbRefBytes,
} from "./attachments";
import { isSendLimit, sendLimitText } from "./attachment-failure";
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
  /** Update the conversation-side callbacks (the view that shows the tray
   *  may have been mounted anew). */
  bind(deps: TrayDeps): void;
  /** Empty the tray for good: items released, nothing more uploaded, no
   *  callback called again. */
  drop(): void;
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

  const find = (id: number): TrayItem | undefined => items().find((i) => i.id === id);
  const patch = (id: number, change: Partial<TrayItem>): void => {
    setItems(items().map((i) => (i.id === id ? { ...i, ...change } : i)));
  };

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
      void upload(id).finally(() => {
        running -= 1;
        pump();
      });
    }
  }

  async function upload(id: number): Promise<void> {
    let item = find(id);
    if (item === undefined) return;
    try {
      if (item.status === "preparing") {
        const prepared = await prepare(item);
        item = find(id);
        if (item === undefined) return;
        patch(id, { ...prepared, status: "uploading", progress: 0 });
        item = find(id);
        if (item === undefined) return;
      } else {
        patch(id, { status: "uploading", progress: 0 });
      }
      const storage = deps.storage();
      const bytes = new Uint8Array(await item.file.arrayBuffer());
      let ref = await encryptAndUploadBytes(
        bytes,
        {
          mime: item.file.type.length > 0 ? item.file.type : undefined,
          name: item.file.name.length > 0 ? item.file.name : undefined,
        },
        {},
        storage,
        (fraction) => {
          if (find(id) !== undefined) patch(id, { progress: Math.max(0, Math.min(1, fraction)) });
        },
      );
      const current = find(id);
      if (current === undefined) {
        // Removed while uploading: the blob was never going to be sent.
        if (!dropped) deps.discard([ref]);
        return;
      }
      ref = await withMeta(ref, current, storage);
      if (find(id) === undefined) {
        if (!dropped) deps.discard([ref]);
        return;
      }
      patch(id, { status: "ready", progress: 1, ref });
    } catch (err) {
      if (find(id) === undefined) return;
      if (err instanceof AttachmentSizeError) {
        deps.onError(t("attach.preview.file_too_big"));
      } else if (isSendLimit(err)) {
        deps.onError(sendLimitText());
      } else {
        console.warn("[tray] upload failed", err instanceof Error ? err.name : "unknown");
      }
      patch(id, { status: "failed" });
    }
  }

  /** The thumb, dimensions and tags the sender attaches (section 10), the
   *  thumb uploaded as its own blob. A failed thumb sends without one. */
  async function withMeta(ref: AttachmentRefBytes, item: TrayItem, storage: StorageClient): Promise<AttachmentRefBytes> {
    const meta = metaOf.get(item.id);
    let out: AttachmentRefBytes = ref;
    if (meta?.size !== undefined && meta.size !== null) {
      out = { ...out, width: meta.size.width, height: meta.size.height };
    }
    if (item.thumbBlob !== null && meta?.thumbSize !== undefined) {
      try {
        const bytes = new Uint8Array(await item.thumbBlob.arrayBuffer());
        const up = await encryptAndUploadBytes(bytes, { mime: "image/jpeg", name: undefined }, {}, storage);
        const thumb: ThumbRefBytes = {
          blobId: up.blobId,
          key: up.key,
          nonce: up.nonce,
          mime: "image/jpeg",
          size: bytes.length,
          width: meta.thumbSize.width,
          height: meta.thumbSize.height,
        };
        out = { ...out, thumb };
      } catch {
        // Sent without a thumb; the receiver lays it out from the dimensions.
      }
    }
    if (item.source === "audio" && meta?.audio !== undefined) {
      const a = meta.audio;
      out = {
        ...out,
        kind: "audio",
        title: a.title,
        ...(a.artist === undefined ? {} : { artist: a.artist }),
        ...(a.durationMs === undefined ? {} : { durationMs: a.durationMs }),
      };
      if (a.cover !== undefined) {
        try {
          const bytes = new Uint8Array(await a.cover.arrayBuffer());
          const up = await encryptAndUploadBytes(bytes, { mime: "image/jpeg", name: undefined }, {}, storage);
          out = {
            ...out,
            thumb: { blobId: up.blobId, key: up.key, nonce: up.nonce, mime: "image/jpeg", size: bytes.length, width: undefined, height: undefined },
          };
        } catch {
          // Sent without a cover.
        }
      }
    }
    return out;
  }

  const metaOf = new Map<number, PreparedMeta>();

  async function prepare(item: TrayItem): Promise<Partial<TrayItem>> {
    const prepared = await prepareItem(item);
    metaOf.set(item.id, prepared.meta);
    return prepared.patch;
  }

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
      const item = find(id);
      if (item === undefined || item.status !== "failed") return;
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
    bind(next) {
      deps = next;
    },
    drop() {
      dropped = true;
      queue.length = 0;
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
async function prepareItem(item: TrayItem): Promise<{ patch: Partial<TrayItem>; meta: PreparedMeta }> {
  if (item.kind === "file") {
    if (item.source !== "audio") return { patch: {}, meta: {} };
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
    },
  };
}
