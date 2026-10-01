// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The device link (ADR-0049 section 2 as amended 2026-09-28): the old
 * device chooses and shows a six-digit code, the new device types it, the
 * two run SPAKE2 over the auth relay.
 *
 * Old device (responder flow):
 *   - the code is shown only after `POST .../pake` answered 200; a 409 (another
 *     device answered) closes with "handled on another device" and shows no
 *     code;
 *   - a verified payload posts `accepted` + `confirm_b` and only then pairs;
 *   - a wrong code, a malformed exchange, or a payload for another name posts
 *     `wrong_code` and pairs nothing;
 *   - at most 5 answers per rolling 24 h; the 6th request is declined with
 *     the limit copy; the window rolls.
 * New device (initiator flow):
 *   - the field stays disabled until `pake_b` arrives;
 *   - Continue seals the pairing payload and posts it;
 *   - `accepted` is trusted only after `verifyConfirmation(confirm_b)`
 *     passes; a failing one is the wrong-code state;
 *   - `wrong_code`, a decline and an expiry end the request.
 * Plus the auth routes' wire shapes and a real-wasm round trip of both
 * flows over an in-memory relay.
 */
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";
import { base64UrlEncodeNoPad } from "../auth/account-id";
import { AuthClient, AuthHttpError, type DeviceRequestStatus, type PendingDeviceRequest } from "../auth/client";
import { loginHandleWith } from "../auth/login-handle";
import {
  DEVICE_LINK_ANSWER_LIMIT,
  DEVICE_LINK_ANSWER_WINDOW_MS,
  LINK_DECLINED_OLD,
  LINK_DEVICE_ADDED,
  LINK_ENTER_SIX_DIGITS,
  LINK_HANDLED_ELSEWHERE,
  LINK_NEW_WRONG_CODE,
  LINK_OLD_WRONG_CODE,
  LINK_TOO_MANY_REQUESTS,
  answersInWindow,
  createDeviceLinkInitiator,
  createDeviceLinkResponder,
  deviceLinkCoreOf,
  initiatorEndMessage,
  type AnswerLogStore,
  type DeviceLinkCore,
  type InitiatorPorts,
  type InitiatorView,
  type ResponderPorts,
  type ResponderView,
} from "./device-link";
import { PAIRING_WRONG_ACCOUNT, parsePairingQrPayloadWith } from "./pairing";

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
});

afterEach(() => {
  vi.unstubAllGlobals();
});

// ---------------------------------------------------------------------------
// Fakes
// ---------------------------------------------------------------------------

const REQUEST_ID = base64UrlEncodeNoPad(new Uint8Array(16).fill(1));
const OWN_HANDLE = new Uint8Array(32).fill(0x61);
const GOOD_CONFIRM_A = 0x0a;
const GOOD_CONFIRM_B = 0xcb;

/** A fake core: the "right" code is 471903; a payload sealed under another
 *  code carries a confirm_a the responder refuses. */
function fakeCore(): DeviceLinkCore & { finishes: Array<{ code: string; payload: Uint8Array }> } {
  const finishes: Array<{ code: string; payload: Uint8Array }> = [];
  return {
    finishes,
    newResponder: () => ({
      code: "471903",
      pakeMessage: new Uint8Array([0xb0, 0xb1]),
      accept(_pakeA, sealed, confirmA) {
        if (confirmA[0] === 0xee) throw new Error("wrong-code: key confirmation failed");
        if (confirmA[0] !== GOOD_CONFIRM_A) throw new Error("malformed: bad confirmation");
        return { confirmB: new Uint8Array([GOOD_CONFIRM_B]), pairingPayload: sealed };
      },
    }),
    newInitiator: () => ({
      finish(_pakeB, code, payload) {
        finishes.push({ code, payload });
        return {
          pakeA: new Uint8Array([0xa0]),
          sealedPayload: payload,
          confirmA: new Uint8Array([code === "471903" ? GOOD_CONFIRM_A : 0xee]),
        };
      },
      verifyConfirmation(confirmB) {
        if (confirmB[0] !== GOOD_CONFIRM_B) throw new Error("wrong-code: confirmation mismatch");
      },
    }),
    formatCode: (c) => `${c.slice(0, 2)} ${c.slice(2, 4)} ${c.slice(4, 6)}`,
    normalizeCode(input) {
      const digits = input.replace(/\s/g, "");
      if (!/^\d{6}$/.test(digits)) throw new Error("invalid-code: not six digits");
      return digits;
    },
  };
}

