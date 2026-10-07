// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The device link: the camera-less another-device route (ADR-0049 section 2
 * as amended 2026-09-28; UX `docs/ux/identity-recovery.md` section 4).
 *
 * The signed-in device (the OLD device) chooses six digits and shows them;
 * the person types them on the NEW device; the two run SPAKE2 over the auth
 * relay with those digits as the password. The new device's pairing payload
 * (the bytes the QR code would carry) travels sealed under the agreed key
 * with a key confirmation; the old device pairs only if that confirmation
 * verifies, and the new device trusts the Welcome that follows only after
 * the old device's confirmation verifies in turn. The relay sees no code and
 * gets one guess per request the person's device answers.
 *
 * This module is the two state machines, with every collaborator injected
 * (the wasm classes, the auth routes, the pairing flow, the clock, storage),
 * so the rules are host-tested without a DOM. `DeviceRequestPrompt` (old
 * device) and `RestoreViaDevicePanel` (new device) render them.
 *
 * The old comparison code (a hash both devices computed from the payload)
 * is gone: a relay could grind a payload of its own to the victim's code.
 */
import { base64UrlDecodeNoPad } from "../auth/account-id";
import { AuthHttpError, type DeviceRequestStatus, type PendingDeviceRequest } from "../auth/client";
import { t } from "../l10n";
import type { Wasm } from "../wasm";

// ---------------------------------------------------------------------------
// Copy (docs/ux/identity-recovery.md section 4)
// ---------------------------------------------------------------------------

/** A line held as the way to render it, so a line already on screen follows
 *  a language switch (ADR-0061). */
export type LinkText = () => string;

/** Old device: another device of this account answered first. */
export const LINK_HANDLED_ELSEWHERE: LinkText = () => t("device_link.request.handled_elsewhere");
/** Old device: the new device typed a wrong code (or sent something that
 *  does not open). */
export const LINK_OLD_WRONG_CODE: LinkText = () => t("device_link.request.wrong_code");
/** Old device: "It is not me". */
export const LINK_DECLINED_OLD: LinkText = () => t("device_link.request.declined");
/** Old device: the pairing finished. */
export const LINK_DEVICE_ADDED: LinkText = () => t("device_link.request.added");
/** Old device: the request went away while "Is this you?" was open. */
export const LINK_REQUEST_EXPIRED: LinkText = () => t("device_link.request.expired");
/** Old device: the answer limit is reached; the request is declined. */
export const LINK_TOO_MANY_REQUESTS: LinkText = () => t("device_link.request.too_many");
/** Old device: the outcome could not be recorded, so nothing was paired. */
export const LINK_RESULT_NOT_RECORDED: LinkText = () => t("device_link.request.not_recorded");

/** New device: the intro above the field (web divergence of section 10). */
export const LINK_NEW_INTRO: LinkText = () => t("device_link.code.intro");
/** New device: the field before the old device has answered. */
export const LINK_WAITING: LinkText = () => t("device_link.code.waiting");
/** New device: a typed value that is not six digits. */
export const LINK_ENTER_SIX_DIGITS: LinkText = () => t("device_link.code.enter_six_digits");
/** New device: one try per request, and it was wrong. */
export const LINK_NEW_WRONG_CODE: LinkText = () => t("device_link.code.wrong");
/** New device: the old device confirmed; the Welcome is on its way. */
export const LINK_RESTORING: LinkText = () => t("device_link.code.restoring");
/** New device: "It is not me" on the other device. */
export const LINK_DECLINED_NEW: LinkText = () => t("device_link.error.declined");
/** New device: ten minutes passed. */
export const LINK_EXPIRED: LinkText = () => t("device_link.code.expired");
/** New device: the payload could not be posted. */
export const LINK_UNREACHABLE: LinkText = () => t("device_link.code.unreachable");

// ---------------------------------------------------------------------------
// The wasm surface, narrowed to what the flows use
// ---------------------------------------------------------------------------

