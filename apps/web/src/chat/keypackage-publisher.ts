// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Keep exactly one USABLE KeyPackage published for this account.
 *
 * ## Why this exists
 *
 * A KeyPackage is single-use on the receiving side. OpenMLS deletes the
 * KeyPackage's private init key when it processes a Welcome built on it
 * (unless the package is marked last-resort, which Lakko never does), so
 * after this device joins ONE group the KeyPackage the directory still serves
 * is dead: the next peer that fetches it and sends a Welcome gets a Welcome
 * this device can no longer open. Discovery stores one row per account and
 * fetching does not consume it, so nothing server-side notices either. Before
 * this module the web client only uploaded a KeyPackage at signup / login /
 * recover, so an identity that had joined one group could not be added to a
 * second until the user logged in again — and the failure was a
 * `console.warn`.
 *
 * ## The order, and why it is fail-closed
 *
 * 1. **Create** the KeyPackage from the canonical identity. This writes its
 *    private init key into the in-memory provider.
 * 2. **Persist** the provider snapshot durably. A throw stops here.
 * 3. **Upload** the public KeyPackage.
 *
 * The previous order (upload, then a best-effort persist) could publish a
 * KeyPackage whose private key existed only in memory: a reload before the
 * persist landed, or a persist that failed silently, left the directory
 * serving a package no device can open. Persisting first means a published
 * package always has its private key on disk. A persist failure therefore
 * aborts the upload — the old (possibly dead) package stays published, which
 * is the same failure the user had before, never a new one.
 *
 * ## Single flight
 *
 * Every Welcome attempt requests a republish, and Welcomes can arrive in a
 * burst. Requests that arrive while a publish is running collapse into ONE
 * trailing publish that starts after it finishes — not zero, because the
 * running publish may be uploading the very package a just-processed Welcome
 * consumed, and not N, because every publish costs a proof-of-work solve.
 */

export interface KeyPackagePublisherDeps<Ctx> {
  /** Capture everything one publish needs (account, canonical identity) at
   *  its start, so a session change mid-publish cannot mix two accounts.
   *  Throws when there is no session to publish for. */
  readonly resolveContext: () => Ctx;
  /** Mint a fresh KeyPackage (writes its private key into the provider). */
  readonly createKeyPackage: (ctx: Ctx) => Uint8Array;
  /** Durable, FAIL-CLOSED snapshot persist. Must throw rather than no-op. */
  readonly persistDurable: (ctx: Ctx) => Promise<void>;
  /** Publish the public KeyPackage to the directory. */
  readonly upload: (ctx: Ctx, keyPackage: Uint8Array) => Promise<void>;
}

export interface KeyPackagePublisher {
  /** Request a publish. Resolves when a publish that started at or after this
   *  request finished; rejects with that publish's error. */
  publish(): Promise<void>;
}

export function createKeyPackagePublisher<Ctx>(
  deps: KeyPackagePublisherDeps<Ctx>,
): KeyPackagePublisher {
  let running: Promise<void> | null = null;
  let trailing: Promise<void> | null = null;

  async function publishOnce(): Promise<void> {
    const ctx = deps.resolveContext();
    const keyPackage = deps.createKeyPackage(ctx);
    // Fail closed: no durable private key, no upload.
    await deps.persistDurable(ctx);
    await deps.upload(ctx, keyPackage);
  }

  function publish(): Promise<void> {
    if (running === null) {
      const run = publishOnce().finally(() => {
        if (running === run) running = null;
      });
      running = run;
      return run;
    }
    if (trailing === null) {
      const after = running;
      trailing = after
        .catch(() => undefined)
        .then(() => {
          trailing = null;
          // A request that landed in the microtask gap between the running
          // publish settling and this callback already started a fresh one;
          // it began after every request this trailing slot stands for, so
          // join it instead of starting a third.
          return running ?? publish();
        });
    }
    return trailing;
  }

  return { publish };
}
