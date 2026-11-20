// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * A supporter code (ADR-0097, `docs/ux/supporters.md` section 1): the
 * exchange of a code for a `beta-place` token, and the place held on this
 * device until the first username bind spends it.
 *
 * ## The exchange
 *
 * `POST /auth/supporter-code/redeem` (no session) takes the code, the epoch
 * and a blinded element the wasm core derives from the code
 * (`SupporterCodeRequest`); the answer is a VOPRF evaluation with a proof,
 * which the core checks against the key of `GET
 * /auth/tokens/key/beta-place/{epoch}` and turns into the
 * `x-lakko-anon-token` header value. Auth sees the code here and the token at
 * the bind, and cannot connect the two.
 *
 * ## The held place
 *
 * The header value is kept in `localStorage` under a key that names no
 * account, for the 30 days of the `beta-place` window. It is pre-account
 * state: no device key exists yet to wrap it (the persisted account marker
 * `lakko.auth.v1` is plain `localStorage` for the same reason), and the
 * ADR-0077 store gate keys on account ids, which this key does not carry, so
 * a wipe of an account leaves the place alone. It is not tied to any account.
 * The bind carries it whenever one is held; auth spends it only when the cap
 * refuses the bind (an open beta leaves it for later, spec 1.2). It is dropped
 * when the bind answers `403 supporter-code-used`.
 *
 * Every read and write is wrapped: a store that throws reads as "no place".
 */

/** The `localStorage` key of the held place. */
export const HELD_PLACE_KEY = "lakko.supporter-place.v1";

/** The token context a supporter code obtains. */
export const BETA_PLACE_CONTEXT = "beta-place";

/** How many letters and digits a code has. */
export const SUPPORTER_CODE_CHARS = 16;

/** A place held on this device. */
export interface HeldPlace {
  /** The `x-lakko-anon-token` header value. */
  readonly header: string;
  /** The `beta-place` epoch it was issued for (decimal). */
  readonly epoch: string;
  /** Unix ms after which it is no longer offered. */
  readonly untilMs: number;
}

/** The part of `Storage` this module uses. */
export type PlaceStorage = Pick<Storage, "getItem" | "setItem" | "removeItem">;

/** The app's storage, or `null` when it is not reachable. */
export function defaultPlaceStorage(): PlaceStorage | null {
  try {
    return typeof localStorage === "undefined" ? null : localStorage;
  } catch {
    return null;
  }
}

function parseHeld(raw: string | null): HeldPlace | null {
  if (raw === null) return null;
  try {
    const v = JSON.parse(raw) as { header?: unknown; epoch?: unknown; untilMs?: unknown };
    if (
      typeof v.header === "string" &&
      v.header.length > 0 &&
      typeof v.epoch === "string" &&
      /^[0-9]+$/.test(v.epoch) &&
      typeof v.untilMs === "number" &&
      Number.isFinite(v.untilMs)
    ) {
      return { header: v.header, epoch: v.epoch, untilMs: v.untilMs };
    }
  } catch {
    // fall through
  }
  return null;
}

/** The held place, or `null` (none, unreadable, or past its time; a stale or
 *  unreadable one is removed). */
export function readHeldPlace(storage: PlaceStorage | null, nowMs: number): HeldPlace | null {
  if (storage === null) return null;
  let raw: string | null;
  try {
    raw = storage.getItem(HELD_PLACE_KEY);
  } catch {
    return null;
  }
  if (raw === null) return null;
  const held = parseHeld(raw);
  if (held === null || nowMs >= held.untilMs) {
    dropHeldPlace(storage);
    return null;
  }
  return held;
}

/** Keep `place`. Returns whether it was stored. */
export function writeHeldPlace(storage: PlaceStorage | null, place: HeldPlace): boolean {
  if (storage === null) return false;
  try {
    storage.setItem(HELD_PLACE_KEY, JSON.stringify(place));
    return true;
  } catch {
    return false;
  }
}

/** Forget the held place. */
export function dropHeldPlace(storage: PlaceStorage | null): void {
  if (storage === null) return;
  try {
    storage.removeItem(HELD_PLACE_KEY);
  } catch {
    // nothing to do
  }
}

/** `Continue` is enabled once the field holds 16 letters and digits
 *  (spaces, dashes and anything else are not counted). */
export function codeLooksComplete(text: string): boolean {
  let n = 0;
  for (const c of text) {
    if (/[A-Za-z0-9]/.test(c)) n += 1;
  }
  return n === SUPPORTER_CODE_CHARS;
}

/** A failed redeem, as the screen shows it. */
export type RedeemFailure = "unknown" | "failed" | "limited";

export type RedeemOutcome =
  | { readonly kind: "held"; readonly place: HeldPlace }
  | { readonly kind: RedeemFailure };

