// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0060 section 7 - the outgoing half of carrying the "who can find me"
 * setting (Visibility envelope), the reach-key version counters (ReachState
 * envelope) and, since ADR-0048 section 4a, the own display name (Profile
 * envelope) to the account's other devices, over the DeviceGroup body
 * `ServiceEnvelope`.
 *
 * Two parties meet here without importing each other:
 * - the owners of the states (`visibility-store.ts`, `reach-service.ts`,
 *   `profile/own-profile-sync.ts`)
 *   register a COMPOSER that returns the current envelope, carrying the
 *   state's own stamp, and mark their kind dirty after a local change;
 * - the DeviceGroup sync runtime registers a SENDER once its DeviceGroup is
 *   loaded (`DeviceGroupSyncContext.tsx`).
 *
 * A dirty kind is sent when both exist. What goes out is always the state
 * as it is at send time, composed with its stored stamp: an envelope is
 * never re-stamped to be sent (the ADR-0044 lesson), so a late or repeated
 * send can never outrank a newer change. A send that fails leaves the kind
 * dirty; the DeviceGroup poll retries it on every pass
 * ({@link retryDirtyServiceEnvelopes}), and so does the next change or the
 * next registration of a sender.
 */

/** The envelopes that ride the DeviceGroup `ServiceEnvelope` body: the
 *  setting, the reach key versions, and (ADR-0048 section 4a) the own
 *  display name (`profile/own-profile-sync.ts`), and (ADR-0080) this
 *  device's own card (`devices/device-card-runtime.ts`). */
export type ServiceEnvelopeKind = "visibility" | "reachState" | "profile" | "deviceCard";

/** Returns the current envelope, or `null` while the state is not loaded. */
export type ServiceEnvelopeComposer = () => Promise<Uint8Array | null>;

/** Publishes one `lakko-contacts` envelope over the DeviceGroup. */
export type ServiceEnvelopeSender = (envelope: Uint8Array) => Promise<void>;

const KINDS: readonly ServiceEnvelopeKind[] = ["visibility", "reachState", "profile", "deviceCard"];

const composers = new Map<ServiceEnvelopeKind, ServiceEnvelopeComposer>();
/** Kind -> told after a send of that kind went out and cleared its mark. */
const sentListeners = new Map<ServiceEnvelopeKind, () => void>();
/** Kind -> generation of its latest mark. A send clears the kind only when
 *  no newer mark arrived while it was in flight. */
const dirty = new Map<ServiceEnvelopeKind, number>();
let generation = 0;
let sender: ServiceEnvelopeSender | null = null;
let chain: Promise<void> = Promise.resolve();

/** Register (or, with `null`, remove) the composer of one kind. */
export function setServiceEnvelopeComposer(
  kind: ServiceEnvelopeKind,
  composer: ServiceEnvelopeComposer | null,
): void {
  if (composer === null) composers.delete(kind);
  else composers.set(kind, composer);
}

/** Register (or, with `null`, remove) what is told when a send of `kind`
 *  went out and no newer mark is waiting (the owner clears a persisted
 *  "not yet sent" flag there). */
export function setServiceEnvelopeSentListener(kind: ServiceEnvelopeKind, listener: (() => void) | null): void {
  if (listener === null) sentListeners.delete(kind);
  else sentListeners.set(kind, listener);
}

/** Whether `kind` is marked and not yet sent. */
export function isServiceEnvelopeDirty(kind: ServiceEnvelopeKind): boolean {
  return dirty.has(kind);
}

/**
 * The DeviceGroup poll answered: send again whatever is still marked (a
 * publish that failed earlier). Sends nothing new: only kinds already
 * marked. Never rejects.
 */
export function retryDirtyServiceEnvelopes(): Promise<void> {
  if (dirty.size === 0) return chain;
  return flushServiceEnvelopes();
}

/** A local change of `kind`: send the current envelope when possible. */
export function markServiceEnvelopeDirty(kind: ServiceEnvelopeKind): void {
  generation += 1;
  dirty.set(kind, generation);
  void flushServiceEnvelopes();
}

/** The DeviceGroup is loaded: send what is pending. Returns the function
 *  that unregisters this sender. */
export function registerServiceEnvelopeSender(send: ServiceEnvelopeSender): () => void {
  sender = send;
  void flushServiceEnvelopes();
  return () => {
    if (sender === send) sender = null;
  };
}

/**
 * ADR-0091 section 3, route 1 - send one envelope to the account's other
 * devices now, through the sync runtime's own handle (the registered
 * sender), serialized with the other sends. Rejects when no DeviceGroup is
 * loaded yet or the publish failed; the caller retries.
 */
export function sendServiceEnvelopeNow(envelope: Uint8Array): Promise<void> {
  const run = chain.then(async () => {
    const send = sender;
    if (send === null) throw new Error("no DeviceGroup sender registered");
    await send(envelope);
  });
  chain = run.then(
    () => undefined,
    () => undefined,
  );
  return run;
}

/** Whether a DeviceGroup sender is registered (the account's DeviceGroup sync
 *  runs on this device). Without one {@link sendServiceEnvelopeNow} throws. */
export function hasServiceEnvelopeSender(): boolean {
  return sender !== null;
}

/** Send every dirty kind that has a composer, serialized. Never rejects. */
export function flushServiceEnvelopes(): Promise<void> {
  chain = chain.then(async () => {
    for (const kind of KINDS) {
      const mark = dirty.get(kind);
      const send = sender;
      const compose = composers.get(kind);
      if (mark === undefined || send === null || compose === undefined) continue;
      try {
        const envelope = await compose();
        if (envelope === null) continue;
        await send(envelope);
        if (dirty.get(kind) === mark) {
          dirty.delete(kind);
          sentListeners.get(kind)?.();
        }
      } catch (err) {
        // eslint-disable-next-line no-console
        console.warn(
          `[reach] sending the ${kind} envelope to the other devices failed; retried at the next poll`,
          err instanceof Error ? err.message : "",
        );
      }
    }
  });
  return chain;
}

/**
 * A device was just added to the DeviceGroup: send every kind again, through
 * the sync runtime's own handle (the registered sender), so the new device
 * takes the setting, the key versions and the own name. Never through a second handle of
 * the same DeviceGroup: two handles sealing at the same sender generation
 * reuse a key and nonce, and the receiver refuses the second as a reuse.
 */
export function resendServiceEnvelopes(): void {
  for (const kind of KINDS) {
    if (composers.has(kind)) {
      generation += 1;
      dirty.set(kind, generation);
    }
  }
  void flushServiceEnvelopes();
}

/**
 * The current envelope of every kind, each with its own stored stamp. Kinds not
 * loaded are left out. Never rejects.
 */
export async function currentServiceEnvelopes(): Promise<Uint8Array[]> {
  const out: Uint8Array[] = [];
  for (const kind of KINDS) {
    const compose = composers.get(kind);
    if (compose === undefined) continue;
    try {
      const envelope = await compose();
      if (envelope !== null) out.push(envelope);
    } catch {
      /* not loaded or not composable: nothing to send */
    }
  }
  return out;
}

/** Sign-out: nothing pending survives into the next account. */
export function resetServiceEnvelopeOutbox(): void {
  dirty.clear();
}
