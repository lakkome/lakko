// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The per-group settings that travel to every member beside the group name
 * (ADR-0051's template): the group photo (ADR-0087 section 18.1,
 * `docs/ux/group-and-channel-privacy.md` section 4.5) and the allowed
 * reactions (`docs/ux/reactions-and-emoji.md` section 9).
 *
 * Each is an owner-or-admin envelope, stamped, the strictly newer stamp wins
 * per group, and a growth re-send carries the stamp verbatim. The receive gate
 * is the group name's ({@link groupNameVerdict}), applied to a canonical
 * string of the value, so the two rules cannot drift.
 *
 * Kept in one sealed per-account row each (`storage/account-kv.ts`).
 */

import { loadAccountJson, saveAccountJson } from "../storage/account-kv";
import type { AvatarData } from "../profile/avatar-model";
import type { AllowedSetting } from "./reaction-rules";

/** A group's photo as the envelope carried it. `avatar: null` = removed. */
export interface StoredGroupPhoto {
  readonly avatar: AvatarData | null;
  readonly updatedAtMs: bigint;
}

/** A group's allowed reactions as the envelope carried it. */
export interface StoredGroupReactions {
  readonly allowed: AllowedSetting;
  readonly updatedAtMs: bigint;
}

/** One canonical string per value, for the identical-re-send test. */
export function photoKey(v: StoredGroupPhoto): string {
  return JSON.stringify(v.avatar);
}

export function reactionsKey(v: StoredGroupReactions): string {
  return JSON.stringify(v.allowed);
}

interface Codec<V> {
  encode(v: V): unknown;
  decode(raw: unknown): V | null;
}

function stampOf(raw: unknown): bigint | null {
  return typeof raw === "string" && /^\d+$/.test(raw) ? BigInt(raw) : null;
}

export const PHOTO_CODEC: Codec<StoredGroupPhoto> = {
  encode: (v) => ({ a: v.avatar, t: v.updatedAtMs.toString() }),
  decode: (raw) => {
    if (raw === null || typeof raw !== "object") return null;
    const r = raw as { a?: unknown; t?: unknown };
    const t = stampOf(r.t);
    if (t === null) return null;
    const a = r.a;
    if (a === null) return { avatar: null, updatedAtMs: t };
    if (typeof a !== "object" || a === undefined) return null;
    const still = (a as { still?: unknown }).still;
    if (still === null || typeof still !== "object") return null;
    return { avatar: a as AvatarData, updatedAtMs: t };
  },
};

export const REACTIONS_CODEC: Codec<StoredGroupReactions> = {
  encode: (v) => ({ a: v.allowed, t: v.updatedAtMs.toString() }),
  decode: (raw) => {
    if (raw === null || typeof raw !== "object") return null;
    const r = raw as { a?: unknown; t?: unknown };
    const t = stampOf(r.t);
    if (t === null || r.a === null || typeof r.a !== "object") return null;
    const mode = (r.a as { mode?: unknown }).mode;
    if (mode === "all" || mode === "off") return { allowed: { mode }, updatedAtMs: t };
    if (mode === "only") {
      const emoji = (r.a as { emoji?: unknown }).emoji;
      if (!Array.isArray(emoji)) return null;
      const list = emoji.filter((e): e is string => typeof e === "string");
      return list.length > 0 ? { allowed: { mode: "only", emoji: list }, updatedAtMs: t } : null;
    }
    return null;
  },
};

/** One group setting per group, in memory, mirrored to one sealed row. */
export class GroupSettingStore<V> {
  private rows = new Map<string, V>();
  private account: string | null = null;
  private readonly listeners = new Set<() => void>();
  private chain: Promise<void> = Promise.resolve();

  constructor(
    private readonly rowName: string,
    private readonly codec: Codec<V>,
  ) {}

  /** Load the account's row; `false` when it could not be read. */
  async open(accountId: string): Promise<boolean> {
    this.account = accountId;
    let raw: unknown = null;
    let readable = true;
    try {
      raw = await loadAccountJson(accountId, this.rowName);
    } catch {
      readable = false;
    }
    if (this.account !== accountId) return readable;
    const next = new Map<string, V>();
    if (raw !== null && typeof raw === "object" && (raw as { v?: unknown }).v === 1) {
      const g = (raw as { g?: unknown }).g;
      if (g !== null && typeof g === "object") {
        for (const [k, v] of Object.entries(g as Record<string, unknown>)) {
          const decoded = this.codec.decode(v);
          if (decoded !== null) next.set(k, decoded);
        }
      }
    }
    for (const [k, v] of this.rows) next.set(k, v);
    this.rows = next;
    this.notify();
    return readable;
  }

  close(): void {
    this.account = null;
    this.rows = new Map();
    this.notify();
  }

  get(groupIdB64: string): V | undefined {
    return this.rows.get(groupIdB64);
  }

  set(accountId: string, groupIdB64: string, value: V): Promise<void> {
    this.rows = new Map(this.rows).set(groupIdB64, value);
    this.notify();
    return this.persist(accountId);
  }

  forget(accountId: string, groupIdB64: string): Promise<void> {
    if (!this.rows.has(groupIdB64)) return Promise.resolve();
    const next = new Map(this.rows);
    next.delete(groupIdB64);
    this.rows = next;
    this.notify();
    return this.persist(accountId);
  }

  subscribe(fn: () => void): () => void {
    this.listeners.add(fn);
    return () => {
      this.listeners.delete(fn);
    };
  }

  private persist(accountId: string): Promise<void> {
    if (this.account !== accountId) return Promise.resolve();
    const g: Record<string, unknown> = {};
    for (const [k, v] of this.rows) g[k] = this.codec.encode(v);
    const payload = { v: 1, g };
    this.chain = this.chain
      .then(() => saveAccountJson(accountId, this.rowName, payload))
      .catch((err: unknown) => {
        // eslint-disable-next-line no-console
        console.warn(`[${this.rowName}] persist failed`, err instanceof Error ? err.name : "error");
      });
    return this.chain;
  }

  private notify(): void {
    for (const fn of this.listeners) fn();
  }
}