function memoryAnswerLog(initial: number[] = []): AnswerLogStore & { times: number[] } {
  const log = {
    times: [...initial],
    load: () => [...log.times],
    save: (t: number[]) => {
      log.times = [...t];
    },
  };
  return log;
}

function pending(over: Partial<PendingDeviceRequest> = {}): PendingDeviceRequest {
  return {
    requestId: REQUEST_ID,
    createdAtMs: 0,
    pakeB: null,
    pakeA: null,
    sealedPayload: null,
    confirmA: null,
    outcome: null,
    ...over,
  };
}

const PAYLOAD = new Uint8Array([7, 7, 7]);

function responderHarness(over: Partial<ResponderPorts> = {}) {
  const views: ResponderView[] = [];
  const calls: string[] = [];
  let listed: PendingDeviceRequest | null = pending();
  let clock = 1_000_000;
  const answers = memoryAnswerLog();
  const ports: ResponderPorts = {
    core: fakeCore(),
    list: async () => listed,
    answer: vi.fn(async () => {
      calls.push("answer");
    }),
    result: vi.fn(async (_id, r) => {
      calls.push(`result:${r.outcome}`);
    }),
    decline: vi.fn(async () => {
      calls.push("decline");
    }),
    ownLoginHandle: async () => OWN_HANDLE,
    checkPayload: vi.fn(async () => {
      calls.push("check");
    }),
    pair: vi.fn(async () => {
      calls.push("pair");
    }),
    failureMessage: (err) => () => (err instanceof Error ? err.message : String(err)),
    answers,
    now: () => clock,
    onView: (v) => views.push(v),
    ...over,
  };
  const flow = createDeviceLinkResponder(ports);
  return {
    flow,
    ports,
    views,
    calls,
    answers,
    setListed: (r: PendingDeviceRequest | null) => {
      listed = r;
    },
    advance: (ms: number) => {
      clock += ms;
    },
  };
}

const sealedRequest = (confirmA: number): PendingDeviceRequest =>
  pending({
    pakeB: new Uint8Array([0xb0]),
    pakeA: new Uint8Array([0xa0]),
    sealedPayload: PAYLOAD,
    confirmA: new Uint8Array([confirmA]),
  });

// ---------------------------------------------------------------------------
// Old device
// ---------------------------------------------------------------------------

describe("old device: consent before the code (second security pass)", () => {
  it("a new request asks 'Is this you?' and sends nothing, makes no responder", async () => {
    const core = fakeCore();
    const newResponder = vi.spyOn(core, "newResponder");
    const h = responderHarness({ core });
    await h.flow.poll();
    expect(h.flow.view()).toEqual({ kind: "asking", requestId: REQUEST_ID });
    expect(h.ports.answer).not.toHaveBeenCalled();
    expect(newResponder).not.toHaveBeenCalled();
    expect(h.answers.times).toHaveLength(0);
    // Polling again keeps asking; still nothing sent.
    await h.flow.poll();
    expect(h.ports.answer).not.toHaveBeenCalled();
  });

  it("dismissing without choosing sends nothing and does not ask again", async () => {
    const h = responderHarness();
    await h.flow.poll();
    h.flow.dismiss();
    expect(h.flow.view().kind).toBe("idle");
    await h.flow.poll();
    expect(h.flow.view().kind).toBe("idle");
    expect(h.ports.answer).not.toHaveBeenCalled();
    expect(h.ports.decline).not.toHaveBeenCalled();
    expect(h.answers.times).toHaveLength(0);
  });

  it("It is not me on the question deletes the request without answering", async () => {
    const h = responderHarness();
    await h.flow.poll();
    await h.flow.decline();
    expect(h.ports.decline).toHaveBeenCalledWith(REQUEST_ID);
    expect(h.ports.answer).not.toHaveBeenCalled();
    expect(h.flow.view()).toEqual({ kind: "closed", message: LINK_DECLINED_OLD });
  });

  it("a request answered elsewhere while asked closes with 'handled on another device'", async () => {
    const h = responderHarness();
    await h.flow.poll();
    h.setListed(pending({ pakeB: new Uint8Array([1]) }));
    await h.flow.poll();
    expect(h.flow.view()).toEqual({ kind: "closed", message: LINK_HANDLED_ELSEWHERE });
    expect(h.ports.answer).not.toHaveBeenCalled();
  });
});

