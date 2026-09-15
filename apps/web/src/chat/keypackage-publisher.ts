// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Keep exactly one USABLE KeyPackage published for this account.
 *
 * ## Why this exists
 *
 * Until ADR-0050 a KeyPackage was single-use on the receiving side: OpenMLS
 * deletes the package's private init key when it processes a Welcome built on
 * it, so after this device joined ONE group the package the directory still
 * served was dead. Since ADR-0050 section 2.6 the published package is a
 * LAST-RESORT package (the core marks it), so every Welcome sealed to it
 * opens — that is what lets one device be admitted to N chats from one fetch
 * (the pairing fan-out, a contact's re-admission). Republishing after each
 * consumed Welcome stays: it is now the forward-secrecy rotation rather than
 * a liveness fix, and it keeps the directory serving a fresh key.
 * Discovery stores one row per device and fetching does not consume it.
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
