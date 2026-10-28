// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0084 section 3 / docs/ux/profile-screen.md section 7 — the per-device
 * cache of decrypted avatars, keyed by blob id.
 *
 * Lazy: nothing is fetched until a circle that needs the photo is drawn
 * (`want`), and never from the service worker, which keeps showing initials.
 * A blob already held is never fetched again. A download, a decryption, a
 * size check or the decode check that fails leaves the initials and is tried
 * again on the next display after {@link RETRY_AFTER_MS}.
 *
 * Pure over its ports; the IndexedDB backend (encrypted under the device key,
 * the posture of every other local store) is `avatar-cache-idb.ts`.
 */

import type { AvatarRefData } from "./avatar-model";

export type AvatarMediaKind = "still" | "video";

/** A failed load is tried again on the next display after this long. */
export const RETRY_AFTER_MS = 60_000;

export interface AvatarCacheBackend {
  get(blobIdB64: string): Promise<Uint8Array | null>;
  put(blobIdB64: string, bytes: Uint8Array): Promise<void>;
}

export interface AvatarCacheDeps {
  readonly backend: AvatarCacheBackend;
  /** Download and decrypt. Throws on failure. */
  fetchAndDecrypt(ref: AvatarRefData): Promise<Uint8Array>;
  /** Whether the bytes decode as `kind` (an image / an MP4 the device plays). */
  verify(bytes: Uint8Array, kind: AvatarMediaKind): Promise<boolean>;
  now(): number;
}

type Entry =
  | { readonly status: "loading" }
  | { readonly status: "ready"; readonly bytes: Uint8Array }
  | { readonly status: "failed"; readonly atMs: number };

export class AvatarCache {
  private readonly entries = new Map<string, Entry>();
  private readonly listeners = new Set<() => void>();

  constructor(private readonly deps: AvatarCacheDeps) {}

  /** The decrypted bytes, when held. */
  bytes(blobIdB64: string): Uint8Array | null {
    const e = this.entries.get(blobIdB64);
    return e?.status === "ready" ? e.bytes : null;
  }

  /** Whether a load for `blobIdB64` would start now. */
  shouldLoad(blobIdB64: string): boolean {
    const e = this.entries.get(blobIdB64);
    if (e === undefined) return true;
    if (e.status === "failed") return this.deps.now() - e.atMs >= RETRY_AFTER_MS;
    return false;
  }

  /** A circle wants this blob: start loading it unless held, in flight, or
   *  failed less than a minute ago. Resolves when the attempt is over. */
  want(ref: AvatarRefData, kind: AvatarMediaKind): Promise<void> {
    const id = ref.blobIdB64;
    if (!this.shouldLoad(id)) return Promise.resolve();
    this.entries.set(id, { status: "loading" });
    return this.load(ref, kind).then(
      (bytes) => {
        this.entries.set(id, { status: "ready", bytes });
        this.notify();
      },
      () => {
        this.entries.set(id, { status: "failed", atMs: this.deps.now() });
        this.notify();
      },
    );
  }

  /** Put bytes this device produced itself (its own upload): no download. */
  seed(blobIdB64: string, bytes: Uint8Array): void {
    this.entries.set(blobIdB64, { status: "ready", bytes });
    void this.deps.backend.put(blobIdB64, bytes).catch(() => undefined);
    this.notify();
  }

  /** Forget everything in memory (sign-out). */
  clear(): void {
    this.entries.clear();
    this.notify();
  }

  subscribe(listener: () => void): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  private async load(ref: AvatarRefData, kind: AvatarMediaKind): Promise<Uint8Array> {
    let held: Uint8Array | null = null;
    try {
      held = await this.deps.backend.get(ref.blobIdB64);
    } catch {
      held = null;
    }
    if (held !== null && held.length === ref.size && (await this.deps.verify(held, kind))) return held;
    const bytes = await this.deps.fetchAndDecrypt(ref);
    if (bytes.length !== ref.size) throw new Error("avatar size mismatch");
    if (!(await this.deps.verify(bytes, kind))) throw new Error("avatar does not decode");
    try {
      await this.deps.backend.put(ref.blobIdB64, bytes);
    } catch {
      // Kept in memory for this session; fetched again next time.
    }
    return bytes;
  }

  private notify(): void {
    for (const l of this.listeners) {
      try {
        l();
      } catch {
        /* a listener's failure is its own */
      }
    }
  }
}

/** A backend that keeps bytes in memory — tests, and a session with no
 *  device key. */
export class MemoryAvatarBackend implements AvatarCacheBackend {
  readonly rows = new Map<string, Uint8Array>();
  async get(id: string): Promise<Uint8Array | null> {
    return this.rows.get(id) ?? null;
  }
  async put(id: string, bytes: Uint8Array): Promise<void> {
    this.rows.set(id, bytes);
  }
}
