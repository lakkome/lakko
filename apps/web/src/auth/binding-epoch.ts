// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * "Is the session this work was started for still the one that is bound?"
 *
 * The providers under `<AuthProvider>` live for the whole page: they are not
 * unmounted on sign-out, they are told the session ended and clear their
 * state. Nothing cancels a promise, so work one of them started for account A
 * (a bootstrap, a poll, a fetch) keeps running after A signed out, and the
 * code after its `await` writes into whatever the provider holds by then:
 * nothing, or account B's state.
 *
 * This is the generation counter the client already uses for that
 * (`channels/ChannelsHydration.tsx`, the attempt sequences in the recovery
 * panels), given a name so the rule can be tested without a provider: the
 * owner advances it whenever the binding ends, work captures it when it
 * starts and asks after every await.
 *
 * A counter and not the account id alone: signing out and back in to the SAME
 * account is a new binding too, with new handles, and work from the first
 * must not complete into the second.
 */
export class BindingEpoch {
  private epoch = 0;

  /** The binding ended (sign-out, account switch, provider disposed). Every
   *  predicate captured before this call answers `false` from now on. */
  advance(): void {
    this.epoch += 1;
  }

  /** Remember the binding that is current now. The returned predicate says
   *  whether it still is. */
  capture(): () => boolean {
    const mine = this.epoch;
    return () => this.epoch === mine;
  }
}

/** Thrown by work that found its binding gone after an await and has a caller
 *  that must not read the outcome as success. */
export class StaleBindingError extends Error {
  override name = "StaleBindingError";
  constructor() {
    super("the session this work was started for has ended");
  }
}

/**
 * The binding of this browser to an account, as the auth flows see it.
 *
 * Two things end the binding a sign-in round trip started under: a sign-out,
 * and ANOTHER landing (a guest account made by opening a call link, a passkey
 * sign-in to a second account, a restore prompt parked for someone else). A
 * flow that began before either must not land after it: its late response
 * would replace the session the person is in.
 *
 * The two are counted apart because one kind of landing answers only to the
 * first. An account whose registration has finished on the server is landed
 * whatever else landed meanwhile (`captureSignOut`); every other flow asks
 * `capture`.
 */
export class SessionBinding {
  private readonly signOuts = new BindingEpoch();
  private readonly changes = new BindingEpoch();

  /** The person signed out (or this device was forgotten). */
  signedOut(): void {
    this.signOuts.advance();
    this.changes.advance();
  }

  /** An account landed: a session, or a restore / new-phrase prompt for it. */
  landed(): void {
    this.changes.advance();
  }

  /** `false` once a sign-out OR another landing happened after this call. */
  capture(): () => boolean {
    return this.changes.capture();
  }

  /** `false` only once a sign-out happened after this call. */
  captureSignOut(): () => boolean {
    return this.signOuts.capture();
  }
}

/** A flow that ended because its binding did: the person moved on, so the
 *  screen that started it shows no error and runs no success step. */
export function isAbandoned(err: unknown): boolean {
  return err instanceof StaleBindingError;
}
