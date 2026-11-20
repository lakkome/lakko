// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `docs/ux/media-messages.md` section 11, chat list row — the 20 px picture at
 * the start of a row's line when the last message carries photos or videos:
 * the first one's own sent thumbnail (never the full photo), a small play
 * glyph on a video's, and one or two offset edges behind it for a message
 * with two or more. Until the thumbnail is decrypted, and if it cannot be,
 * the space shows the row's neutral placeholder, never a broken image.
 *
 * The picture is the micro thumbnail carried inside the message (ADR-0047
 * section 6) when the message has one: drawn at once, nothing fetched. Only
 * without it is the sent thumbnail fetched and decrypted when the row is
 * drawn and kept for the session, a bounded number at a time, so scrolling
 * the list does not fetch it again.
 */
import { createResource, For, Show, type JSX } from "solid-js";

import { downloadAndDecryptRef } from "./attachments";
import { rowThumbSource, stackEdges, type PreviewMedia } from "./list-preview";
import { thumbFromPersisted } from "./persisted-attachment";
import { allowlist } from "../media/media-hub";
import { inlineTypeOf } from "../media/inline-media";
import { Icon } from "../ui/icons";

/** How many decrypted row thumbnails are kept at once (chosen). */
const KEPT = 200;
const cache = new Map<string, Promise<string | null>>();

function keep(key: string, value: Promise<string | null>): void {
  cache.set(key, value);
  while (cache.size > KEPT) {
    const oldest = cache.keys().next().value;
    if (oldest === undefined) break;
    const evicted = cache.get(oldest);
    cache.delete(oldest);
    void evicted?.then((u) => {
      if (u !== null) URL.revokeObjectURL(u);
    });
  }
}

/** The object URL of a row thumbnail, decrypted once per session. */
function thumbUrl(media: PreviewMedia): Promise<string | null> {
  const source = rowThumbSource(media);
  if (source.kind === "inline") return Promise.resolve(source.url);
  if (source.kind === "none") return Promise.resolve(null);
  const ref = thumbFromPersisted(media.thumb);
  if (ref === undefined) return Promise.resolve(null);
  const key = Array.from(ref.blobId, (b) => b.toString(16).padStart(2, "0")).join("");
  const held = cache.get(key);
  if (held !== undefined) {
    // Most recently used goes last.
    cache.delete(key);
    cache.set(key, held);
    return held;
  }
  const loading = (async (): Promise<string | null> => {
    // Only an allowlisted image is ever handed to an element.
    if (inlineTypeOf(ref.mime, allowlist) !== "image") return null;
    try {
      const bytes = await downloadAndDecryptRef({
        blobId: ref.blobId,
        key: ref.key,
        nonce: ref.nonce,
        mime: ref.mime,
        size: ref.size,
        name: undefined,
      });
      return URL.createObjectURL(new Blob([bytes as BlobPart], { type: ref.mime ?? "image/jpeg" }));
    } catch {
      // A failed fetch keeps the placeholder (section 11).
      return null;
    }
  })();
  keep(key, loading);
  // A failure is not kept: the next drawing of the row tries again.
  void loading.then((u) => {
    if (u === null && cache.get(key) === loading) cache.delete(key);
  });
  return loading;
}

export function ListThumb(props: { readonly media: PreviewMedia }): JSX.Element {
  const [url] = createResource(() => props.media, thumbUrl);
  const edges = (): number[] => Array.from({ length: stackEdges(props.media.count) }, (_, i) => i);
  return (
    <span class="lk-row-thumb" aria-hidden="true">
      <For each={edges()}>{(i) => <span class="lk-row-thumb-edge" classList={{ second: i === 1 }} />}</For>
      <span class="lk-row-thumb-face">
        <Show when={url()}>{(u) => <img src={u()} alt="" draggable={false} />}</Show>
        <Show when={props.media.video}>
          <span class="lk-row-thumb-play">
            <Icon name="play-fill" size={8} />
          </span>
        </Show>
      </span>
    </span>
  );
}
