// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The state behind the shared proof block (docs/ux/identity-recovery.md
 * section 17.1): one controller per surface (the reset panel, the delete
 * dialog, "Is this you?", the scan confirm, add a passkey, change
 * password). Every decision it shows is `proofBlockView` /
 * `proofFailureOf` / `proofFailureLine` in `fresh-auth.ts`, which are pure
 * and tested; this file only wires them to signals and to `AuthContext`.
 *
 * The typed password lives in this controller's signal and nowhere else: it
 * is cleared when a proof succeeds and when the owning surface goes away.
 */
import { createResource, createSignal, onCleanup, type Accessor } from "solid-js";

import type { AuthApi } from "./AuthContext";
import { AuthHttpError, AuthNetworkError } from "./client";
import {
  errorCodeOfBody,
  FreshAuthRequiredError,
  isFreshAuthRequired,
  ProofCancelledError,
  proofBlockView,
  proofFailureLine,
  proofFailureOf,
  ProofOtherAccountError,
  ProofPasskeyFailedError,
  ProofWrongPasswordError,
  type ProofBlockView,
  type ProofCopy,
  type ProofFailure,
  type ProofPhase,
} from "./fresh-auth";
import { isOffline, screenText } from "./auth-error-copy";
import { t } from "../l10n";
import { createRetryCountdown, type RetryCountdown } from "./recover/retry-countdown";
import { WebauthnClient, WebauthnHttpError, WebauthnNetworkError } from "./webauthn-client";

/** Read the facts of an error and classify it (17.1's shared states). */
export function proofFailureFromError(err: unknown): ProofFailure {
  let status: number | null = null;
  let retryAfterSecs: number | null = null;
  let errorCode: string | null = null;
  if (err instanceof AuthHttpError) {
    status = err.status;
    retryAfterSecs = err.retryAfter;
    errorCode = err.errorCode;
  } else if (err instanceof WebauthnHttpError) {
    status = err.status;
    errorCode = errorCodeOfBody(err.body);
    retryAfterSecs = retryAfterOfBody(err.body);
  }
  return proofFailureOf({
    required: err instanceof FreshAuthRequiredError || isFreshAuthRequired({ status, errorCode }),
    wrongPassword: err instanceof ProofWrongPasswordError,
    cancelled: err instanceof ProofCancelledError,
    otherAccount: err instanceof ProofOtherAccountError,
    passkeyFailed: err instanceof ProofPasskeyFailedError,
    status,
    retryAfterSecs,
    network:
      err instanceof AuthNetworkError || err instanceof WebauthnNetworkError || isOffline(err),
  });
}

function retryAfterOfBody(body: string): number | null {
  try {
    const secs = (JSON.parse(body) as { retry_after_secs?: unknown }).retry_after_secs;
    return typeof secs === "number" && Number.isFinite(secs) ? Math.max(0, Math.ceil(secs)) : null;
  } catch {
    return null;
  }
}

export interface ProofGateOptions {
  readonly auth: AuthApi;
  readonly copy: ProofCopy;
  /** The surface leads with the passkey when one is usable (17.4). */
  readonly passkeyFirst?: boolean;
  /** Whether the surface offers a passkey route at all (the delete dialog
   *  of an account without a passkey does not). Default `true`. */
  readonly offerPasskey?: boolean;
  /** Keep the block folded away until the main button is first pressed
   *  without a proof (see `ProofBlockInput.collapsed`). */
  readonly collapsed?: boolean;
  /** The surface's own action is running. */
  readonly busy?: Accessor<boolean>;
  /** The bearer to list the account's passkeys with; `null` while there is
   *  no session. */
  readonly sessionToken: Accessor<string | null>;
  /** The account's username is known on this device. */
  readonly usernameKnown: Accessor<boolean>;
}

export interface ProofLine {
  readonly text: () => string;
  readonly error: boolean;
}

export interface ProofGate {
  readonly view: Accessor<ProofBlockView>;
  readonly copy: ProofCopy;
  readonly password: Accessor<string>;
  setPassword(value: string): void;
  readonly username: Accessor<string>;
  setUsername(value: string): void;
  /** The one line under the field: its text (rendered at display time, so
   *  it follows a language switch) and whether it is an error. */
  readonly line: Accessor<ProofLine | null>;
  readonly wait: RetryCountdown;
  readonly phase: Accessor<ProofPhase>;
  readonly proofLive: Accessor<boolean>;
  /** `true` / `false` once known; `false` while loading (the passkey
   *  button is absent rather than flickering in). */
  readonly passkeyUsable: Accessor<boolean>;
  /** Whether the account has any passkey (`null` while unknown). */
  readonly hasPasskey: Accessor<boolean | null>;
  /**
   * Make sure a proof is live, by the route the block shows. Resolves the
   * grant, or `null` when the proof did not succeed (the line, or the
   * countdown, already says why; `failure` names it for the surface).
   */
  ensure(): Promise<string | null>;
  /** The ghost button `Use a passkey instead`: obtain the proof by passkey
   *  now. Resolves whether a proof is live afterwards. */
  usePasskey(): Promise<boolean>;
  /** `Use your password instead` on a passkey-first surface. */
  usePassword(): void;
  /**
   * A gated request failed. When it was `403 fresh-auth-required`, drop the
   * grant and show the block with no error (nothing went wrong); a limit
   * starts the countdown; offline shows the shared line. Returns the
   * failure so the surface words anything else.
   */
  failed(err: unknown, grant: string | null): ProofFailure;
  /** The last failure `ensure` / `usePasskey` met, or `null`. */
  readonly lastFailure: Accessor<ProofFailure | null>;
  /** Forget the typed password and the line (the surface closed). */
  clear(): void;
}

