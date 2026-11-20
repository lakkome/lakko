// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * UX 11.1 — "one draft per conversation", kept when the editor is closed with
 * `Save draft`: a sealed `account-kv` row per conversation (so it survives a
 * reload, is written only while the account's stores are writable, and goes
 * with the account on Forget device). Images are stored as their bytes.
 */
import { loadAccountJson, saveAccountJson } from "../storage/account-kv";
import { base64Decode, base64Encode } from "../auth/account-id";
import type { Article } from "../../wasm/lakko_ffi_wasm";
import type { ArticleDraftOut } from "./types";

interface StoredDraft {
  readonly v: 1;
  readonly article: Article;
  readonly images: readonly { readonly mime: string; readonly b64: string }[];
}

const rowOf = (draftKey: string): string => `article-draft:${draftKey}`;

/** Encode a draft for storage (pure: the images' bytes already read). */
export function encodeDraft(article: Article, images: readonly { mime: string; bytes: Uint8Array }[]): StoredDraft {
  return { v: 1, article, images: images.map((i) => ({ mime: i.mime, b64: base64Encode(i.bytes) })) };
}

/** Decode a stored draft; `null` for anything not shaped like one. */
export function decodeDraft(raw: unknown): ArticleDraftOut | null {
  if (raw === null || typeof raw !== "object") return null;
  const d = raw as Partial<StoredDraft>;
  if (d.v !== 1 || d.article === undefined || !Array.isArray(d.images)) return null;
  try {
    const images = d.images.map((i) => new Blob([base64Decode(i.b64).slice().buffer as ArrayBuffer], { type: i.mime }));
    return { article: d.article, images };
  } catch {
    return null;
  }
}

export async function saveArticleDraft(accountId: string, draftKey: string, out: ArticleDraftOut): Promise<void> {
  const images = await Promise.all(
    out.images.map(async (b) => ({ mime: b.type || "image/jpeg", bytes: new Uint8Array(await b.arrayBuffer()) })),
  );
  await saveAccountJson(accountId, rowOf(draftKey), encodeDraft(out.article, images));
}

export async function loadArticleDraft(accountId: string, draftKey: string): Promise<ArticleDraftOut | null> {
  try {
    return decodeDraft(await loadAccountJson(accountId, rowOf(draftKey)));
  } catch {
    return null;
  }
}

/** A published draft is gone (an empty row reads as no draft). */
export async function deleteArticleDraft(accountId: string, draftKey: string): Promise<void> {
  await saveAccountJson(accountId, rowOf(draftKey), null);
}
