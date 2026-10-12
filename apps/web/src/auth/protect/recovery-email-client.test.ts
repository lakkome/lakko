// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { AuthHttpError, AuthNetworkError } from "../client";
import { answerOf, RecoveryEmailClient } from "./recovery-email-client";

interface Call {
  readonly url: string;
  readonly method: string;
  readonly headers: Record<string, string>;
  readonly body: unknown;
}

/** A client over a scripted transport: no request leaves the process. */
function client(answer: (call: Call) => Response | Error): { client: RecoveryEmailClient; calls: Call[] } {
  const calls: Call[] = [];
  const fetchImpl = async (url: string, init?: RequestInit): Promise<Response> => {
    const call: Call = {
      url,
      method: init?.method ?? "GET",
      headers: (init?.headers ?? {}) as Record<string, string>,
      body: typeof init?.body === "string" ? (JSON.parse(init.body) as unknown) : null,
    };
    calls.push(call);
    const out = answer(call);
    if (out instanceof Error) throw out;
    return out;
  };
  return { client: new RecoveryEmailClient("/api/auth", fetchImpl), calls };
}
const json = (body: unknown, status = 200, headers: Record<string, string> = {}): Response =>
  new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json", ...headers } });

const HANDLE = "h".repeat(43);

describe("the recovery-email routes", () => {
  it("GET /recovery-email carries the bearer and reads the four facts", async () => {
    const c = client(() => json({ available: true, enabled: true, vault: true, phrase_saved: false }));
    expect(await c.client.status("tok")).toEqual({
      available: true,
      enabled: true,
      vault: true,
      phraseSaved: false,
      totp: false,
    });
    expect(c.calls[0]).toMatchObject({
      url: "/api/auth/recovery-email",
      method: "GET",
      headers: { authorization: "Bearer tok" },
    });
  });

  it("the availability probe is anonymous", async () => {
    const c = client(() => json({ available: false }));
    expect(await c.client.available()).toBe(false);
    expect(c.calls[0]?.headers["authorization"]).toBeUndefined();
  });

  it("enrol/start sends the grant, the handle, the address and the language", async () => {
    const c = client(() =>
      json({ challenge_id: "cid", vault_salt: "c2FsdA==", resend_after_secs: 45, expires_in_secs: 600 }),
    );
    const sent = await c.client.enrolStart({
      sessionToken: "tok",
      freshGrant: "k1.grant",
      handle: HANDLE,
      address: "name@example.com",
      lang: "ru",
    });
    expect(sent).toEqual({ challengeId: "cid", vaultSalt: "c2FsdA==", resendAfterSecs: 45, expiresInSecs: 600 });
    expect(c.calls[0]).toMatchObject({
      url: "/api/auth/recovery-email/enrol/start",
      method: "POST",
      headers: { authorization: "Bearer tok" },
      body: { fresh_grant: "k1.grant", handle: HANDLE, address: "name@example.com", lang: "ru" },
    });
  });

  it("enrol/finish sends the vault only when there is one", async () => {
    const c = client(() => json({ enabled: true, vault: true }));
    await c.client.enrolFinish({ sessionToken: "tok", challengeId: "cid", code: "123456", sealedEntropy: "AAAA" });
    await c.client.enrolFinish({ sessionToken: "tok", challengeId: "cid", code: "123456", sealedEntropy: null });
    expect(c.calls[0]?.body).toEqual({ challenge_id: "cid", code: "123456", sealed_entropy: "AAAA" });
    expect(c.calls[1]?.body).toEqual({ challenge_id: "cid", code: "123456" });
  });

  it("DELETE /recovery-email carries the grant in its body", async () => {
    const c = client(() => json({ removed: true }));
    await c.client.remove("tok", "k1.grant");
    expect(c.calls[0]).toMatchObject({
      url: "/api/auth/recovery-email",
      method: "DELETE",
      body: { fresh_grant: "k1.grant" },
    });
  });

  it("the recover routes are anonymous and carry what the server names", async () => {
    const c = client((call) => {
      if (call.url.endsWith("/recover/start")) {
        return json({ challenge_id: "cid", resend_after_secs: 45, expires_in_secs: 600 });
      }
      if (call.url.endsWith("/recover/verify")) {
        return json({ ticket: "tkt", totp_required: true, expires_in_secs: 600 });
      }
      if (call.url.endsWith("/recover/totp")) return json({ ok: true });
      return json({ mode: "vault", vault_salt: "c2FsdA==", sealed_entropy: "c2VhbGVk" });
    });
    expect(await c.client.recoverStart(HANDLE, "name@example.com", "en")).toEqual({
      challengeId: "cid",
      resendAfterSecs: 45,
      expiresInSecs: 600,
    });
    expect(await c.client.recoverVerify("cid", "123456")).toEqual({ ticket: "tkt", totpRequired: true });
    await c.client.recoverTotp("tkt", "654321");
    expect(await c.client.recoverOpen("tkt")).toEqual({
      mode: "vault",
      vaultSalt: "c2FsdA==",
      sealedEntropy: "c2VhbGVk",
    });
    expect(c.calls.map((x) => x.body)).toEqual([
      { handle: HANDLE, address: "name@example.com", lang: "en" },
      { challenge_id: "cid", code: "123456" },
      { ticket: "tkt", code: "654321" },
      { ticket: "tkt" },
    ]);
    for (const call of c.calls) expect(call.headers["authorization"]).toBeUndefined();
  });

  it("open answers the password-only mode with nothing else", async () => {
    const c = client(() => json({ mode: "password" }));
    expect(await c.client.recoverOpen("tkt")).toEqual({ mode: "password" });
  });

  it("an answer that is not one of the two modes is refused", async () => {
    const c = client(() => json({ mode: "something" }));
    await expect(c.client.recoverOpen("tkt")).rejects.toBeInstanceOf(AuthHttpError);
  });

  it("GET /account/notices asks from the cursor", async () => {
    const c = client(() =>
      json({ notices: [{ id: 8, kind: "recovery-email-added", at_ms: 5 }], next_after: 8, more: false }),
    );
    const page = await c.client.notices("tok", { kind: "after", after: 7 });
    expect(page.notices).toEqual([{ id: 8, kind: "recovery-email-added", atMs: 5 }]);
    expect(c.calls[0]?.url).toBe("/api/auth/account/notices?after=7");
    // A device with no cursor asks by time instead.
    await c.client.notices("tok", { kind: "since", sinceMs: 1_700_000_000_000 });
    expect(c.calls[1]?.url).toBe("/api/auth/account/notices?since_ms=1700000000000");
  });
});