describe("old device: answering a request", () => {
  it("shows the code only after the answer returned 200", async () => {
    let release: () => void = () => undefined;
    const h = responderHarness({
      answer: vi.fn(
        () =>
          new Promise<void>((resolve) => {
            release = resolve;
          }),
      ),
    });
    await h.flow.poll();
    const confirmed = h.flow.consent();
    await new Promise((r) => setTimeout(r, 0));
    expect(h.ports.answer).toHaveBeenCalledTimes(1);
    expect(h.views.some((v) => v.kind === "showing")).toBe(false);
    expect(h.flow.view().kind).toBe("answering");
    release();
    await confirmed;
    expect(h.flow.view()).toEqual({ kind: "showing", requestId: REQUEST_ID, code: "47 19 03" });
    expect(h.answers.times).toHaveLength(1);
  });

  it("posts this device's own SPAKE2 message for the listed request", async () => {
    const h = responderHarness();
    await h.flow.poll();
    await h.flow.consent();
    expect(h.ports.answer).toHaveBeenCalledWith(REQUEST_ID, new Uint8Array([0xb0, 0xb1]));
  });

  it("a 409 closes with 'handled on another device', never shows a code, and is not retried", async () => {
    const h = responderHarness({
      answer: vi.fn(async () => {
        throw new AuthHttpError(409, "/device/requests/x/pake", JSON.stringify({ error: "device-request-already-answered" }));
      }),
    });
    await h.flow.poll();
    await h.flow.consent();
    expect(h.flow.view()).toEqual({ kind: "closed", message: LINK_HANDLED_ELSEWHERE });
    expect(h.views.some((v) => v.kind === "showing")).toBe(false);
    expect(h.answers.times).toHaveLength(0);
    await h.flow.poll();
    expect(h.ports.answer).toHaveBeenCalledTimes(1);
  });

  it("an offline answer after Yes is retried on the next poll without asking again", async () => {
    let fail = true;
    const h = responderHarness({
      answer: vi.fn(async () => {
        if (fail) throw new TypeError("fetch failed");
      }),
    });
    await h.flow.poll();
    await h.flow.consent();
    expect(h.flow.view().kind).toBe("answering");
    fail = false;
    await h.flow.poll();
    expect(h.flow.view().kind).toBe("showing");
    expect(h.views.filter((v) => v.kind === "asking")).toHaveLength(1);
  });

  it("a request already answered elsewhere (or before a reload) is not offered", async () => {
    const h = responderHarness();
    h.setListed(pending({ pakeB: new Uint8Array([1]) }));
    await h.flow.poll();
    expect(h.ports.answer).not.toHaveBeenCalled();
    expect(h.views).toEqual([]);
  });
});

describe("old device: the decision", () => {
  it("a verified payload posts accepted with confirm_b, then pairs the opened payload", async () => {
    const h = responderHarness();
    await h.flow.poll();
    await h.flow.consent();
    h.setListed(sealedRequest(GOOD_CONFIRM_A));
    await h.flow.poll();
    expect(h.ports.checkPayload).toHaveBeenCalledWith(PAYLOAD, OWN_HANDLE);
    expect(h.ports.result).toHaveBeenCalledWith(REQUEST_ID, {
      outcome: "accepted",
      confirmB: new Uint8Array([GOOD_CONFIRM_B]),
    });
    expect(h.ports.pair).toHaveBeenCalledWith(PAYLOAD);
    expect(h.calls).toEqual(["answer", "check", "result:accepted", "pair"]);
    expect(h.views.map((v) => v.kind)).toEqual(["asking", "answering", "showing", "adding", "closed"]);
    expect(h.flow.view()).toEqual({ kind: "closed", message: LINK_DEVICE_ADDED });
  });

  it("a wrong code posts wrong_code and pairs nothing", async () => {
    const h = responderHarness();
    await h.flow.poll();
    await h.flow.consent();
    h.setListed(sealedRequest(0xee));
    await h.flow.poll();
    expect(h.ports.result).toHaveBeenCalledWith(REQUEST_ID, { outcome: "wrong_code" });
    expect(h.ports.pair).not.toHaveBeenCalled();
    expect(h.ports.checkPayload).not.toHaveBeenCalled();
    expect(h.flow.view()).toEqual({ kind: "closed", message: LINK_OLD_WRONG_CODE });
  });

  it("a malformed exchange posts wrong_code and pairs nothing", async () => {
    const h = responderHarness();
    await h.flow.poll();
    await h.flow.consent();
    h.setListed(sealedRequest(0x55));
    await h.flow.poll();
    expect(h.calls).toEqual(["answer", "result:wrong_code"]);
    expect(h.flow.view()).toEqual({ kind: "closed", message: LINK_OLD_WRONG_CODE });
  });

  it("a payload for another name posts wrong_code and pairs nothing", async () => {
    const h = responderHarness({
      checkPayload: vi.fn(async () => {
        throw new Error(PAIRING_WRONG_ACCOUNT);
      }),
    });
    await h.flow.poll();
    await h.flow.consent();
    h.setListed(sealedRequest(GOOD_CONFIRM_A));
    await h.flow.poll();
    expect(h.calls).toEqual(["answer", "result:wrong_code"]);
    expect(h.ports.pair).not.toHaveBeenCalled();
    const closed = h.flow.view();
    expect(closed.kind).toBe("closed");
    expect(closed.kind === "closed" ? closed.message() : null).toBe(PAIRING_WRONG_ACCOUNT);
  });

  it("an outcome that could not be recorded pairs nothing", async () => {
    const h = responderHarness({
      result: vi.fn(async () => {
        throw new TypeError("fetch failed");
      }),
    });
    await h.flow.poll();
    await h.flow.consent();
    h.setListed(sealedRequest(GOOD_CONFIRM_A));
    await h.flow.poll();
    expect(h.ports.pair).not.toHaveBeenCalled();
    expect(h.flow.view().kind).toBe("closed");
  });

  it("It is not me deletes the request and closes with the decline copy", async () => {
    const h = responderHarness();
    await h.flow.poll();
    await h.flow.consent();
    await h.flow.decline();
    expect(h.ports.decline).toHaveBeenCalledWith(REQUEST_ID);
    expect(h.flow.view()).toEqual({ kind: "closed", message: LINK_DECLINED_OLD });
    // A sealed payload arriving later is never opened.
    h.setListed(sealedRequest(GOOD_CONFIRM_A));
    await h.flow.poll();
    expect(h.ports.pair).not.toHaveBeenCalled();
  });

  it("a vanished or replaced request closes the modal", async () => {
    const h = responderHarness();
    await h.flow.poll();
    await h.flow.consent();
    h.setListed(null);
    await h.flow.poll();
    expect(h.flow.view().kind).toBe("idle");
  });
});