/** The old device's half of one exchange (`DeviceLinkResponder`). */
export interface ResponderHandle {
  /** The six digits this device chose, e.g. `"471903"`. */
  readonly code: string;
  /** The SPAKE2 message to post as `pake_b_b64`. */
  readonly pakeMessage: Uint8Array;
  /** Verify and open. Throws `wrong-code: ...`, `malformed: ...` or
   *  `already-used: ...`. Runs once. */
  accept(
    pakeA: Uint8Array,
    sealedPayload: Uint8Array,
    confirmA: Uint8Array,
  ): { readonly confirmB: Uint8Array; readonly pairingPayload: Uint8Array };
  free?(): void;
}

/** The new device's half of one exchange (`DeviceLinkInitiator`). */
export interface InitiatorHandle {
  /** Throws `invalid-code: ...` (nothing spent), `malformed: ...`,
   *  `bad-input: ...` or `already-used: ...`. Runs once. */
  finish(
    pakeB: Uint8Array,
    typedCode: string,
    pairingPayload: Uint8Array,
  ): { readonly pakeA: Uint8Array; readonly sealedPayload: Uint8Array; readonly confirmA: Uint8Array };
  /** Throws `wrong-code: ...`, `malformed: ...` or `not-ready: ...`. */
  verifyConfirmation(confirmB: Uint8Array): void;
  free?(): void;
}

export interface DeviceLinkCore {
  newResponder(requestId: Uint8Array, loginHandle: Uint8Array): ResponderHandle;
  newInitiator(requestId: Uint8Array, loginHandle: Uint8Array): InitiatorHandle;
  /** `"471903"` -> `"47 19 03"`. */
  formatCode(code: string): string;
  /** `"47 19 03"` -> `"471903"`; throws `invalid-code: ...`. */
  normalizeCode(input: string): string;
}

/** The app's wasm core as a {@link DeviceLinkCore}. The accessor objects the
 *  wasm returns are read out at once and freed, so no handle leaks. */
export function deviceLinkCoreOf(w: Wasm): DeviceLinkCore {
  return {
    newResponder(requestId, loginHandle) {
      const r = new w.DeviceLinkResponder(requestId, loginHandle);
      return {
        code: r.code,
        pakeMessage: r.pakeMessage,
        accept(pakeA, sealedPayload, confirmA) {
          const a = r.accept(pakeA, sealedPayload, confirmA);
          const out = { confirmB: a.confirmB, pairingPayload: a.pairingPayload };
          a.free();
          return out;
        },
        free: () => r.free(),
      };
    },
    newInitiator(requestId, loginHandle) {
      const i = new w.DeviceLinkInitiator(requestId, loginHandle);
      return {
        finish(pakeB, typedCode, pairingPayload) {
          const s = i.finish(pakeB, typedCode, pairingPayload);
          const out = { pakeA: s.pakeA, sealedPayload: s.sealedPayload, confirmA: s.confirmA };
          s.free();
          return out;
        },
        verifyConfirmation: (confirmB) => i.verifyConfirmation(confirmB),
        free: () => i.free(),
      };
    },
    formatCode: (code) => w.deviceLinkFormatCode(code),
    normalizeCode: (input) => w.deviceLinkNormalizeCode(input),
  };
}

