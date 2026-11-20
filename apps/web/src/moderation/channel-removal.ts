// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0098 step 5, `docs/ux/reporting-and-moderation.md` section 3 ("A
 * channel that was removed") - a channel Lakko's moderators removed, as this
 * page holds it.
 *
 * The channels service answers every route of a removed channel `410
 * {"code":"channel-removed"}`. That is not the owner deleting it (`410
 * channel-deleted`, which ends the channel here): the channel stays in the
 * list, the posts this device holds stay readable, and nothing that writes is
 * offered. What the conversation and the info then SAY depends on the
 * moderation log: `removed` ("This channel was removed for breaking Lakko's
 * rules.") only when `moderationChannelStanding` verified the removal
 * (C-97), `unavailable` ("This channel isn't available.") otherwise.
 *
 * While a channel is held as removed, no request for it leaves the page,
 * except one check per standing reuse period (the snapshot's own reuse time,
 * at least a minute; ten minutes when there is none). A check that gets a
 * normal answer ends the state with no trace (a restore); one that gets the
 * removal again refreshes it. Memory only: nothing is written to disk.
 */

import { createSignal } from "solid-js";

import { writeGuardedFetch } from "../auth/suspension-state";

import { MIN_REUSE_MS, suspensionMarks, type StandingResult } from "./suspension-marks";

/** The code the channels service answers for a removed channel. */
export const CHANNEL_REMOVED = "channel-removed";

/** When the standing gives no reuse time (nothing pinned, a failure), a
 *  removed channel is asked again after this long. Chosen. */
export const CHANNEL_RECHECK_FALLBACK_MS = 10 * 60_000;

/** What the conversation and the info say about a removed channel. */
export type ChannelRemovalLine = "removed" | "unavailable";

/** The catalog key of a line. */
export function channelRemovalLineKey(line: ChannelRemovalLine): "suspended.channel_removed" | "channels.unavailable" {
  return line === "removed" ? "suspended.channel_removed" : "channels.unavailable";
}

/** Whether a channels error says moderators removed the channel. Pure. */
export function isChannelRemovedError(status: number, code: string): boolean {
  return status === 410 && code === CHANNEL_REMOVED;
}

/** The 32-hex channel id a channels URL names, or `null`. Pure. */
export function channelIdOfUrl(url: string): string | null {
  const m = /\/v1\/channels\/([0-9a-fA-F]{32})(?:[/?#]|$)/.exec(url);
  return m === null ? null : (m[1] ?? "").toLowerCase();
}

/** When to ask a removed channel again, from the standing's answer. Pure. */
export function nextCheckAt(result: StandingResult, now: number): number {
  if (!Number.isFinite(result.recheckAtMs)) return now + CHANNEL_RECHECK_FALLBACK_MS;
  return Math.max(result.recheckAtMs, now + MIN_REUSE_MS);
}

interface Entry {
  readonly line: ChannelRemovalLine;
  readonly nextCheckAtMs: number;
  /** Bumped on every note, so a late standing answer for an older note is
   *  not applied. */
  readonly generation: number;
}

export interface ChannelRemovalsDeps {
  /** The channel's standing (`SuspensionMarks.checkChannel`). Never throws. */
  readonly standing: (channelId: Uint8Array) => Promise<StandingResult>;
  readonly now: () => number;
  /** Called after every change a view could show. */
  readonly onChange: () => void;
}

function idBytes(idHex: string): Uint8Array | null {
  if (!/^[0-9a-f]{32}$/.test(idHex)) return null;
  const out = new Uint8Array(16);
  for (let i = 0; i < 16; i += 1) out[i] = Number.parseInt(idHex.slice(i * 2, i * 2 + 2), 16);
  return out;
}

/** The removed channels this page holds. */
export class ChannelRemovals {
  private readonly entries = new Map<string, Entry>();
  private generation = 0;

  constructor(private readonly deps: ChannelRemovalsDeps) {}

  /** The line for `idHex`, or `null` when it is not held as removed. */
  line(idHex: string): ChannelRemovalLine | null {
    return this.entries.get(idHex.toLowerCase())?.line ?? null;
  }

  /** The service answered `410 channel-removed` for `idHex`. Asks the log
   *  whether the removal is verified; until it answers the line is the
   *  neutral one (or the one already held). */
  noteRemoved(idHex: string): void {
    const id = idHex.toLowerCase();
    const bytes = idBytes(id);
    if (bytes === null) return;
    const generation = ++this.generation;
    const held = this.entries.get(id);
    const now = this.deps.now();
    this.entries.set(id, {
      line: held?.line ?? "unavailable",
      nextCheckAtMs: now + CHANNEL_RECHECK_FALLBACK_MS,
      generation,
    });
    if (held === undefined) this.deps.onChange();
    void this.deps.standing(bytes).then((result) => {
      const current = this.entries.get(id);
      if (current === undefined || current.generation !== generation) return;
      const line: ChannelRemovalLine = result.standing === "suspended" ? "removed" : "unavailable";
      this.entries.set(id, { line, nextCheckAtMs: nextCheckAt(result, this.deps.now()), generation });
      if (line !== current.line) this.deps.onChange();
    });
  }

  /** The service answered normally for `idHex`: a restore, or it was never
   *  removed. Nothing about it is kept. */
  noteAnswered(idHex: string): void {
    if (this.entries.delete(idHex.toLowerCase())) this.deps.onChange();
  }

  /**
   * Whether a request for `idHex` may leave now. A channel not held as
   * removed: always. A removed one: once its check is due; that request IS
   * the check, and until it returns no other leaves (the next check is put
   * off by the fallback period at once).
   */
  mayRequest(idHex: string): boolean {
    const id = idHex.toLowerCase();
    const entry = this.entries.get(id);
    if (entry === undefined) return true;
    const now = this.deps.now();
    if (now < entry.nextCheckAtMs) return false;
    this.entries.set(id, { ...entry, nextCheckAtMs: now + CHANNEL_RECHECK_FALLBACK_MS });
    return true;
  }

  /** Forget everything held (sign-out). */
  clear(): void {
    if (this.entries.size === 0) return;
    this.entries.clear();
    this.deps.onChange();
  }
}

// ----------------------------------------------------------------------------
// The page's state and its reactive reads
// ----------------------------------------------------------------------------

const [version, setVersion] = createSignal(0);
const bump = (): void => {
  setVersion((v) => v + 1);
};

let removals: ChannelRemovals | null = null;

/** The page's one holder, asking the page's standing service. */
export function channelRemovals(): ChannelRemovals {
  if (removals === null) {
    removals = new ChannelRemovals({
      standing: (id) => suspensionMarks().checkChannel(id),
      now: () => Date.now(),
      onChange: bump,
    });
  }
  return removals;
}

/** Tests: replace the page's holder. */
export function installChannelRemovals(next: ChannelRemovals | null): void {
  removals = next;
  bump();
}

/** Reactive: the line a removed channel shows, or `null`. */
export function channelRemovalLine(idHex: string): ChannelRemovalLine | null {
  version();
  return channelRemovals().line(idHex);
}

/** Reactive: whether `idHex` is held as removed (nothing that writes is
 *  offered for it). */
export function isChannelRemoved(idHex: string): boolean {
  return channelRemovalLine(idHex) !== null;
}

/** Sign-out: nothing held. */
export function clearChannelRemovals(): void {
  removals?.clear();
}

/** The body of the answer a held request gets instead of the network. */
const HELD_BODY = JSON.stringify({ code: CHANNEL_REMOVED, detail: "the channel was removed" });

/**
 * `fetch` for the channels service. A request naming a channel held as
 * removed is answered here (the same `410 channel-removed` the service gave)
 * unless its check is due; every real answer for a channel updates what is
 * held: `410 channel-removed` notes the removal, a success ends it.
 */
export async function channelServiceFetch(
  send: (input: string, init?: RequestInit) => Promise<Response>,
  url: string,
  init?: RequestInit,
  holder: ChannelRemovals = channelRemovals(),
): Promise<Response> {
  const id = channelIdOfUrl(url);
  if (id === null) return send(url, init);
  if (!holder.mayRequest(id)) {
    return new Response(HELD_BODY, { status: 410, headers: { "content-type": "application/json" } });
  }
  const resp = await send(url, init);
  if (resp.ok) {
    holder.noteAnswered(id);
  } else if (resp.status === 410) {
    const code = await codeOf(resp);
    if (code === CHANNEL_REMOVED) holder.noteRemoved(id);
    else holder.noteAnswered(id);
  }
  return resp;
}

async function codeOf(resp: Response): Promise<string | null> {
  try {
    if (!(resp.headers.get("content-type") ?? "").toLowerCase().includes("application/json")) return null;
    const body = (await resp.clone().json()) as { code?: unknown; error?: unknown };
    const code = body.code ?? body.error;
    return typeof code === "string" ? code : null;
  } catch {
    return null;
  }
}

/** {@link channelServiceFetch} over the suspension guard: the `fetch` every
 *  channels client uses. */
export function guardedChannelFetch(input: string, init?: RequestInit): Promise<Response> {
  return channelServiceFetch((u, i) => writeGuardedFetch(u, i), input, init);
}
