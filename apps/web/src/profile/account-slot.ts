// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * One fetched value that belongs to the signed-in account, held by a provider
 * that outlives a sign-out.
 *
 * "Fetch once, keep" is wrong by itself in such a provider: the value kept is
 * the FIRST account's, and the early return that makes the fetch happen once
 * also keeps the second account from ever fetching its own. The own username
 * certificate was held that way, so after sign-out and sign-in the second
 * account's profile envelopes carried the first account's certificate.
 *
 * The slot is bound to an account. Changing the account empties it and
 * disowns the fetch in flight; a fetch that returns for an account that is no
 * longer the bound one stores nothing.
 */
export class AccountSlot<T> {
  private account: string | null = null;
  private value: T | null = null;
  private inFlight: Promise<void> | null = null;
  /** Bumped on every change of account; a fetch stores only under the
   *  generation it started in. */
  private generation = 0;

  /** `publish` receives every change of the held value (a reactive setter). */
  constructor(private readonly publish: (value: T | null) => void) {}

  /** The account changed (or there is none). A no-op for the same account. */
  rebind(account: string | null): void {
    if (account === this.account) return;
    this.account = account;
    this.generation += 1;
    this.inFlight = null;
    if (this.value !== null) {
      this.value = null;
      this.publish(null);
    }
  }

  /** The value held for the bound account, or `null`. */
  get(): T | null {
    return this.value;
  }

  /** The held value went stale for the SAME account (its username changed):
   *  drop it, and let a fetch that is still out fill nothing. */
  reset(): void {
    this.generation += 1;
    this.inFlight = null;
    if (this.value !== null) {
      this.value = null;
      this.publish(null);
    }
  }

  /** Hold `value` for `account`: it arrived with a write this session made
   *  (the register call of a username claim). Nothing is held for `null`. */
  put(account: string | null, value: T): void {
    this.rebind(account);
    if (account === null) return;
    this.generation += 1;
    this.inFlight = null;
    this.value = value;
    this.publish(value);
  }

  /**
   * Make sure `account`'s value is held, fetching it when it is not. One
   * fetch at a time per account; a `null` result or a rejection leaves the
   * slot empty, so the next call tries again. Rejects with the fetch's error.
   */
  ensure(account: string | null, fetch: () => Promise<T | null>): Promise<void> {
    this.rebind(account);
    if (account === null || this.value !== null) return Promise.resolve();
    if (this.inFlight !== null) return this.inFlight;
    const mine = this.generation;
    const run = fetch()
      .then((fetched) => {
        if (mine !== this.generation || fetched === null) return;
        this.value = fetched;
        this.publish(fetched);
      })
      .finally(() => {
        if (mine === this.generation) this.inFlight = null;
      });
    this.inFlight = run;
    return run;
  }
}
