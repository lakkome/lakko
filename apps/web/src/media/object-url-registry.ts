// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Object-URL bookkeeping for decrypted media.
 *
 * Every `URL.createObjectURL` pins its `Blob` — the decrypted plaintext — in
 * memory until `URL.revokeObjectURL` is called or the document goes away. The
 * chat view used to create one per previewed image and never revoke any of
 * them on unmount, so every image, voice message and video note opened in a
 * chat stayed in memory for the life of the tab.
 *
 * The registry owns every URL it hands out, keyed by a caller string
 * (hex blob id for received media, a local key for a take not yet uploaded):
 *
 *   * `set` over an existing key revokes the URL it replaces;
 *   * `revoke` / `revokeAll` release them;
 *   * after `dispose`, a late `set` — a download that resolves after the view
 *     unmounted — revokes its new URL at once and returns `null`, so an
 *     in-flight fetch cannot leak past the view that asked for it.
 */

export type CreateObjectUrl = (blob: Blob) => string;
export type RevokeObjectUrl = (url: string) => void;

export class ObjectUrlRegistry {
  private readonly urls = new Map<string, string>();
  private disposed = false;

  constructor(
    private readonly create: CreateObjectUrl = (b) => URL.createObjectURL(b),
    private readonly release: RevokeObjectUrl = (u) => URL.revokeObjectURL(u),
  ) {}

  get(key: string): string | null {
    return this.urls.get(key) ?? null;
  }

  has(key: string): boolean {
    return this.urls.has(key);
  }

  entries(): IterableIterator<[string, string]> {
    return this.urls.entries();
  }

  get size(): number {
    return this.urls.size;
  }

  get isDisposed(): boolean {
    return this.disposed;
  }

  /** Register `blob` under `key` and return its URL, or `null` once
   *  disposed (the URL is then revoked immediately). */
  set(key: string, blob: Blob): string | null {
    const url = this.create(blob);
    if (this.disposed) {
      this.release(url);
      return null;
    }
    const previous = this.urls.get(key);
    this.urls.set(key, url);
    if (previous !== undefined && previous !== url) this.release(previous);
    return url;
  }

  /**
   * Move the URL filed under `from` to `to`, keeping the URL ITSELF.
   *
   * Not a copy: nothing is created, nothing is revoked, and the URL string does
   * not change — which is the whole point. An outgoing take's bytes are filed
   * under a local key and re-filed under the blob id once the upload lands, and
   * doing that as `set` + `revoke` meant a second `Blob` of the same bytes, a
   * second URL, and the FIRST one revoked from under whatever was playing it.
   * Moving the one entry leaves a media element that is mid-playback holding a
   * source that is still valid.
   *
   * `false` when `from` holds nothing (so the caller can file the bytes
   * itself). An existing entry at `to` is released, as `set` would.
   */
  rename(from: string, to: string): boolean {
    const url = this.urls.get(from);
    if (url === undefined) return false;
    if (from === to) return true;
    const replaced = this.urls.get(to);
    this.urls.delete(from);
    this.urls.set(to, url);
    if (replaced !== undefined && replaced !== url) this.release(replaced);
    return true;
  }

  revoke(key: string): void {
    const url = this.urls.get(key);
    if (url === undefined) return;
    this.urls.delete(key);
    this.release(url);
  }

  revokeAll(): void {
    for (const url of this.urls.values()) this.release(url);
    this.urls.clear();
  }

  /** Revoke everything and refuse (revoke-on-arrival) anything registered
   *  later. Idempotent. */
  dispose(): void {
    this.revokeAll();
    this.disposed = true;
  }
}