/** How an answer of the redeem route reads. `epoch`: rebuild the request at
 *  `current_epoch` and ask once more. */
export function redeemErrorOutcome(
  status: number,
  errorCode: string | null,
): RedeemFailure | "epoch" {
  if (status === 429) return "limited";
  if (status === 400 && errorCode === "epoch-not-issuable") return "epoch";
  if (status === 404 || status === 409 || status === 410) return "unknown";
  if (status === 400 && (errorCode === "supporter-code-malformed" || errorCode === "bad-blinded-element")) {
    return "unknown";
  }
  return "failed";
}

/** The subset of the wasm core the exchange uses (injectable in tests). */
export interface SupporterCodeWasm {
  supporterCodeEpochAt(unixSecs: bigint): bigint;
  betaPlaceWindowSecs(): bigint;
  SupporterCodeRequest: new (
    code: string,
    epoch: bigint,
  ) => {
    redeemBodyJson(): string;
    finalize(evaluationB64url: string, proofB64url: string, publicKeyB64url: string): string;
    free(): void;
  };
}

export interface RedeemDeps {
  readonly wasm: SupporterCodeWasm;
  readonly fetch: typeof fetch;
  /** `/api/auth` in the app. */
  readonly authBaseUrl: string;
  readonly nowMs: () => number;
}

/** A request with no session, no cookie and no referrer. */
function anonymousInit(method: string, body?: string): RequestInit {
  const init: RequestInit = {
    method,
    headers: body !== undefined ? { "content-type": "application/json" } : {},
    credentials: "omit",
    referrerPolicy: "no-referrer",
    cache: "no-store",
  };
  if (body !== undefined) init.body = body;
  return init;
}

async function readJson(response: Response): Promise<Record<string, unknown>> {
  try {
    const v = (await response.json()) as unknown;
    return typeof v === "object" && v !== null ? (v as Record<string, unknown>) : {};
  } catch {
    return {};
  }
}

async function betaPlaceKey(deps: RedeemDeps, epoch: bigint): Promise<string | null> {
  let response: Response;
  try {
    response = await deps.fetch(
      `${deps.authBaseUrl}/auth/tokens/key/${BETA_PLACE_CONTEXT}/${epoch}`,
      anonymousInit("GET"),
    );
  } catch {
    return null;
  }
  if (!response.ok) return null;
  const json = await readJson(response);
  if (
    typeof json.public_key !== "string" ||
    json.context !== BETA_PLACE_CONTEXT ||
    (typeof json.epoch !== "number" && typeof json.epoch !== "string") ||
    BigInt(json.epoch) !== epoch
  ) {
    return null;
  }
  return json.public_key;
}

/**
 * Exchange `code` for a held place. Never throws: every failure is one of
 * the three lines of the screen. A code the core cannot parse is `unknown`.
 */
export async function redeemSupporterCode(deps: RedeemDeps, code: string): Promise<RedeemOutcome> {
  let epoch = deps.wasm.supporterCodeEpochAt(BigInt(Math.floor(deps.nowMs() / 1000)));
  for (let attempt = 0; attempt < 2; attempt += 1) {
    let request: InstanceType<SupporterCodeWasm["SupporterCodeRequest"]>;
    try {
      request = new deps.wasm.SupporterCodeRequest(code, epoch);
    } catch {
      return { kind: "unknown" };
    }
    try {
      let response: Response;
      try {
        response = await deps.fetch(
          `${deps.authBaseUrl}/auth/supporter-code/redeem`,
          anonymousInit("POST", request.redeemBodyJson()),
        );
      } catch {
        return { kind: "failed" };
      }
      const json = await readJson(response);
      if (!response.ok) {
        const outcome = redeemErrorOutcome(
          response.status,
          typeof json.error === "string" ? json.error : null,
        );
        if (outcome !== "epoch") return { kind: outcome };
        const current = json.current_epoch;
        if (attempt > 0 || (typeof current !== "number" && typeof current !== "string")) {
          return { kind: "failed" };
        }
        epoch = BigInt(current);
        continue;
      }
      if (
        typeof json.evaluation !== "string" ||
        typeof json.proof !== "string" ||
        json.context !== BETA_PLACE_CONTEXT
      ) {
        return { kind: "failed" };
      }
      const key = await betaPlaceKey(deps, epoch);
      if (key === null) return { kind: "failed" };
      let header: string;
      try {
        header = request.finalize(json.evaluation, json.proof, key);
      } catch {
        // The proof did not check against the published key.
        return { kind: "failed" };
      }
      const windowMs = Number(deps.wasm.betaPlaceWindowSecs()) * 1000;
      return {
        kind: "held",
        place: { header, epoch: epoch.toString(), untilMs: deps.nowMs() + windowMs },
      };
    } finally {
      try {
        request.free();
      } catch {
        // already freed
      }
    }
  }
  return { kind: "failed" };
}
