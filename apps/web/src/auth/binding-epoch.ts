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