describe("old device: the answer limit (5 per 24 h, chosen, not measured)", () => {
  it("is five per rolling 24 hours", () => {
    expect(DEVICE_LINK_ANSWER_LIMIT).toBe(5);
    expect(DEVICE_LINK_ANSWER_WINDOW_MS).toBe(86_400_000);
  });

  it("answers five, declines the sixth with the limit copy, and the window rolls", async () => {
    const h = responderHarness();
    for (let i = 0; i < 5; i++) {
      h.setListed(pending({ requestId: base64UrlEncodeNoPad(new Uint8Array(16).fill(10 + i)) }));
      await h.flow.poll();
      await h.flow.consent();
      expect(h.flow.view().kind).toBe("showing");
      h.advance(60_000);
    }
    expect(h.ports.answer).toHaveBeenCalledTimes(5);

    // A request nobody says Yes to costs nothing against the limit.
    const unanswered = base64UrlEncodeNoPad(new Uint8Array(16).fill(98));
    h.setListed(pending({ requestId: unanswered }));
    await h.flow.poll();
    expect(h.flow.view().kind).toBe("asking");
    h.flow.dismiss();
    expect(h.ports.decline).not.toHaveBeenCalled();

    const sixth = base64UrlEncodeNoPad(new Uint8Array(16).fill(99));
    h.setListed(pending({ requestId: sixth }));
    await h.flow.poll();
    expect(h.flow.view().kind).toBe("asking");
    await h.flow.consent();
    expect(h.ports.answer).toHaveBeenCalledTimes(5);
    expect(h.ports.decline).toHaveBeenCalledWith(sixth);
    expect(h.flow.view()).toEqual({ kind: "limited", message: LINK_TOO_MANY_REQUESTS });
    expect(LINK_TOO_MANY_REQUESTS()).toBe(
      "Too many sign-in requests today. Use a QR code, or try again tomorrow.",
    );
    h.flow.dismiss();
    expect(h.flow.view().kind).toBe("idle");

    // The first answer leaves the window 24 h after it was given.
    h.advance(DEVICE_LINK_ANSWER_WINDOW_MS - 5 * 60_000 + 1);
    h.setListed(pending({ requestId: base64UrlEncodeNoPad(new Uint8Array(16).fill(100)) }));
    await h.flow.poll();
    await h.flow.consent();
    expect(h.ports.answer).toHaveBeenCalledTimes(6);
    expect(h.flow.view().kind).toBe("showing");
    expect(h.answers.times).toHaveLength(5);
  });

  it("answersInWindow drops old, future and non-finite times", () => {
    const now = 10 * DEVICE_LINK_ANSWER_WINDOW_MS;
    expect(
      answersInWindow([now - DEVICE_LINK_ANSWER_WINDOW_MS, now - 1, now + 5, Number.NaN, now], now),
    ).toEqual([now - 1, now]);
  });
});

