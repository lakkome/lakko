// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0060 section 7 - the incoming half: a DeviceGroup `ServiceEnvelope`
 * body from another device of this account.
 *
 * The body carries one `lakko-contacts` envelope verbatim. It is dispatched
 * (`dispatchIncomingEnvelope`) and only three kinds are kept from it:
 * Visibility (the "who can find me" setting), kept only when strictly newer
 * by its own stamp; ReachState (the reach key version counters), handed
 * whatever its stamp to the core's `mergeReachState` rule (an older state
 * can still carry a counter this device lacks; the core decides what is
 * stored and what is sent back); and (ADR-0048 section 4a) Profile, the own
 * display name set on another device, kept only when it names no other
 * account and its stamp is strictly newer than the stored one. Anything else
 * inside a `ServiceEnvelope` is dropped: this carrier is the owner's own
 * devices talking, and no other envelope kind has a meaning on it.
 *
 * The DeviceGroup can deliver before the account's setting and reach
 * session are loaded (the journal cursor moves on either way), so an
 * envelope that arrives early is held (the newest Visibility, the newest
 * Profile, every ReachState up to a bound) and applied by
 * {@link applyHeldServiceEnvelopes}, which `ReachProvider` calls once both
 * are loaded and `ProfileContext` once the own profile is bound.
 */
import type { Wasm } from "../wasm";
import { loadWasm } from "../wasm";
import { reachSession } from "./reach-service";
import { isVisibilityLoaded, receiveVisibility, visibilitySetting } from "./visibility-store";
import { isOwnProfileBound, ownProfileHeld, receiveOwnProfile } from "../profile/own-profile-sync";

/** What to do with one `ServiceEnvelope` body. */
export type ServiceEnvelopeVerdict =
  | { readonly kind: "visibility"; readonly updatedAtMs: bigint }
  | { readonly kind: "reachState"; readonly updatedAtMs: bigint }
  /** The own display name from another device (`null` = cleared). */
  | { readonly kind: "profile"; readonly updatedAtMs: bigint; readonly displayName: string | null }
  /** A Visibility or Profile not newer than what is held (equal included: a
   *  re-send). */
  | { readonly kind: "stale"; readonly of: "visibility" | "profile" }
  /** Any other envelope kind, a Profile naming another account, or bytes
   *  that are no envelope at all. */
  | { readonly kind: "foreign" };

/** The setting stamp this device holds; `null` = nothing loaded to compare
 *  with. (A ReachState is not pre-filtered: the core's merge decides.) */
export interface HeldStamps {
  readonly visibility: bigint | null;
  /** The stored own-profile stamp and this account's 16-byte id; `null`
   *  while no account is bound. */
  readonly ownProfile: { readonly stamp: bigint; readonly accountId: Uint8Array | null } | null;
}

/**
 * Classify one body against the held stamps. Pure over the wasm core; never
 * throws. A kind whose stamp is `null` classifies by kind alone (the caller
 * holds it until there is something to compare with).
 */
export function classifyServiceEnvelope(
  wasm: Wasm,
  envelope: Uint8Array,
  held: HeldStamps,
): ServiceEnvelopeVerdict {
  let dispatched;
  try {
    dispatched = wasm.dispatchIncomingEnvelope(envelope);
  } catch {
    return { kind: "foreign" };
  }
  try {
    switch (dispatched.kind) {
      case wasm.IncomingEnvelopeKind.Visibility: {
        const v = dispatched.visibility;
        if (v === undefined) return { kind: "foreign" };
        const stamp = v.updatedAtMs;
        v.free();
        if (held.visibility !== null && !wasm.visibilityIsNewer(stamp, held.visibility)) {
          return { kind: "stale", of: "visibility" };
        }
        return { kind: "visibility", updatedAtMs: stamp };
      }
      case wasm.IncomingEnvelopeKind.ReachState: {
        const r = dispatched.reachState;
        if (r === undefined) return { kind: "foreign" };
        const stamp = r.updatedAtMs;
        r.free();
        return { kind: "reachState", updatedAtMs: stamp };
      }
      case wasm.IncomingEnvelopeKind.Profile: {
        const p = dispatched.profile;
        if (p === undefined) return { kind: "foreign" };
        let stamp: bigint;
        let displayName: string | null;
        let accountId: Uint8Array | undefined;
        try {
          stamp = p.updatedAtMs;
          displayName = p.displayName ?? null;
          accountId = p.accountId;
        } finally {
          p.free();
        }
        const own = held.ownProfile;
        if (own === null) return { kind: "profile", updatedAtMs: stamp, displayName };
        // It names an account: it must be this one. (One without an id is a
        // DeviceGroup member's, which is this account by construction.)
        if (accountId !== undefined && (own.accountId === null || !sameBytes(accountId, own.accountId))) {
          return { kind: "foreign" };
        }
        if (!wasm.profileIsNewer(stamp, own.stamp)) return { kind: "stale", of: "profile" };
        return { kind: "profile", updatedAtMs: stamp, displayName };
      }
      default:
        return { kind: "foreign" };
    }
  } finally {
    dispatched.free();
  }
}

