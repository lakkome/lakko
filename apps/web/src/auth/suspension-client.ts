// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The two routes of auth that exist for a suspended account (ADR-0098
 * section 4): `GET /account/suspension` and `POST /account/suspension/appeal`.
 * Both take the restricted session as their bearer; they go through the
 * session-aware fetch, which sends the live restricted token (refreshed ahead
 * of its expiry) or, once the suspension has ended, the new access token.
 */

import { base64Encode } from "./account-id";
import { AuthNetworkError, DEFAULT_BASE_URL } from "./client";
import { errorCodeOf } from "./session-401";
import { sessionFetch } from "./session-fetch";
import { suspensionStatusOf, type SuspensionStatus } from "./suspension";

/** `GET /account/suspension`. `null` for a refusal or a body of the wrong
 *  shape; throws {@link AuthNetworkError} when there is no answer. */
export async function fetchSuspensionStatus(
  bearer: string,
  base: string = DEFAULT_BASE_URL,
): Promise<SuspensionStatus | null> {
  const endpoint = "/account/suspension";
  let response: Response;
  try {
    response = await sessionFetch(`${base}${endpoint}`, {
      method: "GET",
      headers: { authorization: `Bearer ${bearer}` },
    });
  } catch (err) {
    throw new AuthNetworkError(endpoint, err);
  }
  if (!response.ok) return null;
  try {
    return suspensionStatusOf(await response.json());
  } catch {
    return null;
  }
}

/** `POST /account/suspension/appeal` with the sealed appeal: the status and
 *  the `error` code. Throws {@link AuthNetworkError} when there is no
 *  answer. */
export async function postSuspensionAppeal(
  bearer: string,
  sealed: Uint8Array,
  base: string = DEFAULT_BASE_URL,
): Promise<{ readonly status: number; readonly errorCode: string | null }> {
  const endpoint = "/account/suspension/appeal";
  let response: Response;
  try {
    response = await sessionFetch(`${base}${endpoint}`, {
      method: "POST",
      headers: { authorization: `Bearer ${bearer}`, "content-type": "application/json" },
      body: JSON.stringify({ sealed_b64: base64Encode(sealed) }),
    });
  } catch (err) {
    throw new AuthNetworkError(endpoint, err);
  }
  if (response.ok) return { status: response.status, errorCode: null };
  let text: string | null = null;
  try {
    text = await response.text();
  } catch {
    text = null;
  }
  return { status: response.status, errorCode: errorCodeOf(text) };
}