// ---------------------------------------------------------------------------
// New device
// ---------------------------------------------------------------------------

function initiatorHarness(over: Partial<InitiatorPorts> = {}) {
  const views: InitiatorView[] = [];
  let status: DeviceRequestStatus = { pending: true, pakeB: null, outcome: null, confirmB: null };
  let clock = 0;
  const core = fakeCore();
  const ports: InitiatorPorts = {
    core,
    create: vi.fn(async () => ({ requestId: REQUEST_ID, expiresInSecs: 600 })),
    status: vi.fn(async () => status),
    postPayload: vi.fn(async () => undefined),
    now: () => clock,
    sleep: async () => undefined,
    onView: (v) => views.push(v),
    ...over,
  };
  const flow = createDeviceLinkInitiator(ports, { loginHandle: OWN_HANDLE, pairingPayload: PAYLOAD });
  return {
    flow,
    ports,
    core,
    views,
    setStatus: (s: Partial<DeviceRequestStatus>) => {
      status = { ...status, ...s };
    },
    advance: (ms: number) => {
      clock += ms;
    },
  };
}

async function toReady(h: ReturnType<typeof initiatorHarness>): Promise<void> {
  await h.flow.start();
  h.setStatus({ pakeB: new Uint8Array([0xb0]) });
  await h.flow.poll();
}

describe("new device: the code field", () => {
  it("posts the handle, then keeps the field disabled until pake_b arrives", async () => {
    const h = initiatorHarness();
    expect(await h.flow.start()).toEqual({ kind: "waiting" });
    expect(h.ports.create).toHaveBeenCalledWith(OWN_HANDLE);
    await h.flow.poll();
    expect(h.flow.view().kind).toBe("waiting");
    await h.flow.submit("471903");
    expect(h.core.finishes).toHaveLength(0);
    h.setStatus({ pakeB: new Uint8Array([0xb0]) });
    await h.flow.poll();
    expect(h.flow.view()).toEqual({ kind: "ready", error: null });
  });

  it("a value that is not six digits stays on the field with a hint, nothing spent", async () => {
    const h = initiatorHarness();
    await toReady(h);
    await h.flow.submit("4719");
    expect(h.flow.view()).toEqual({ kind: "ready", error: LINK_ENTER_SIX_DIGITS });
    expect(h.core.finishes).toHaveLength(0);
    expect(h.ports.postPayload).not.toHaveBeenCalled();
  });

  it("Continue seals the pairing payload with the typed code and posts it", async () => {
    const h = initiatorHarness();
    await toReady(h);
    await h.flow.submit("47 19 03");
    expect(h.core.finishes).toEqual([{ code: "471903", payload: PAYLOAD }]);
    expect(h.ports.postPayload).toHaveBeenCalledWith(REQUEST_ID, {
      pakeA: new Uint8Array([0xa0]),
      sealedPayload: PAYLOAD,
      confirmA: new Uint8Array([GOOD_CONFIRM_A]),
    });
    expect(h.flow.view().kind).toBe("checking");
  });
});

