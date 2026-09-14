// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0047 — the chat view's media runtime, shared by every media bubble:
 *
 *   * decrypted media as object URLs, owned by one `ObjectUrlRegistry` that
 *     the view disposes on unmount — so no decrypted plaintext outlives the
 *     chat that opened it;
 *   * one fetch per blob, however many bubbles ask;
 *   * the one-at-a-time player and the auto-continue to the next voice
 *     message (`playback-queue.ts` decides which);
 *   * the local-only "played" set (UX spec §8 — never sent).
 *
 * Every `Blob` is stamped with a type only if the core's MIME allowlist
 * accepts it (`blobTypeFor`), so a refused type is never sniffed into
 * something a media element renders.
 */

import { createSignal, type Accessor } from "solid-js";

import { downloadAndDecryptRef, type AttachmentRefBytes, type ThumbRefBytes } from "../chat/attachments";
import { inlineMediaType as wasmInlineMediaType } from "../../wasm/lakko_ffi_wasm";
import { blobTypeFor, inlineTypeOf, renderDecision, type InlineType, type RenderDecision } from "./inline-media";
import { ObjectUrlRegistry } from "./object-url-registry";
import { nextVoiceToPlay, type VoiceQueueItem } from "./playback-queue";

/** Lowercase hex of a blob id — the key every media URL is filed under. */
export function mediaKey(blobId: Uint8Array): string {
  let out = "";
  for (const b of blobId) out += b.toString(16).padStart(2, "0");
  return out;
}

export function allowlist(mime: string): number | undefined {
  return wasmInlineMediaType(mime);
}

export interface MediaHub {
  readonly registry: ObjectUrlRegistry;
  decision(ref: AttachmentRefBytes): RenderDecision;
  inlineType(mime: string | undefined): InlineType | null;
  /** Reactive: the URL already filed under `key`, or `null`. */
  urlFor(key: string): string | null;
  /** Download, decrypt (authenticated) and file the media; resolves its URL. */
  load(ref: AttachmentRefBytes): Promise<string>;
  /** Same for a poster thumb; `null` when its type is not an allowlisted image. */
  loadThumb(thumb: ThumbRefBytes): Promise<string | null>;
  /** File local bytes (a take not yet uploaded) under `key`. */
  fileLocal(key: string, blob: Blob, mime: string | undefined): string | null;
  revoke(key: string): void;

  isPlayed(key: string): boolean;
  markPlayed(key: string): void;
  playedKeys: Accessor<ReadonlySet<string>>;
  seedPlayed(keys: Iterable<string>): void;

  /** Become the only thing playing; the previous player is paused. */
  claim(key: string, pause: () => void): void;
  release(key: string): void;
  /** The key a bubble should start playing now (auto-continue), or `null`. */
  requestedPlay: Accessor<string | null>;
  consumeRequest(key: string): void;
  voiceEnded(key: string): void;

  dispose(): void;
}

export function createMediaHub(opts: {
  /** The timeline's voice messages in order, for auto-continue. */
  voiceQueue: () => readonly VoiceQueueItem[];
  /** Fired when the played set changes, so the log can persist it. */
  onPlayedChange?: () => void;
}): MediaHub {
  const registry = new ObjectUrlRegistry();
  const [version, setVersion] = createSignal(0);
  const [played, setPlayed] = createSignal<ReadonlySet<string>>(new Set());
  const [requestedPlay, setRequestedPlay] = createSignal<string | null>(null);
  const inflight = new Map<string, Promise<string>>();
  let current: { key: string; pause: () => void } | null = null;

  function file(key: string, bytes: Uint8Array | Blob, mime: string | undefined): string | null {
    const type = blobTypeFor(mime, allowlist);
    const blob = bytes instanceof Blob ? new Blob([bytes], { type }) : new Blob([bytes.slice()], { type });
    const url = registry.set(key, blob);
    setVersion((v) => v + 1);
    return url;
  }

  function fetchAndFile(key: string, ref: { blobId: Uint8Array; key: Uint8Array; nonce: Uint8Array }, mime: string | undefined): Promise<string> {
    const existing = registry.get(key);
    if (existing !== null) return Promise.resolve(existing);
    const pending = inflight.get(key);
    if (pending !== undefined) return pending;
    const p = downloadAndDecryptRef({
      blobId: ref.blobId,
      key: ref.key,
      nonce: ref.nonce,
      mime,
      size: 0,
      name: undefined,
    })
      .then((plaintext) => {
        const url = file(key, plaintext, mime);
        if (url === null) throw new Error("chat view closed");
        return url;
      })
      .finally(() => inflight.delete(key));
    inflight.set(key, p);
    return p;
  }

  return {
    registry,
    decision: (ref) => renderDecision(ref, allowlist),
    inlineType: (mime) => inlineTypeOf(mime, allowlist),
    urlFor(key) {
      version();
      return registry.get(key);
    },
    load: (ref) => fetchAndFile(mediaKey(ref.blobId), ref, ref.mime),
    async loadThumb(thumb) {
      if (inlineTypeOf(thumb.mime, allowlist) !== "image") return null;
      return fetchAndFile(`thumb:${mediaKey(thumb.blobId)}`, thumb, thumb.mime);
    },
    fileLocal: (key, blob, mime) => file(key, blob, mime),
    revoke(key) {
      registry.revoke(key);
      setVersion((v) => v + 1);
    },
    isPlayed: (key) => played().has(key),
    markPlayed(key) {
      if (played().has(key)) return;
      const next = new Set(played());
      next.add(key);
      setPlayed(next);
      opts.onPlayedChange?.();
    },
    playedKeys: played,
    seedPlayed(keys) {
      const next = new Set(played());
      for (const k of keys) next.add(k);
      setPlayed(next);
    },
    claim(key, pause) {
      if (current !== null && current.key !== key) {
        const prev = current;
        current = null;
        prev.pause();
      }
      current = { key, pause };
    },
    release(key) {
      if (current?.key === key) current = null;
    },
    requestedPlay,
    consumeRequest(key) {
      if (requestedPlay() === key) setRequestedPlay(null);
    },
    voiceEnded(key) {
      if (current?.key === key) current = null;
      const next = nextVoiceToPlay(
        opts.voiceQueue().map((item) => ({ ...item, played: item.played || played().has(item.key) })),
        key,
      );
      setRequestedPlay(next);
    },
    dispose() {
      current?.pause();
      current = null;
      registry.dispose();
      setVersion((v) => v + 1);
    },
  };
}
