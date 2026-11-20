// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Signing and sending one decision: build the statement in the browser, ask
 * the security key to sign its challenge, encode, send. The body is kept so a
 * decision that was logged but not applied can be sent again unchanged.
 */

import { api } from "./api/client";
import { ApiError, coreErrorMessage } from "./api/errors";
import { canResend, decideBody, decideOutcome, statementInputs, type DecideOutcome, type DecisionOption } from "./model/decisions";
import { requireSession } from "./session";
import { loadWasm } from "./wasm";
import { signStatementChallenge } from "./webauthn";

export interface DecisionResult {
  readonly outcome: DecideOutcome;
  /** Send this again to retry applying the same signed statement. */
  readonly body: Record<string, unknown>;
  /** When the statement was signed: it can be sent again for 24 hours. */
  readonly issuedAtMs: number;
}

function random16(): Uint8Array {
  const b = new Uint8Array(16);
  crypto.getRandomValues(b);
  return b;
}

export async function signAndDecide(args: {
  readonly option: DecisionOption;
  readonly reasonCode: number;
  readonly note: string;
}): Promise<DecisionResult> {
  const s = requireSession();
  const wasm = await loadWasm();
  // The current list version: a statement names the list it is signed under.
  const list = await api.moderators();
  const nowMs = Date.now();
  const inputs = statementInputs({
    option: args.option,
    reasonCode: args.reasonCode,
    nowMs,
    moderatorId: s.moderatorId,
    listVersion: list.list_version,
    statementId: random16(),
    reasonSalt: random16(),
  });
  let bytes: Uint8Array;
  let challenge: Uint8Array;
  try {
    const statement = wasm.buildModerationStatement(
      inputs.statementId,
      inputs.action,
      inputs.targetKind,
      inputs.target,
      inputs.reason,
      inputs.reasonSalt,
      inputs.untilMs,
      inputs.issuedAtMs,
      inputs.moderatorId,
      inputs.listVersion,
    );
    bytes = statement.bytes;
    challenge = statement.challenge;
    statement.free();
  } catch (e) {
    throw new Error(coreErrorMessage(e, "statement"));
  }
  const assertion = await signStatementChallenge(s, challenge);
  let signed: Uint8Array;
  try {
    signed = wasm.encodeSignedStatement(bytes, assertion.authenticatorData, assertion.clientDataJson, assertion.signature);
  } catch (e) {
    throw new Error(coreErrorMessage(e, "statement"));
  }
  const body = decideBody({
    signedStatement: signed,
    reasonCode: args.reasonCode,
    reasonSalt: inputs.reasonSalt,
    note: args.note,
    option: args.option,
  });
  return resend(body, nowMs);
}

/**
 * Send a decide body (first time or again). Every service refuses a
 * statement more than 24 hours old, so a resend after that is refused here
 * without a request.
 */
export async function resend(body: Record<string, unknown>, issuedAtMs: number): Promise<DecisionResult> {
  if (!canResend(issuedAtMs, Date.now())) throw new ApiError(400, "stale-statement", null);
  const { status, answer } = await api.decide(body);
  return { outcome: decideOutcome(status, answer), body, issuedAtMs };
}