function errorText(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

function safeFree(h: { free?(): void } | null): void {
  try {
    h?.free?.();
  } catch {
    // Already consumed or freed.
  }
}

// ---------------------------------------------------------------------------
// The answer limit (old device)
// ---------------------------------------------------------------------------

/** Chosen, not measured: an old device answers at most this many requests
 *  in {@link DEVICE_LINK_ANSWER_WINDOW_MS}. Each answer gives whoever posted
 *  the request (possibly a malicious relay) one guess at a six-digit code,
 *  and puts a prompt on this device. */
export const DEVICE_LINK_ANSWER_LIMIT = 5;
/** The rolling window of {@link DEVICE_LINK_ANSWER_LIMIT}: 24 hours. */
export const DEVICE_LINK_ANSWER_WINDOW_MS = 24 * 60 * 60 * 1000;

/** Where the answer times (ms) persist. */
export interface AnswerLogStore {
  load(): number[];
  save(times: number[]): void;
}

/** The answers still inside the window at `now`. */
export function answersInWindow(times: readonly number[], now: number): number[] {
  return times.filter((t) => Number.isFinite(t) && t <= now && now - t < DEVICE_LINK_ANSWER_WINDOW_MS);
}

/** `true` while this device may answer one more request. */
export function canAnswer(store: AnswerLogStore, now: number): boolean {
  return answersInWindow(store.load(), now).length < DEVICE_LINK_ANSWER_LIMIT;
}

/** Record an answer at `now`, dropping the ones the window left behind. */
export function recordAnswer(store: AnswerLogStore, now: number): void {
  store.save([...answersInWindow(store.load(), now), now]);
}

function answerLogKey(accountId: string): string {
  return `lakko/device-link/answers/v1/${accountId}`;
}

/** Drop the account's answer log (Forget device, Delete account). */
export function clearDeviceLinkAnswers(accountId: string): void {
  try {
    globalThis.localStorage?.removeItem(answerLogKey(accountId));
  } catch {
    // Storage blocked: nothing was stored either.
  }
}

/**
 * The answer log in `localStorage`, per account on this browser (the
 * browser is the device). Every access is guarded: with storage blocked the
 * log reads empty and the server still enforces one exchange per request.
 */
export function localStorageAnswerLog(accountId: string): AnswerLogStore {
  const key = answerLogKey(accountId);
  return {
    load(): number[] {
      try {
        const raw = globalThis.localStorage?.getItem(key) ?? null;
        if (raw === null) return [];
        const parsed: unknown = JSON.parse(raw);
        return Array.isArray(parsed) ? parsed.filter((t): t is number => typeof t === "number") : [];
      } catch {
        return [];
      }
    },
    save(times: number[]): void {
      try {
        globalThis.localStorage?.setItem(key, JSON.stringify(times));
      } catch {
        // Storage full or blocked; see above.
      }
    },
  };
}

// ---------------------------------------------------------------------------
// Old device: the responder flow
// ---------------------------------------------------------------------------

/** What the old device's modal shows. */
export type ResponderView =
  | { readonly kind: "idle" }
  /** Step 1: "Is this you?". Nothing has been sent for the request; the
   *  responder does not exist yet (second security pass, 2026-09-28). */
  | { readonly kind: "asking"; readonly requestId: string }
  /** "Yes, it's me" was tapped and the answer is on its way; the code
   *  shows once it lands. */
  | { readonly kind: "answering"; readonly requestId: string }
  /** "New sign-in request": the code, shown only after the answer landed. */
  | { readonly kind: "showing"; readonly requestId: string; readonly code: string }
  /** The code was typed right; pairing runs. */
  | { readonly kind: "adding"; readonly requestId: string }
  /** The code was typed right, and the server asked for a fresh proof
   *  before it records the outcome (the ten minutes ran out between the
   *  steps). The request is kept; `resume` goes on once a proof is live. */
  | { readonly kind: "proving"; readonly requestId: string }
  /** Over the answer limit; the request was declined. A modal, with Close. */
  | { readonly kind: "limited"; readonly message: LinkText }
  /** The modal closed with a one-line result (a toast). */
  | { readonly kind: "closed"; readonly message: LinkText };

export interface ResponderPorts {
  readonly core: DeviceLinkCore;
  /** `GET /device/requests`. */
  list(): Promise<PendingDeviceRequest | null>;
  /** `POST /device/requests/{id}/pake`. */
  answer(requestId: string, pakeB: Uint8Array): Promise<void>;
  /** `POST /device/requests/{id}/result`. */
  result(
    requestId: string,
    result: { outcome: "accepted"; confirmB: Uint8Array } | { outcome: "wrong_code" },
  ): Promise<void>;
  /** `DELETE /device/requests/{id}`. */
  decline(requestId: string): Promise<void>;
  /** This account's own login handle; throws the no-username copy for a
   *  guest. */
  ownLoginHandle(): Promise<Uint8Array>;
  /** Parse the opened payload against this account's own handle, exactly as
   *  a scanned QR (throws on a payload for another name or version). */
  checkPayload(payload: Uint8Array, ownHandle: Uint8Array): Promise<void>;
  /** The unchanged add-device flow over the opened payload. */
  pair(payload: Uint8Array): Promise<void>;
  /** The toast for a failed `checkPayload` or `pair`. */
  failureMessage(err: unknown): LinkText;
  /**
   * Whether `err` (from `answer` or `result`) is the server asking for a
   * fresh-auth proof (`403 fresh-auth-required`, or no proof held). When it
   * is, nothing is lost: the question is asked again and the surface shows
   * its proof block (docs/ux/identity-recovery.md 17.4). Absent: never.
   */
  proofRefused?(err: unknown): boolean;
  readonly answers: AnswerLogStore;
  now(): number;
  onView(view: ResponderView): void;
}

export interface DeviceLinkResponderFlow {
  /** One poll of `GET /device/requests`. Single-flight; never throws. */
  poll(): Promise<void>;
  /** "Yes, it's me" on the asked request: only now is a responder made and
   *  its SPAKE2 message posted. Never throws. */
  consent(): Promise<void>;
  /** After the `proving` step obtained a proof: record the outcome and
   *  pair. Never throws. */
  resume(): Promise<void>;
  /** "It is not me" on the asked or shown request. */
  decline(): Promise<void>;
  /** Close the "Is this you?" step without choosing (nothing is sent; the
   *  request expires on its own), a `limited` modal or a `closed` toast. */
  dismiss(): void;
  view(): ResponderView;
  /** Drop the held responder (component unmount). */
  dispose(): void;
}

/**
 * The old device's side. Each poll looks at the one open request:
 *
 * - new and unanswered: ask "Is this you?" and send NOTHING (UX section 4,
 *   amended by the second security pass): a request nobody expected gets no
 *   answer, so it gives its sender no guess. Dismissing the question leaves
 *   the request unanswered to expire;
 * - after "Yes, it's me": check the answer limit (over it: decline, show the
 *   limited modal; the limit counts answers given after Yes), make a
 *   responder, post its message; the code is shown only after that post
 *   returns 200, and a 409 closes with "handled on another device". An
 *   offline answer is retried on the next poll without asking again;
 * - already answered but not by the responder this page holds (another
 *   device, or this one before a reload): not offered, since the code is
 *   gone and the request could only be declined; it expires on its own;
 * - sealed payload arrived: open it; wrong code or anything malformed posts
 *   `wrong_code` and pairs nothing; otherwise the payload is checked against
 *   this account's own handle, `accepted` is posted with `confirm_b`, and
 *   only after that lands does the unchanged pairing run;
 * - gone, replaced or decided: the modal closes.
 */
export function createDeviceLinkResponder(ports: ResponderPorts): DeviceLinkResponderFlow {
  type Active = {
    readonly requestId: string;
    readonly responder: ResponderHandle;
    readonly ownHandle: Uint8Array;
  };
  let active: Active | null = null;
  /** A verified exchange whose `accepted` outcome the server refused for
   *  want of a fresh proof; kept until `resume`, a decline or its expiry. */
  let owed: { readonly requestId: string; readonly confirmB: Uint8Array; readonly payload: Uint8Array } | null =
    null;
  /** The request "Is this you?" is asked about; nothing sent for it yet. */
  let asking: PendingDeviceRequest | null = null;
  /** Requests the person said "Yes, it's me" to whose answer has not landed
   *  (an offline answer is retried without asking again). */
  const consented = new Set<string>();
  let current: ResponderView = { kind: "idle" };
  let busy = false;
  const handled = new Set<string>();
  // Poll steps and the person's choices run one at a time, in order.
  let chain: Promise<void> = Promise.resolve();
  const serial = (fn: () => Promise<void>): Promise<void> => {
    const next = chain.then(fn, fn);
    chain = next.catch(() => undefined);
    return next;
  };

  const show = (v: ResponderView): void => {
    current = v;
    ports.onView(v);
  };
  const drop = (): void => {
    if (active !== null) {
      safeFree(active.responder);
      active = null;
    }
  };
  const closeIfShowing = (): void => {
    if (
      current.kind === "showing" ||
      current.kind === "adding" ||
      current.kind === "asking" ||
      current.kind === "answering"
    ) {
      show({ kind: "idle" });
    }
  };

  async function answerNew(req: PendingDeviceRequest): Promise<void> {
    const now = ports.now();
    if (!canAnswer(ports.answers, now)) {
      handled.add(req.requestId);
      consented.delete(req.requestId);
      show({ kind: "limited", message: LINK_TOO_MANY_REQUESTS });
      try {
        await ports.decline(req.requestId);
      } catch {
        // The request expires on its own.
      }
      return;
    }
    let ownHandle: Uint8Array;
    try {
      ownHandle = await ports.ownLoginHandle();
    } catch (err) {
      handled.add(req.requestId);
      consented.delete(req.requestId);
      show({ kind: "closed", message: ports.failureMessage(err) });
      return;
    }
    const responder = ports.core.newResponder(base64UrlDecodeNoPad(req.requestId), ownHandle);
    try {
      await ports.answer(req.requestId, responder.pakeMessage);
    } catch (err) {
      safeFree(responder);
      if (ports.proofRefused?.(err) === true) {
        // The proof ran out between the tap and the post: nothing was
        // answered. Ask again; the surface shows its proof block.
        consented.delete(req.requestId);
        asking = req;
        show({ kind: "asking", requestId: req.requestId });
        return;
      }
      if (err instanceof AuthHttpError && err.status === 409) {
        handled.add(req.requestId);
        consented.delete(req.requestId);
        show({ kind: "closed", message: LINK_HANDLED_ELSEWHERE });
      } else if (err instanceof AuthHttpError && err.status === 404) {
        handled.add(req.requestId);
        consented.delete(req.requestId);
        closeIfShowing();
      }
      // Anything else (offline): still "answering"; the next poll retries
      // without asking again.
      return;
    }
    consented.delete(req.requestId);
    recordAnswer(ports.answers, now);
    active = { requestId: req.requestId, responder, ownHandle };
    show({ kind: "showing", requestId: req.requestId, code: ports.core.formatCode(responder.code) });
  }

  async function finish(a: Active, pakeA: Uint8Array, sealed: Uint8Array, confirmA: Uint8Array): Promise<void> {
    handled.add(a.requestId);
    active = null;
    let opened: { confirmB: Uint8Array; pairingPayload: Uint8Array };
    try {
      opened = a.responder.accept(pakeA, sealed, confirmA);
    } catch (err) {
      safeFree(a.responder);
      if (errorText(err).startsWith("already-used")) {
        show({ kind: "idle" });
        return;
      }
      await postWrongCode(a.requestId);
      show({ kind: "closed", message: LINK_OLD_WRONG_CODE });
      return;
    }
    safeFree(a.responder);
    try {
      await ports.checkPayload(opened.pairingPayload, a.ownHandle);
    } catch (err) {
      await postWrongCode(a.requestId);
      show({ kind: "closed", message: ports.failureMessage(err) });
      return;
    }
    await recordAndPair(a.requestId, opened.confirmB, opened.pairingPayload);
  }

  async function recordAndPair(requestId: string, confirmB: Uint8Array, payload: Uint8Array): Promise<void> {
    show({ kind: "adding", requestId });
    try {
      await ports.result(requestId, { outcome: "accepted", confirmB });
    } catch (err) {
      if (ports.proofRefused?.(err) === true) {
        // The proof ran out while the code was being typed: keep the
        // verified exchange and ask for the proof again.
        owed = { requestId, confirmB, payload };
        show({ kind: "proving", requestId });
        return;
      }
      // Without the recorded outcome the new device never sees `confirm_b`
      // and would refuse the Welcome: pair nothing.
      show({ kind: "closed", message: LINK_RESULT_NOT_RECORDED });
      return;
    }
    try {
      await ports.pair(payload);
      show({ kind: "closed", message: LINK_DEVICE_ADDED });
    } catch (err) {
      show({ kind: "closed", message: ports.failureMessage(err) });
    }
  }

  async function postWrongCode(requestId: string): Promise<void> {
    try {
      await ports.result(requestId, { outcome: "wrong_code" });
    } catch {
      // The request expires on its own; nothing was paired either way.
    }
  }

  async function step(req: PendingDeviceRequest | null): Promise<void> {
    if (req === null) {
      drop();
      const wasAsking = asking !== null && current.kind === "asking";
      const wasProving = owed !== null && current.kind === "proving";
      asking = null;
      owed = null;
      consented.clear();
      if (wasAsking || wasProving) {
        // The request went away under the open question (17.4).
        show({ kind: "closed", message: LINK_REQUEST_EXPIRED });
        return;
      }
      closeIfShowing();
      return;
    }
    if (owed !== null && owed.requestId !== req.requestId) {
      owed = null;
      if (current.kind === "proving") show({ kind: "closed", message: LINK_REQUEST_EXPIRED });
    }
    if (active !== null && active.requestId !== req.requestId) {
      // A newer request replaced the shown one: its code is void.
      drop();
      closeIfShowing();
    }
    if (asking !== null && asking.requestId !== req.requestId) {
      // A newer request replaced the asked one.
      asking = null;
      closeIfShowing();
    }
    for (const id of consented) if (id !== req.requestId) consented.delete(id);
    if (handled.has(req.requestId)) return;
    if (req.outcome !== null) {
      handled.add(req.requestId);
      drop();
      asking = null;
      consented.delete(req.requestId);
      closeIfShowing();
      return;
    }
    if (active === null) {
      if (req.pakeB !== null) {
        // Answered by another device of this account (or by this one before
        // a reload): not offered.
        handled.add(req.requestId);
        consented.delete(req.requestId);
        if (asking !== null) {
          asking = null;
          show({ kind: "closed", message: LINK_HANDLED_ELSEWHERE });
        } else if (current.kind === "answering") {
          closeIfShowing();
        }
        return;
      }
      if (current.kind === "adding") return;
      if (consented.has(req.requestId)) {
        // "Yes, it's me" was given and the answer did not land: retry.
        await answerNew(req);
        return;
      }
      if (asking === null) {
        asking = req;
        show({ kind: "asking", requestId: req.requestId });
      }
      return;
    }
    if (req.pakeA !== null && req.sealedPayload !== null && req.confirmA !== null) {
      await finish(active, req.pakeA, req.sealedPayload, req.confirmA);
    }
  }

  return {
    async poll(): Promise<void> {
      if (busy) return;
      busy = true;
      try {
        await serial(async () => {
          await step(await ports.list());
        });
      } catch {
        // Polling is best-effort; the next tick retries.
      } finally {
        busy = false;
      }
    },
    async consent(): Promise<void> {
      const asked = asking;
      if (asked === null) return;
      try {
        await serial(async () => {
          // The question may have been replaced or dismissed meanwhile.
          if (asking === null || asking.requestId !== asked.requestId) return;
          const req = asking;
          asking = null;
          consented.add(req.requestId);
          show({ kind: "answering", requestId: req.requestId });
          await answerNew(req);
        });
      } catch {
        // answerNew does not throw; the next poll retries an offline answer.
      }
    },
    async resume(): Promise<void> {
      const pending = owed;
      if (pending === null) return;
      try {
        await serial(async () => {
          if (owed === null || owed.requestId !== pending.requestId) return;
          owed = null;
          await recordAndPair(pending.requestId, pending.confirmB, pending.payload);
        });
      } catch {
        // recordAndPair does not throw.
      }
    },
    async decline(): Promise<void> {
      const id =
        active?.requestId ??
        asking?.requestId ??
        owed?.requestId ??
        (current.kind === "answering" ? current.requestId : null);
      if (id === null) return;
      handled.add(id);
      consented.delete(id);
      drop();
      asking = null;
      owed = null;
      show({ kind: "closed", message: LINK_DECLINED_OLD });
      try {
        await ports.decline(id);
      } catch {
        // Already gone or unreachable; the request expires on its own.
      }
    },
    dismiss(): void {
      if (current.kind === "asking" && asking !== null) {
        // Closed without choosing: nothing is sent, the request is not asked
        // about again on this page and expires on its own.
        handled.add(asking.requestId);
        asking = null;
        show({ kind: "idle" });
        return;
      }
      if (current.kind === "limited" || current.kind === "closed") show({ kind: "idle" });
    },
    view: () => current,
    dispose(): void {
      drop();
      asking = null;
      owed = null;
      consented.clear();
    },
  };
}

// ---------------------------------------------------------------------------
// New device: the initiator flow
// ---------------------------------------------------------------------------

/** What the new device's code screen shows. */
export type InitiatorView =
  | { readonly kind: "requesting" }
  /** Before the old device answered: the field is disabled. */
  | { readonly kind: "waiting" }
  /** The field is enabled; `error` after a typed value that is not six
   *  digits. */
  | { readonly kind: "ready"; readonly error: LinkText | null }
  | { readonly kind: "checking" }
  /** The old device's confirmation verified: the Welcome may be trusted. */
  | { readonly kind: "confirmed" }
  | { readonly kind: "wrong-code" }
  | { readonly kind: "declined" }
  | { readonly kind: "expired" }
  /** The request could not be posted or the payload not delivered. */
  | { readonly kind: "failed"; readonly error: unknown };

/** The views after which this request is over (Start again = a new one). */
export function isTerminalInitiatorView(v: InitiatorView): boolean {
  return (
    v.kind === "wrong-code" || v.kind === "declined" || v.kind === "expired" || v.kind === "failed"
  );
}

/** The one line a terminal view shows. `failed` is the caller's to word. */
export function initiatorEndMessage(v: InitiatorView): LinkText | null {
  switch (v.kind) {
    case "wrong-code":
      return LINK_NEW_WRONG_CODE;
    case "declined":
      return LINK_DECLINED_NEW;
    case "expired":
      return LINK_EXPIRED;
    default:
      return null;
  }
}

export interface InitiatorPorts {
  readonly core: DeviceLinkCore;
  /** `POST /device/request`. */
  create(loginHandle: Uint8Array): Promise<{ requestId: string; expiresInSecs: number }>;
  /** `POST /device/request/status`. */
  status(requestId: string): Promise<DeviceRequestStatus>;
  /** `POST /device/request/payload`. */
  postPayload(
    requestId: string,
    sealed: { pakeA: Uint8Array; sealedPayload: Uint8Array; confirmA: Uint8Array },
  ): Promise<void>;
  now(): number;
  sleep(ms: number): Promise<void>;
  onView(view: InitiatorView): void;
}

export interface DeviceLinkInitiatorFlow {
  /** Post the request. Resolves with the view it reached. */
  start(): Promise<InitiatorView>;
  /** One status poll. Single-flight; never throws. */
  poll(): Promise<void>;
  /** Continue with the typed code. */
  submit(typed: string): Promise<void>;
  /** `true` once the old device's confirmation verified. The ONLY gate on
   *  trusting the Welcome and the recovery row. */
  isConfirmed(): boolean;
  /** Poll until confirmed (`true`) or over (`false`). */
  awaitConfirmation(signal?: AbortSignal, pollMs?: number): Promise<boolean>;
  view(): InitiatorView;
  dispose(): void;
}

/**
 * The new device's side. `pairingPayload` is exactly what the QR code would
 * carry (`encodePairingQr(handle, keyPackage, rendezvous)`); it leaves this
 * device only sealed.
 */
export function createDeviceLinkInitiator(
  ports: InitiatorPorts,
  args: { readonly loginHandle: Uint8Array; readonly pairingPayload: Uint8Array },
): DeviceLinkInitiatorFlow {
  let current: InitiatorView = { kind: "requesting" };
  let requestId: string | null = null;
  let expiresAtMs = Number.POSITIVE_INFINITY;
  let initiator: InitiatorHandle | null = null;
  let pakeB: Uint8Array | null = null;
  let sealed: { pakeA: Uint8Array; sealedPayload: Uint8Array; confirmA: Uint8Array } | null = null;
  let posted = false;
  let confirmed = false;
  let busy = false;

  const show = (v: InitiatorView): void => {
    current = v;
    if (isTerminalInitiatorView(v)) {
      safeFree(initiator);
      initiator = null;
    }
    ports.onView(v);
  };
  const over = (): boolean => isTerminalInitiatorView(current);

  async function sendPayload(): Promise<void> {
    if (requestId === null || sealed === null) return;
    try {
      await ports.postPayload(requestId, sealed);
      posted = true;
    } catch (err) {
      if (err instanceof AuthHttpError) {
        if (err.status === 409 && err.errorCode === "device-request-already-used") {
          // Ours landed on an earlier try whose answer was lost, or someone
          // else posted: either way the old device's outcome decides, and a
          // `confirm_b` for any other payload fails to verify here.
          posted = true;
          return;
        }
        if (err.status === 404) {
          show({ kind: "expired" });
          return;
        }
        show({ kind: "failed", error: err });
        return;
      }
      // Offline: the sealed values are kept and re-sent on the next poll.
    }
  }

  async function step(): Promise<void> {
    if (requestId === null || over() || confirmed) return;
    if (ports.now() >= expiresAtMs) {
      show({ kind: "expired" });
      return;
    }
    if (sealed !== null && !posted) {
      await sendPayload();
      if (over()) return;
    }
    const st = await ports.status(requestId);
    if (!st.pending) {
      show({ kind: ports.now() >= expiresAtMs ? "expired" : "declined" });
      return;
    }
    if (st.outcome === "wrong_code") {
      show({ kind: "wrong-code" });
      return;
    }
    if (st.outcome === "accepted") {
      if (!posted || initiator === null) {
        // An acceptance of a payload this device never sent.
        show({ kind: "wrong-code" });
        return;
      }
      if (st.confirmB === null) return;
      try {
        initiator.verifyConfirmation(st.confirmB);
      } catch {
        show({ kind: "wrong-code" });
        return;
      }
      confirmed = true;
      show({ kind: "confirmed" });
      return;
    }
    if (st.pakeB !== null && current.kind === "waiting") {
      pakeB = st.pakeB;
      show({ kind: "ready", error: null });
    }
  }

  return {
    async start(): Promise<InitiatorView> {
      show({ kind: "requesting" });
      try {
        const r = await ports.create(args.loginHandle);
        requestId = r.requestId;
        expiresAtMs = ports.now() + r.expiresInSecs * 1000;
        initiator = ports.core.newInitiator(base64UrlDecodeNoPad(r.requestId), args.loginHandle);
      } catch (err) {
        show({ kind: "failed", error: err });
        return current;
      }
      show({ kind: "waiting" });
      return current;
    },
    async poll(): Promise<void> {
      if (busy) return;
      busy = true;
      try {
        await step();
      } catch {
        // Best-effort; the next tick retries.
      } finally {
        busy = false;
      }
    },
    async submit(typed: string): Promise<void> {
      if (current.kind !== "ready" || initiator === null || pakeB === null) return;
      let code: string;
      try {
        code = ports.core.normalizeCode(typed);
      } catch {
        show({ kind: "ready", error: LINK_ENTER_SIX_DIGITS });
        return;
      }
      try {
        sealed = initiator.finish(pakeB, code, args.pairingPayload);
      } catch (err) {
        if (errorText(err).startsWith("invalid-code")) {
          show({ kind: "ready", error: LINK_ENTER_SIX_DIGITS });
        } else {
          show({ kind: "failed", error: err });
        }
        return;
      }
      show({ kind: "checking" });
      busy = true;
      try {
        await sendPayload();
      } finally {
        busy = false;
      }
    },
    isConfirmed: () => confirmed,
    async awaitConfirmation(signal?: AbortSignal, pollMs = 1500): Promise<boolean> {
      for (;;) {
        if (confirmed) return true;
        if (over() || signal?.aborted === true) return false;
        await this.poll();
        if (confirmed) return true;
        if (over()) return false;
        await ports.sleep(pollMs);
      }
    },
    view: () => current,
    dispose(): void {
      safeFree(initiator);
      initiator = null;
    },
  };
}