export function createProofGate(options: ProofGateOptions): ProofGate {
  const { auth, copy } = options;
  const [password, setPasswordSignal] = createSignal("");
  const [username, setUsername] = createSignal("");
  const [line, setLine] = createSignal<ProofLine | null>(null);
  const [ownPhase, setOwnPhase] = createSignal<ProofPhase>("idle");
  const [passwordChosen, setPasswordChosen] = createSignal(false);
  const [lastFailure, setLastFailure] = createSignal<ProofFailure | null>(null);
  const wait = createRetryCountdown();

  // Whether a proof is live depends on the clock, so it is polled: the
  // block appears on its own when the ten minutes run out.
  const [proofLive, setProofLive] = createSignal(auth.freshProofLive());
  const collapsed = options.collapsed === true;
  const [revealed, setRevealed] = createSignal(!collapsed && !proofLive());
  const poll = setInterval(() => {
    const live = auth.freshProofLive();
    setProofLive(live);
    if (!live && !collapsed) setRevealed(true);
  }, 1000);

  const passkeysSupported = WebauthnClient.isWebauthnAvailable();
  const [passkeyCount] = createResource(
    () => options.sessionToken(),
    async (token): Promise<number | null> => {
      try {
        const c = new WebauthnClient();
        c.setSessionToken(token);
        return (await c.listCredentials()).credentials.length;
      } catch {
        // Unknown: the password route still works.
        return null;
      }
    },
  );
  const hasPasskey = (): boolean | null => {
    const n = passkeyCount();
    return n === undefined || n === null ? null : n > 0;
  };
  const passkeyUsable = (): boolean =>
    options.offerPasskey !== false && passkeysSupported && hasPasskey() === true;

  const phase = (): ProofPhase => {
    const own = ownPhase();
    if (own !== "idle") return own;
    return options.busy?.() === true ? "checking" : "idle";
  };

  const view = (): ProofBlockView =>
    proofBlockView({
      proofLive: proofLive(),
      revealed: revealed(),
      collapsed,
      passkeyUsable: passkeyUsable(),
      passkeyFirst: options.passkeyFirst === true,
      passwordChosen: passwordChosen(),
      usernameKnown: options.usernameKnown(),
      password: password(),
      username: username(),
      phase: phase(),
      waiting: wait.waiting(),
    });

  function clear(): void {
    setPasswordSignal("");
    setLine(null);
    setLastFailure(null);
  }
  onCleanup(() => {
    clearInterval(poll);
    setPasswordSignal("");
  });

  function show(failure: ProofFailure): void {
    setLastFailure(failure);
    if (failure.kind === "rate-limited" && failure.retryAfterSecs !== null) {
      wait.startFor(failure.retryAfterSecs);
    }
    const worded = proofFailureLine(failure, copy);
    setLine(worded === null ? null : { text: () => t(worded.key), error: worded.error });
  }

  function succeeded(): string | null {
    setPasswordSignal("");
    setLine(null);
    setLastFailure(null);
    setProofLive(true);
    // A completed proof hides the block again.
    setRevealed(false);
    return auth.freshProof();
  }

  async function run(route: "password" | "passkey"): Promise<string | null> {
    setLine(null);
    setLastFailure(null);
    setOwnPhase(route === "passkey" ? "passkey" : "checking");
    try {
      if (route === "passkey") await auth.proveWithPasskey();
      else await auth.proveWithPassword(password(), username());
      return succeeded();
    } catch (err) {
      const failure = proofFailureFromError(err);
      show(failure);
      if (failure.kind === "other") {
        // A sentence written for a screen (the username rule, a keyed
        // error) is shown as it is; anything else is the surface's line.
        const own = screenText(err);
        if (own !== null) setLine({ text: () => own, error: true });
        else {
          // eslint-disable-next-line no-console
          console.warn("proof failed", err instanceof Error ? err.name : "unknown");
        }
      }
      return null;
    } finally {
      setOwnPhase("idle");
    }
  }

  return {
    view,
    copy,
    password,
    setPassword(value) {
      setLine(null);
      setPasswordSignal(value);
    },
    username,
    setUsername,
    line,
    wait,
    phase,
    proofLive,
    passkeyUsable,
    hasPasskey,
    lastFailure,
    async ensure() {
      const live = auth.freshProof();
      if (live !== null) {
        setProofLive(true);
        return live;
      }
      setProofLive(false);
      const wasShown = view().visible;
      setRevealed(true);
      if (!wasShown) {
        // The block was folded away (or the proof ran out while the surface
        // was open): it appears, with no error styling.
        setLastFailure({ kind: "required" });
        return null;
      }
      const route = view().route;
      if (route === "passkey") return run("passkey");
      if (password().length === 0) {
        // The proof ran out while the surface was open: the field simply
        // appears, with no error styling.
        setLastFailure({ kind: "required" });
        return null;
      }
      return run("password");
    },
    async usePasskey() {
      return (await run("passkey")) !== null;
    },
    usePassword() {
      setLine(null);
      setPasswordChosen(true);
    },
    failed(err, grant) {
      const failure = proofFailureFromError(err);
      if (failure.kind === "required") {
        if (grant !== null) auth.dropFreshProof(grant);
        setProofLive(auth.freshProofLive());
        setRevealed(true);
        setLastFailure(failure);
        setLine(null);
        return failure;
      }
      if (failure.kind === "rate-limited" || failure.kind === "offline") show(failure);
      return failure;
    },
    clear,
  };
}