describe("new device: trusting the outcome", () => {
  it("accepted is trusted only after confirm_b verifies", async () => {
    const h = initiatorHarness();
    await toReady(h);
    await h.flow.submit("471903");
    expect(h.flow.isConfirmed()).toBe(false);
    h.setStatus({ outcome: "accepted", confirmB: new Uint8Array([GOOD_CONFIRM_B]) });
    await expect(h.flow.awaitConfirmation()).resolves.toBe(true);
    expect(h.flow.isConfirmed()).toBe(true);
    expect(h.flow.view().kind).toBe("confirmed");
  });

  it("a confirm_b that does not verify refuses: wrong-code, never confirmed", async () => {
    const h = initiatorHarness();
    await toReady(h);
    await h.flow.submit("471903");
    h.setStatus({ outcome: "accepted", confirmB: new Uint8Array([0x00]) });
    await expect(h.flow.awaitConfirmation()).resolves.toBe(false);
    expect(h.flow.isConfirmed()).toBe(false);
    expect(h.flow.view().kind).toBe("wrong-code");
    expect(initiatorEndMessage(h.flow.view())).toBe(LINK_NEW_WRONG_CODE);
  });

  it("an acceptance of a payload this device never sent is refused", async () => {
    const h = initiatorHarness();
    await toReady(h);
    h.setStatus({ outcome: "accepted", confirmB: new Uint8Array([GOOD_CONFIRM_B]) });
    await h.flow.poll();
    expect(h.flow.isConfirmed()).toBe(false);
    expect(h.flow.view().kind).toBe("wrong-code");
  });

  it("wrong_code from the old device is the wrong-code state", async () => {
    const h = initiatorHarness();
    await toReady(h);
    await h.flow.submit("000000");
    h.setStatus({ outcome: "wrong_code" });
    await h.flow.poll();
    expect(h.flow.view().kind).toBe("wrong-code");
    expect(LINK_NEW_WRONG_CODE()).toBe(
      "That code does not match. Start again and enter the code your other device shows.",
    );
  });

  it("a vanished request is a decline before ten minutes and an expiry after", async () => {
    const declined = initiatorHarness();
    await declined.flow.start();
    declined.setStatus({ pending: false });
    await declined.flow.poll();
    expect(declined.flow.view().kind).toBe("declined");
    expect(initiatorEndMessage(declined.flow.view())?.()).toBe("Declined on another device.");

    const expired = initiatorHarness();
    await expired.flow.start();
    expired.advance(600_000);
    await expired.flow.poll();
    expect(expired.flow.view().kind).toBe("expired");
    expect(initiatorEndMessage(expired.flow.view())?.()).toBe("This request expired. Start again.");
  });

  it("an offline payload post is re-sent on the next poll; already-used counts as posted", async () => {
    let attempt = 0;
    const h = initiatorHarness({
      postPayload: vi.fn(async () => {
        attempt += 1;
        if (attempt === 1) throw new TypeError("fetch failed");
        throw new AuthHttpError(409, "/device/request/payload", JSON.stringify({ error: "device-request-already-used" }));
      }),
    });
    await toReady(h);
    await h.flow.submit("471903");
    expect(h.flow.view().kind).toBe("checking");
    await h.flow.poll();
    expect(h.ports.postPayload).toHaveBeenCalledTimes(2);
    h.setStatus({ outcome: "accepted", confirmB: new Uint8Array([GOOD_CONFIRM_B]) });
    await h.flow.poll();
    expect(h.flow.isConfirmed()).toBe(true);
  });

  it("a request that could not be posted is a failed start", async () => {
    const h = initiatorHarness({
      create: vi.fn(async () => {
        throw new AuthHttpError(429, "/device/request", JSON.stringify({ error: "device-request-limited" }));
      }),
    });
    expect((await h.flow.start()).kind).toBe("failed");
  });
});

// ---------------------------------------------------------------------------
// The auth routes
// ---------------------------------------------------------------------------

interface Seen {
  readonly method: string;
  readonly url: string;
  readonly authorization: string | null;
  readonly body: unknown;
}

function stubFetch(answer: (url: string) => { status: number; json: unknown }): Seen[] {
  const seen: Seen[] = [];
  vi.stubGlobal(
    "fetch",
    vi.fn(async (input: RequestInfo | URL, reqInit?: RequestInit) => {
      const url = String(input);
      const headers = (reqInit?.headers ?? {}) as Record<string, string>;
      seen.push({
        method: reqInit?.method ?? "GET",
        url,
        authorization: headers["authorization"] ?? null,
        body: typeof reqInit?.body === "string" ? JSON.parse(reqInit.body) : null,
      });
      const a = answer(url);
      return new Response(JSON.stringify(a.json), { status: a.status });
    }),
  );
  return seen;
}

