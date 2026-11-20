// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The two security-key ceremonies: signing in (with the PRF extension, which
 * gives the report secret) and signing one decision's statement.
 */

import { api } from "./api/client";
import { b64ToBytes, bytesToB64, toBuffer } from "./model/bytes";
import { moderatorForCredential, prfOutput } from "./model/webauthn-rules";
import { type PanelSession } from "./session";
import { loadWasm } from "./wasm";

interface Assertion {
  readonly rawId: Uint8Array;
  readonly authenticatorData: Uint8Array;
  readonly clientDataJson: Uint8Array;
  readonly signature: Uint8Array;
  readonly extensions: unknown;
}

async function getAssertion(options: PublicKeyCredentialRequestOptions): Promise<Assertion> {
  if (typeof navigator === "undefined" || navigator.credentials === undefined || typeof PublicKeyCredential === "undefined") {
    throw new Error("This browser can't use security keys. Open the panel in a current browser.");
  }
  const cred = await navigator.credentials.get({ publicKey: options });
  if (!(cred instanceof PublicKeyCredential)) throw new Error("The security key gave no answer. Try again.");
  const response = cred.response;
  if (!(response instanceof AuthenticatorAssertionResponse)) {
    throw new Error("The security key gave an answer the panel can't use. Try again.");
  }
  return {
    rawId: new Uint8Array(cred.rawId),
    authenticatorData: new Uint8Array(response.authenticatorData),
    clientDataJson: new Uint8Array(response.clientDataJSON),
    signature: new Uint8Array(response.signature),
    extensions: cred.getClientExtensionResults(),
  };
}

/** Sign in with one touch of the security key. Throws a readable error. */
export async function signInWithKey(): Promise<PanelSession> {
  const wasm = await loadWasm();
  const start = await api.sessionStart();
  if (start.credentials.length === 0) throw new Error("No moderator is listed on this server.");
  const assertion = await getAssertion({
    challenge: toBuffer(b64ToBytes(start.challenge_b64)),
    rpId: start.rp_id,
    allowCredentials: start.credentials.map((c) => ({
      type: "public-key" as const,
      id: toBuffer(b64ToBytes(c.credential_id_b64)),
    })),
    userVerification: "required",
    timeout: 120_000,
    extensions: { prf: { eval: { first: toBuffer(wasm.moderatorPrfInput()) } } } as AuthenticationExtensionsClientInputs,
  });
  const moderatorIdB64 = moderatorForCredential(assertion.rawId, start.credentials);
  if (moderatorIdB64 === null) throw new Error("This security key isn't on the moderator list.");
  const secret = prfOutput(assertion.extensions);
  if (secret === null) {
    throw new Error(
      "This security key can't open reports: it didn't return the report key. Sign in with a key that supports it.",
    );
  }
  const finish = await api.sessionFinish({
    moderator_id_b64: moderatorIdB64,
    authenticator_data_b64: bytesToB64(assertion.authenticatorData),
    client_data_json_b64: bytesToB64(assertion.clientDataJson),
    signature_b64: bytesToB64(assertion.signature),
  });
  return {
    token: finish.token,
    expiresAtMs: finish.expires_at_ms,
    moderatorId: b64ToBytes(finish.moderator_id_b64),
    moderatorIdB64: finish.moderator_id_b64,
    name: finish.name,
    reportSecret: secret,
    rpId: start.rp_id,
    credentialId: assertion.rawId,
  };
}

/** Sign one statement's challenge with the signed-in moderator's key. */
export async function signStatementChallenge(
  s: PanelSession,
  challenge: Uint8Array,
): Promise<{ authenticatorData: Uint8Array; clientDataJson: Uint8Array; signature: Uint8Array }> {
  const a = await getAssertion({
    challenge: toBuffer(challenge),
    rpId: s.rpId,
    allowCredentials: [{ type: "public-key", id: toBuffer(s.credentialId) }],
    userVerification: "required",
    timeout: 120_000,
  });
  return { authenticatorData: a.authenticatorData, clientDataJson: a.clientDataJson, signature: a.signature };
}
