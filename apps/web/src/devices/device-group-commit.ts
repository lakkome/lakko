// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0081 decision 2 - the DeviceGroup commit outbox.
 *
 * A DeviceGroup commit (add a device by pairing or by the ADR-0049
 * confirmation, remove a device, enable attested removals, and the two
 * commits a rejoin admission makes) is built on a throwaway handle restored
 * from the durable snapshot at epoch N. Before this module the device then
 * registered N+1 on the DS, persisted N+1 and published the commit, and three
 * failures stranded the account: a crash between the registration and the
 * persist (the DS held N+1, every device N, the commit nowhere), a failed
 * publish after the persist (nothing retried it), and a persist failure that
 * was swallowed (the commit published while the device stayed at N).
 *
 * The order now, with the DeviceGroup's current snapshot staying at N until
 * the DS has answered:
 *
 *   1. build the commit on a throwaway handle (the caller);
 *   2. write an entry - the journal row (the ADR-0015 envelope, so the
 *      removal attestation rides in it), the Welcome and its rendezvous, the
 *      N+1 epoch and its access key, the N+1 snapshot - BESIDE the N
 *      snapshot (`device-group-snapshot-store.ts`, `pending`). A failed write
 *      aborts: nothing has left the device;
 *   3. `POST /ds/group/<hex>/commit`: the N+1 registration (signed by the
 *      account's root) and the row in one transaction (decision 1);
 *   4. on success make the entry's N+1 snapshot current (fail-closed), tell
 *      the sync runtime its handle is stale, deliver the Welcome, delete the
 *      entry;
 *   5. on `409` delete the entry: the device stays at N and applies the
 *      winner's commit from the journal;
 *   6. on any other failure, or a restart, the entry is retried
 *      ({@link resendOwedDeviceGroupCommit}). The re-send is idempotent on the
 *      DS, so a crash after the DS answered completes on the retry.
 *
 * An entry is re-sent before any new DeviceGroup commit is built, so two
 * commits of one device never interleave: the callers hold
 * {@link withDeviceGroupCommitLock} across "re-send what is owed, build,
 * run". At most one entry exists per account.
 *
 * ## Why step 4 checks the current epoch
 *
 * A retry may find the N+1 snapshot already current (the earlier run made it
 * current and then failed on the Welcome, or crashed after the write). Writing
 * the entry's snapshot again would then put back an N+1 state the sync
 * runtime has since moved on from - older sender ratchet generations, so this
 * device would seal two messages under one key and nonce. So the snapshot is
 * made current only while the durable state is still below the entry's epoch.
 *
 * Everything that decides is here; the network, the store and the MLS calls
 * are ports, so the host tests drive each failure path.
 */

/** Which DeviceGroup change an entry carries (diagnostics, and what the
 *  caller does after it landed). */
export type DeviceGroupCommitKind = "add" | "remove" | "policy" | "rejoin-remove" | "rejoin-add";

/** One DeviceGroup commit in flight. Stored as JSON, encrypted under the
 *  device key (it carries MLS private state). */
export interface DeviceGroupCommitEntry {
  readonly v: 1;
  readonly kind: DeviceGroupCommitKind;
  /** The DeviceGroup journal id, standard base64. */
  readonly journalIdB64: string;
  /** The epoch the commit moves the DeviceGroup to (decimal `u64`). */
  readonly epoch: string;
  /** That epoch's access verifying key (32 bytes), standard base64. */
  readonly epochKeyB64: string;
  /** The journal row: the ADR-0015 envelope around the commit. */
  readonly rowB64: string;
  /** The Welcome for an added device, and the rendezvous it goes to. */
  readonly welcomeB64?: string;
  readonly welcomeRendezvousB64?: string;
  /** The DeviceIdentity provider snapshot at the commit's epoch. */
  readonly nextSnapshotB64: string;
  /** Leaf indices before an add and this device's own leaf (the growth
   *  re-send after it lands). */
  readonly priorLeaves?: readonly number[];
  readonly ownLeaf?: number;
  readonly createdAtMs: number;
}

/** The proof signer of the epoch the device's DURABLE DeviceGroup state is
 *  at, read from a throwaway handle (structural: the wasm `DsProofSigner`). */
export interface DurableProof {
  readonly epoch: bigint;
  sign(method: string, path: string, query: string, body: Uint8Array): string;
  free(): void;
}

/** What one commit attempt came to. */
export type DeviceGroupCommitOutcome =
  /** The commit is on the journal and its epoch is this device's. */
  | { readonly kind: "landed"; readonly messageId: number }
  /** Another commit owns the epoch: nothing of this one is on the DS, the
   *  entry is gone, the device stays where it was. */
  | { readonly kind: "lost" };

/** The ports the outbox runs through. */
export interface DeviceGroupCommitPorts {
  saveEntry(entry: DeviceGroupCommitEntry): Promise<void>;
  loadEntry(): Promise<DeviceGroupCommitEntry | null>;
  deleteEntry(): Promise<void>;
  /** The proof signer of the durable state's epoch, or `null` when no
   *  durable DeviceGroup state can be loaded. The caller frees it. */
  durableProof(): Promise<DurableProof | null>;
  /** `POST .../commit` with the root's statement for `entry`, proven by
   *  `proof`; throws `CommitConflictError` on a lost epoch. */
  commit(entry: DeviceGroupCommitEntry, proof: DurableProof): Promise<{ readonly id: number }>;
  /** Write the entry's snapshot as the current DeviceGroup state. Throws on
   *  a failed write. */
  makeCurrent(entry: DeviceGroupCommitEntry): Promise<void>;
  /** The DeviceGroup changed through another handle: the sync runtime
   *  reloads (and runs the growth re-send for an add). Never throws. */
  announce(entry: DeviceGroupCommitEntry): void;
  /** Deliver the entry's Welcome to its rendezvous. Throws on failure. */
  deliverWelcome(entry: DeviceGroupCommitEntry): Promise<void>;
  /** `true` when `err` is a lost epoch (the commit route's 409). */
  isConflict(err: unknown): boolean;
}

/**
 * Steps 2-5 for a commit just built: write the entry beside the N snapshot
 * (a failure aborts with nothing sent), then send it. `proof` is the signer of
 * epoch N, taken from the handle the commit was built on BEFORE the create
 * call. Throws on any failure other than a lost epoch; the entry then stays
 * and {@link resendOwedDeviceGroupCommit} finishes it.
 */
export async function runDeviceGroupCommit(
  entry: DeviceGroupCommitEntry,
  proof: DurableProof,
  ports: DeviceGroupCommitPorts,
): Promise<DeviceGroupCommitOutcome> {
  if (proof.epoch + 1n !== BigInt(entry.epoch)) {
    throw new Error("DeviceGroup commit: the proof is not of the epoch the commit leaves");
  }
  await ports.saveEntry(entry);
  return finishEntry(entry, proof, ports);
}

/**
 * Step 6 - finish the entry a previous run left, if any, before anything
 * else touches the DeviceGroup. `null` when there was none. Throws when the
 * DS cannot be reached or a write fails (the entry stays for the next try).
 */
export async function resendOwedDeviceGroupCommit(
  ports: DeviceGroupCommitPorts,
): Promise<DeviceGroupCommitOutcome | null> {
  const entry = await ports.loadEntry();
  if (entry === null) return null;
  const proof = await ports.durableProof();
  if (proof === null) {
    // No durable DeviceGroup to prove from: the state this entry was built
    // on is gone (deleted for a rejoin). Nothing can make it current; drop it.
    await ports.deleteEntry();
    return { kind: "lost" };
  }
  try {
    return await finishEntry(entry, proof, ports);
  } finally {
    proof.free();
  }
}

async function finishEntry(
  entry: DeviceGroupCommitEntry,
  proof: DurableProof,
  ports: DeviceGroupCommitPorts,
): Promise<DeviceGroupCommitOutcome> {
  let answer: { readonly id: number };
  try {
    answer = await ports.commit(entry, proof);
  } catch (err) {
    if (!ports.isConflict(err)) throw err;
    // Step 5: another commit owns the epoch. Whether or not this entry's
    // snapshot was ever made current, deleting it is right: below the epoch
    // the device applies the winner's commit; at or above it the entry's work
    // is already durable.
    await ports.deleteEntry();
    return { kind: "lost" };
  }
  // Step 4: the commit is on the journal. Make its epoch this device's,
  // unless a previous run already did (see the module doc).
  if (proof.epoch < BigInt(entry.epoch)) {
    await ports.makeCurrent(entry);
  }
  ports.announce(entry);
  if (entry.welcomeB64 !== undefined && entry.welcomeRendezvousB64 !== undefined) {
    await ports.deliverWelcome(entry);
  }
  await ports.deleteEntry();
  return { kind: "landed", messageId: answer.id };
}

// ----------------------------------------------------------------------------
// Serialisation of the device's DeviceGroup commits
// ----------------------------------------------------------------------------

const chains = new Map<string, Promise<unknown>>();

/**
 * Run `body` after every DeviceGroup commit of `accountIdB64` started before
 * it (serializing, never coalescing). The commit paths hold it across "re-send
 * what is owed, build, run", so a second commit is never built on an epoch a
 * first one is still taking.
 */
export async function withDeviceGroupCommitLock<T>(
  accountIdB64: string,
  body: () => Promise<T>,
): Promise<T> {
  const prior = chains.get(accountIdB64) ?? Promise.resolve();
  let release: () => void = () => undefined;
  const mine = new Promise<void>((resolve) => {
    release = resolve;
  });
  const tail = prior.then(() => mine);
  chains.set(accountIdB64, tail);
  try {
    await prior.catch(() => undefined);
    return await body();
  } finally {
    release();
    if (chains.get(accountIdB64) === tail) chains.delete(accountIdB64);
  }
}

// ----------------------------------------------------------------------------
// Codec
// ----------------------------------------------------------------------------

const KINDS: readonly DeviceGroupCommitKind[] = ["add", "remove", "policy", "rejoin-remove", "rejoin-add"];

/** Encode an entry for the store. */
export function encodeDeviceGroupCommitEntry(entry: DeviceGroupCommitEntry): Uint8Array {
  return new TextEncoder().encode(JSON.stringify(entry));
}

/** Decode a stored entry, or `null` when it is not one this build wrote (a
 *  row it cannot read is treated as absent and dropped by the caller). */
export function decodeDeviceGroupCommitEntry(bytes: Uint8Array): DeviceGroupCommitEntry | null {
  let v: unknown;
  try {
    v = JSON.parse(new TextDecoder().decode(bytes));
  } catch {
    return null;
  }
  if (v === null || typeof v !== "object") return null;
  const e = v as Record<string, unknown>;
  const str = (k: string): boolean => typeof e[k] === "string";
  if (e["v"] !== 1 || !KINDS.includes(e["kind"] as DeviceGroupCommitKind)) return null;
  if (!str("journalIdB64") || !str("epoch") || !str("epochKeyB64") || !str("rowB64")) return null;
  if (!str("nextSnapshotB64") || typeof e["createdAtMs"] !== "number") return null;
  if (!/^[0-9]+$/.test(e["epoch"] as string)) return null;
  if ((e["welcomeB64"] === undefined) !== (e["welcomeRendezvousB64"] === undefined)) return null;
  if (e["welcomeB64"] !== undefined && (!str("welcomeB64") || !str("welcomeRendezvousB64"))) return null;
  const prior = e["priorLeaves"];
  if (prior !== undefined && (!Array.isArray(prior) || !prior.every((n) => Number.isInteger(n)))) {
    return null;
  }
  if (e["ownLeaf"] !== undefined && !Number.isInteger(e["ownLeaf"])) return null;
  return v as DeviceGroupCommitEntry;
}