describe("the device-request routes", () => {
  const c = (): AuthClient => new AuthClient("/api/auth");

  it("the new device's status and payload: request id in the body, values standard base64", async () => {
    const seen = stubFetch((url) =>
      url.endsWith("/status")
        ? { status: 200, json: { pending: true, pake_b_b64: "+/8=", outcome: "accepted", confirm_b_b64: "AQI=" } }
        : { status: 200, json: {} },
    );
    const st = await c().deviceRequestStatus(REQUEST_ID);
    expect(st).toEqual({
      pending: true,
      pakeB: new Uint8Array([0xfb, 0xff]),
      outcome: "accepted",
      confirmB: new Uint8Array([1, 2]),
    });
    await c().deviceRequestPayload(REQUEST_ID, {
      pakeA: new Uint8Array([0xfb, 0xff]),
      sealedPayload: new Uint8Array([1]),
      confirmA: new Uint8Array([2]),
    });
    expect(seen.map((s) => s.url)).toEqual([
      "/api/auth/device/request/status",
      "/api/auth/device/request/payload",
    ]);
    expect(seen[0]?.body).toEqual({ request_id_b64: REQUEST_ID });
    expect(seen[1]?.body).toEqual({
      request_id_b64: REQUEST_ID,
      pake_a_b64: "+/8=",
      sealed_payload_b64: "AQ==",
      confirm_a_b64: "Ag==",
    });
    expect(seen.every((s) => s.authorization === null)).toBe(true);
  });

  it("the old device's list, answer and result carry the session", async () => {
    const seen = stubFetch((url) =>
      url.endsWith("/device/requests")
        ? {
            status: 200,
            json: {
              request: {
                request_id_b64: REQUEST_ID,
                created_at_ms: 5,
                pake_b_b64: null,
                pake_a_b64: null,
                sealed_payload_b64: null,
                confirm_a_b64: null,
                outcome: null,
              },
            },
          }
        : { status: 200, json: {} },
    );
    const r = await c().deviceRequestsList("tok");
    expect(r).toEqual(pending({ createdAtMs: 5 }));
    await c().deviceRequestAnswer("tok", REQUEST_ID, new Uint8Array([0xfb, 0xff]));
    await c().deviceRequestResult("tok", REQUEST_ID, { outcome: "accepted", confirmB: new Uint8Array([1]) });
    await c().deviceRequestResult("tok", REQUEST_ID, { outcome: "wrong_code" });
    expect(seen.map((s) => `${s.method} ${s.url}`)).toEqual([
      "GET /api/auth/device/requests",
      `POST /api/auth/device/requests/${REQUEST_ID}/pake`,
      `POST /api/auth/device/requests/${REQUEST_ID}/result`,
      `POST /api/auth/device/requests/${REQUEST_ID}/result`,
    ]);
    expect(seen.every((s) => s.authorization === "Bearer tok")).toBe(true);
    expect(seen[1]?.body).toEqual({ pake_b_b64: "+/8=" });
    expect(seen[2]?.body).toEqual({ outcome: "accepted", confirm_b_b64: "AQ==" });
    expect(seen[3]?.body).toEqual({ outcome: "wrong_code" });
  });

  it("a 409 on the answer surfaces with its code", async () => {
    stubFetch(() => ({ status: 409, json: { error: "device-request-already-answered" } }));
    const err = await c()
      .deviceRequestAnswer("tok", REQUEST_ID, new Uint8Array([1]))
      .catch((e: unknown) => e);
    expect(err).toBeInstanceOf(AuthHttpError);
    expect((err as AuthHttpError).status).toBe(409);
    expect((err as AuthHttpError).errorCode).toBe("device-request-already-answered");
  });
});

// ---------------------------------------------------------------------------
// Real wasm, both flows, one in-memory relay
// ---------------------------------------------------------------------------

/** The relay as `services/auth/src/device_request.rs` behaves: one request,
 *  each value written once. */
function memoryRelay() {
  type Row = {
    id: string;
    pakeB: Uint8Array | null;
    pakeA: Uint8Array | null;
    sealed: Uint8Array | null;
    confirmA: Uint8Array | null;
    outcome: "accepted" | "wrong_code" | null;
    confirmB: Uint8Array | null;
  };
  let row: Row | null = null;
  const conflict = (code: string): AuthHttpError => new AuthHttpError(409, "/device", JSON.stringify({ error: code }));
  return {
    create: async () => {
      const id = base64UrlEncodeNoPad(crypto.getRandomValues(new Uint8Array(16)));
      row = { id, pakeB: null, pakeA: null, sealed: null, confirmA: null, outcome: null, confirmB: null };
      return { requestId: id, expiresInSecs: 600 };
    },
    status: async (): Promise<DeviceRequestStatus> => ({
      pending: row !== null,
      pakeB: row?.pakeB ?? null,
      outcome: row?.outcome ?? null,
      confirmB: row?.confirmB ?? null,
    }),
    postPayload: async (_id: string, s: { pakeA: Uint8Array; sealedPayload: Uint8Array; confirmA: Uint8Array }) => {
      if (row === null) throw new AuthHttpError(404, "/device", JSON.stringify({ error: "device-request-not-found" }));
      if (row.pakeB === null) throw conflict("device-request-not-ready");
      if (row.sealed !== null) throw conflict("device-request-already-used");
      row.pakeA = s.pakeA;
      row.sealed = s.sealedPayload;
      row.confirmA = s.confirmA;
    },
    list: async (): Promise<PendingDeviceRequest | null> =>
      row === null
        ? null
        : {
            requestId: row.id,
            createdAtMs: 0,
            pakeB: row.pakeB,
            pakeA: row.pakeA,
            sealedPayload: row.sealed,
            confirmA: row.confirmA,
            outcome: row.outcome,
          },
    answer: async (_id: string, pakeB: Uint8Array) => {
      if (row === null) throw new AuthHttpError(404, "/device", "{}");
      if (row.pakeB !== null) throw conflict("device-request-already-answered");
      row.pakeB = pakeB;
    },
    result: async (_id: string, r: { outcome: "accepted"; confirmB: Uint8Array } | { outcome: "wrong_code" }) => {
      if (row === null) throw new AuthHttpError(404, "/device", "{}");
      if (row.sealed === null) throw conflict("device-request-not-ready");
      if (row.outcome !== null) throw conflict("device-request-already-used");
      row.outcome = r.outcome;
      row.confirmB = r.outcome === "accepted" ? r.confirmB : null;
    },
    decline: async () => {
      row = null;
    },
  };
}