function sameBytes(a: Uint8Array, b: Uint8Array): boolean {
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i++) if (a[i] !== b[i]) return false;
  return true;
}

/** The intake chain: bodies are applied one at a time. */
let intake: Promise<void> = Promise.resolve();

/** Held until the account's state is loaded: the newest Visibility, and
 *  every ReachState (an older one may carry a counter a newer one lacks, and
 *  the core's merge folds each), up to {@link MAX_HELD_REACH_STATES}. */
let heldVisibility: { stamp: bigint; bytes: Uint8Array } | null = null;
let heldProfile: { stamp: bigint; bytes: Uint8Array } | null = null;
const heldReachStates: Uint8Array[] = [];
/** Chosen, not measured: a device receives a handful before it loads. */
const MAX_HELD_REACH_STATES = 16;

function holdVisibility(stamp: bigint, bytes: Uint8Array): void {
  if (heldVisibility === null || stamp > heldVisibility.stamp) heldVisibility = { stamp, bytes: bytes.slice() };
}

function holdProfile(stamp: bigint, bytes: Uint8Array): void {
  if (heldProfile === null || stamp > heldProfile.stamp) heldProfile = { stamp, bytes: bytes.slice() };
}

function holdReachState(bytes: Uint8Array): void {
  if (heldReachStates.length >= MAX_HELD_REACH_STATES) heldReachStates.shift();
  heldReachStates.push(bytes.slice());
}

/** The stamps of what this device holds now. */
function heldStamps(): HeldStamps {
  const setting = isVisibilityLoaded() ? visibilitySetting() : null;
  return { visibility: setting === null ? null : setting.updatedAtMs, ownProfile: ownProfileHeld() };
}

/**
 * One `ServiceEnvelope` body from the DeviceGroup: keep a newer Visibility,
 * hand any ReachState to the core's merge, adopt a newer own Profile, hold
 * each while its state is not loaded yet, drop anything else. Returns the verdict (for the log). Never throws.
 */
export function receiveServiceEnvelope(envelope: Uint8Array): Promise<ServiceEnvelopeVerdict> {
  // One at a time, in journal order: each compares against what the one
  // before it kept.
  const run = intake.then(() => receiveOne(envelope));
  intake = run.then(
    () => undefined,
    () => undefined,
  );
  return run;
}

async function receiveOne(envelope: Uint8Array): Promise<ServiceEnvelopeVerdict> {
  const wasm = await loadWasm();
  const verdict = classifyServiceEnvelope(wasm, envelope, heldStamps());
  try {
    if (verdict.kind === "visibility") {
      if (!isVisibilityLoaded()) holdVisibility(verdict.updatedAtMs, envelope);
      else await receiveVisibility(envelope);
    } else if (verdict.kind === "profile") {
      // Held while unbound; re-classified against the account when applied.
      // Adopted into the own-profile store only: never republished.
      if (!isOwnProfileBound()) holdProfile(verdict.updatedAtMs, envelope);
      else receiveOwnProfile(verdict.displayName, verdict.updatedAtMs);
    } else if (verdict.kind === "reachState") {
      const session = reachSession();
      if (session === null) {
        holdReachState(envelope);
      } else {
        const state = wasm.decodeReachStateEnvelope(envelope);
        try {
          await session.receiveReachState(state);
        } finally {
          state.free();
        }
      }
    }
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("[reach] a setting from another device was not applied", err instanceof Error ? err.message : "");
  }
  return verdict;
}

/** Apply what arrived before the state was loaded. Never throws. */
export async function applyHeldServiceEnvelopes(): Promise<void> {
  if (heldVisibility !== null && isVisibilityLoaded()) {
    const { bytes } = heldVisibility;
    heldVisibility = null;
    await receiveServiceEnvelope(bytes);
  }
  if (heldProfile !== null && isOwnProfileBound()) {
    const { bytes } = heldProfile;
    heldProfile = null;
    await receiveServiceEnvelope(bytes);
  }
  if (heldReachStates.length > 0 && reachSession() !== null) {
    const all = heldReachStates.splice(0);
    for (const bytes of all) await receiveServiceEnvelope(bytes);
  }
}

/** Sign-out: nothing held survives into the next account. */
export function clearHeldServiceEnvelopes(): void {
  heldVisibility = null;
  heldProfile = null;
  heldReachStates.length = 0;
}