describe("what a failure says", () => {
  it("a refusal carries its status, its code and the wait", async () => {
    const c = client(() =>
      json({ error: "email-rate-limited", retry_after_secs: 90 }, 429, { "retry-after": "90" }),
    );
    const err = await c.client.recoverStart(HANDLE, "name@example.com", "en").catch((e: unknown) => e);
    expect(answerOf(err)).toEqual({ status: 429, errorCode: "email-rate-limited", retryAfterSecs: 90 });
  });

  it("no answer is a network error", async () => {
    const c = client(() => new TypeError("failed to fetch"));
    const err = await c.client.recoverVerify("cid", "123456").catch((e: unknown) => e);
    expect(err).toBeInstanceOf(AuthNetworkError);
    expect(answerOf(err)).toEqual({ status: null, errorCode: null, retryAfterSecs: null });
  });

  it("anything else is not an answer", () => {
    expect(answerOf(new Error("x"))).toBeNull();
  });

  it("the error of a refused request never repeats the address or the code", async () => {
    const c = client(() => json({ error: "code-refused" }, 401));
    const err = (await c.client
      .enrolFinish({ sessionToken: "tok", challengeId: "cid", code: "123456", sealedEntropy: null })
      .catch((e: unknown) => e)) as Error;
    expect(err.message.includes("123456")).toBe(false);
    const c2 = client(() => json({ error: "address-invalid" }, 400));
    const err2 = (await c2.client.recoverStart(HANDLE, "name@example.com", "en").catch((e: unknown) => e)) as Error;
    expect(err2.message.includes("name@example.com")).toBe(false);
  });
});