function realPairingPayload(handle: Uint8Array): Uint8Array {
  const id = wasm.Identity.fromSeed(new Uint8Array(32).fill(4));
  try {
    return wasm.encodePairingQr(handle, id.createKeyPackage(), new Uint8Array(32).fill(9));
  } finally {
    id.free();
  }
}

async function roundTrip(typeCode: (shown: string) => string) {
  const relay = memoryRelay();
  const core = deviceLinkCoreOf(wasm);
  const handle = loginHandleWith(wasm, "alice").bytes;
  const payload = realPairingPayload(handle);
  const paired: Uint8Array[] = [];
  const results: string[] = [];
  const old = createDeviceLinkResponder({
    core,
    list: relay.list,
    answer: relay.answer,
    result: async (id, r) => {
      results.push(r.outcome);
      await relay.result(id, r);
    },
    decline: relay.decline,
    ownLoginHandle: async () => handle,
    checkPayload: async (p, h) => {
      parsePairingQrPayloadWith(wasm, p, h);
    },
    pair: async (p) => {
      paired.push(p);
    },
    failureMessage: (e) => () => String(e),
    answers: memoryAnswerLog(),
    now: () => Date.now(),
    onView: () => undefined,
  });
  const fresh = createDeviceLinkInitiator(
    {
      core,
      create: relay.create,
      status: relay.status,
      postPayload: relay.postPayload,
      now: () => Date.now(),
      sleep: async () => undefined,
      onView: () => undefined,
    },
    { loginHandle: handle, pairingPayload: payload },
  );
  await fresh.start();
  await fresh.poll();
  expect(fresh.view().kind).toBe("waiting");
  await old.poll();
  expect(old.view().kind).toBe("asking");
  await old.consent();
  const shown = old.view();
  if (shown.kind !== "showing") throw new Error(`expected the code, got ${shown.kind}`);
  expect(shown.code).toMatch(/^\d\d \d\d \d\d$/);
  await fresh.poll();
  expect(fresh.view().kind).toBe("ready");
  await fresh.submit(typeCode(shown.code));
  await old.poll();
  const confirmed = await fresh.awaitConfirmation();
  return { confirmed, paired, payload, results, oldView: old.view(), freshView: fresh.view() };
}

describe("the device link over the real wasm core", () => {
  it("the right code pairs the exact payload and the new device's confirmation verifies", async () => {
    const r = await roundTrip((shown) => shown);
    expect(r.results).toEqual(["accepted"]);
    expect(r.paired).toEqual([r.payload]);
    expect(r.confirmed).toBe(true);
    expect(r.oldView).toEqual({ kind: "closed", message: LINK_DEVICE_ADDED });
    expect(r.freshView.kind).toBe("confirmed");
  });

  it("a wrong code pairs nothing and ends in the wrong-code state on both sides", async () => {
    const r = await roundTrip((shown) => {
      const digits = shown.replace(/\s/g, "");
      const last = (Number(digits[5]) + 1) % 10;
      return `${digits.slice(0, 5)}${last}`;
    });
    expect(r.results).toEqual(["wrong_code"]);
    expect(r.paired).toEqual([]);
    expect(r.confirmed).toBe(false);
    expect(r.oldView).toEqual({ kind: "closed", message: LINK_OLD_WRONG_CODE });
    expect(r.freshView.kind).toBe("wrong-code");
  });
});
