// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Anonymous-token pool (ADR-0054 S3).
 *
 * A handful of actions are anonymous by design but done by a signed-in
 * person: a username or channel lookup (the OPRF evaluation), registering a
 * channel name, consuming an invite link, knocking on a group link, and the
 * four push-gw registration routes. Each spends one anonymous token in the
 * `x-lakko-anon-token` header. A token is drawn from `services/auth` WITH the
 * session (`POST /auth/tokens`, counted against the account's daily quota),
 * unblinded here, and spent later WITHOUT the session, so the verifying
 * service learns only that some rationed account produced it.
 *
 * ## The pool, refilled on a schedule (ADR-0078 section 8)
 *
 * For the signed-in account, one bucket per context, kept at about a day of
 * use plus a reserve ({@link POOL_TARGET}, never above {@link MAX_POOL}).
 * Refills run on a randomised schedule that does not look at spending: when
 * a session is installed each context is topped up at once (a fresh session
 * holds no tokens and its first actions need them), and after that at independent
 * random intervals between {@link REFILL_INTERVAL_MIN_MS} and
 * {@link REFILL_INTERVAL_MAX_MS}. A take never starts an issuance (a test
 * build alone may, see {@link anonTokenRefillOnDemandInThisBuild}). Before
 * ADR-0078 a take below a low-water mark started one, so the bearer issuance
 * landed seconds from the anonymous spend it refilled for, from the same
 * address, and the issuer and the spending service could pair them by time.
 * An action that finds its bucket empty waits only for a refill the schedule
 * already has running (bounded by {@link TAKE_WAIT_MS}), else fails with
 * {@link AnonTokenUnavailableError} (the network copy) until the next
 * scheduled refill.
 *
 * ## Issuance on its own connection
 *
 * The issuance is a bearer request with the browser's default credentials
 * mode; every spend is built by {@link anonymousFetchInit} with
 * `credentials: "omit"`. Browsers keep the two on separate connection pools
 * (Chromium's privacy-mode socket pools, Firefox's anonymous connections), so
 * an issuance and a spend do not share a keep-alive connection. A page cannot
 * do more than that.
 *
 * ## The tokens belong to the account (2026-10-01)
 *
 * The buckets in memory are the signed-in account's. An account switch drops
 * them from memory and loads the new account's own; Log out
 * ({@link AnonTokenPool.unbindIfNoSession}) drops them from memory only, so
 * the account's next sign-in on this device finds them again. Forget device
 * and Delete account remove the account's stored rows (the `anon-tokens`
 * step of `auth/wipe-account.ts`, and {@link AnonTokenPool.forgetAccount}
 * for what the sign-out spends after it), never another account's. A take
 * that started under one account and finishes after the pool moved to
 * another fails instead of handing over a token: one account's token is
 * never spent while another is signed in.
 *
 * ## Across launches (ADR-0054 S5)
 *
 * The buckets are written through to an {@link AnonTokenStore} (IndexedDB in
 * the app, `anon-token-store.ts`) under the account after every take and
 * every refill, and loaded when a session is installed: tokens whose epoch is
 * dead are dropped on load, and a take waits for the load before it decides
 * to refill. A token taken is removed from the store before it is spent, so
 * a reload never offers it again.
 *
 * ## Verification of an issuance
 *
 * The batch's DLEQ proof is checked against the key served by
 * `GET /auth/tokens/key/{context}/{epoch}` (anonymous, cached for 300 s),
 * NEVER against the `public_key` the issuance response carries: a key taken
 * from the same response that it is supposed to authenticate would let a
 * malicious issuer tag each account with its own key. A `proof-verification`
 * refusal discards the whole batch.
 *
 * ## Privacy rule (load-bearing)
 *
 * A token route never carries `Authorization`, cookies or any account
 * identifier: {@link anonymousFetchInit} builds the request from scratch with
 * `credentials: "omit"` and `referrerPolicy: "no-referrer"`. Browsers keep
 * uncredentialed requests on connections separate from credentialed ones
 * (Chromium's privacy-mode socket pools, Firefox's anonymous connections),
 * which is the closest a page can get to a separate connection pool; the page
 * cannot open one of its own.
 *
 * ## Refusals
 *
 * No refusal on a token route means a lost session - none of them carries
 * one - so nothing here ever signs anyone out. `anon-token-spent` /
 * `-invalid` / `-expired` / `-wrong-context` / `-malformed` drop that token
 * and retry ONCE with another ({@link AnonTokenPool.spend});
 * `anon-token-required` is a client bug and is surfaced as is.
 */
import { loadWasm } from "../wasm";
import { base64Decode, base64UrlEncodeNoPad } from "../auth/account-id";
import { sessionFetch } from "../auth/session-fetch";
import { tokenAccountSegment } from "../auth/session-pair";
import { defaultAnonTokenStore, type AnonTokenStore } from "./anon-token-store";

/**
 * The token contexts, in wire order: the names `anonTokenContexts()` returns
 * (`lakko_anon_token::TokenContext::ALL`, wire bytes 1..=15), pinned against
 * the wasm core by `anon-token-contexts.test.ts`. `channel-subscribe` and
 * `directory-search` are ADR-0059; `vault-claim` is ADR-0064 (nothing spends
 * it yet); `blob-upload` and `blob-read` are ADR-0065, one per hour-long
 * blob pass (`storage/blob-access.ts`);
 * `mailbox-write` is ADR-0078 section 1: a mailbox write with no reach proof;
 * `call-join` is ADR-0078 section 2: one SFU call admission;
 * `circle-claim` is ADR-0086: claiming a circle directory;
 * `channel-react` and `channel-comment` are ADR-0087 section 13a: one per
 * write under a channel post (nothing on the web spends them yet: channel
 * reactions and comments are phase B);
 * `report` is ADR-0099 section 2: one report to Lakko's moderators.
 */
export const ANON_TOKEN_CONTEXTS = [
  "discovery-lookup",
  "discovery-register",
  "invite-consume",
  "ds-knock",
  "push-register",
  "channel-subscribe",
  "directory-search",
  "vault-claim",
  "blob-upload",
  "blob-read",
  "mailbox-write",
  "call-join",
  "circle-claim",
  "channel-react",
  "channel-comment",
  "report",
] as const;

/** One token context (see {@link ANON_TOKEN_CONTEXTS}). */
export type AnonTokenContext = (typeof ANON_TOKEN_CONTEXTS)[number];

/** Tokens a context keeps on hand: about a day of use plus a reserve, so a
 *  spend finds a token without waiting for an issuance (ADR-0078 section 8).
 *  A scheduled refill tops the bucket up to this. Chosen, not measured; each
 *  stays well under the issuer's daily quota for its context. */
export const POOL_TARGET: Readonly<Record<AnonTokenContext, number>> = {
  "discovery-lookup": 32,
  "push-register": 8,
  "invite-consume": 4,
  "ds-knock": 8,
  "discovery-register": 2,
  "channel-subscribe": 4,
  "directory-search": 4,
  "vault-claim": 8,
  "blob-upload": 4,
  "blob-read": 4,
  "mailbox-write": 4,
  // ADR-0078 section 2: a join, a reconnect, the TURN credential of a
  // one-to-one call. Chosen, not measured.
  "call-join": 4,
  // ADR-0086: claiming this account's one circle directory. Chosen, not
  // measured.
  "circle-claim": 2,
  // ADR-0087 section 13a: channel reactions and comments. Chosen, not
  // measured.
  "channel-react": 4,
  "channel-comment": 4,
  // ADR-0099 section 2: reports to the moderators. Kept on hand so issuance
  // time says nothing about when a report is sent. Chosen, not measured.
  report: 2,
};

/** The pool target of `context`. */
export function poolTarget(context: AnonTokenContext): number {
  return POOL_TARGET[context];
}

/** A context's first refill after a session is installed falls at a random
 *  moment within this window. Zero since 2026-10-22: a fresh session holds no
 *  tokens, so a signup or a first sign-in whose first lookup came before a
 *  later first refill failed after {@link TAKE_WAIT_MS}. The issuer sees the
 *  session start either way. */
export const INITIAL_REFILL_SPREAD_MS = 0;
/** After that, a context is topped up at independent random intervals
 *  between these two. Chosen, not measured: short enough that a burst which
 *  empties a bucket recovers within the hour and a half, long enough that
 *  the issuer sees a handful of issuances a day per context, none of them
 *  caused by a spend. */
export const REFILL_INTERVAL_MIN_MS = 30 * 60_000;
export const REFILL_INTERVAL_MAX_MS = 90 * 60_000;

/** Never hold more than this many tokens of one context. */
export const MAX_POOL = 32;
/** How long an action waits for a refill before failing. */
export const TAKE_WAIT_MS = 8_000;
/** How long a fetched issuance key is reused (the route's own max-age). */
export const KEY_CACHE_MS = 300_000;
/** How long a `503 anon-tokens-disabled` suppresses further refills. */
const DISABLED_BACKOFF_MS = 60_000;
/** How long a failed refill (network, 5xx, a discarded batch) suppresses the
 *  background refill; a waiting action still tries once itself. */
const FAILURE_BACKOFF_MS = 10_000;

/** The route refusals that mean "this token is no good, take another". */
export const RETRYABLE_TOKEN_REFUSALS: ReadonlySet<string> = new Set([
  "anon-token-spent",
  "anon-token-invalid",
  "anon-token-expired",
  "anon-token-wrong-context",
  "anon-token-malformed",
]);

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/** The account's allowance for this context is used up for the window
 *  (`429 anon-token-quota-exhausted`). Never a sign-out. */
export class AnonTokenQuotaExhaustedError extends Error {
  override name = "AnonTokenQuotaExhaustedError";
  constructor(
    public readonly context: AnonTokenContext,
    public readonly retryAfterSecs: number | null,
  ) {
    super(`anon-token ${context}: daily allowance used up`);
  }
}

/** The server has anonymous tokens switched off (`503 anon-tokens-disabled`,
 *  from the issuer or from the spending route). Shown as the generic network
 *  failure. */
export class AnonTokensDisabledError extends Error {
  override name = "AnonTokensDisabledError";
  constructor(public readonly context: AnonTokenContext) {
    super(`anon-token ${context}: disabled on the server`);
  }
}

/** No token could be had in time: no session, a network failure, a refused
 *  or discarded issuance. Shown as the generic network failure. */
export class AnonTokenUnavailableError extends Error {
  override name = "AnonTokenUnavailableError";
  constructor(
    public readonly context: AnonTokenContext,
    public readonly reason: string,
  ) {
    super(`anon-token ${context}: no token available (${reason})`);
  }
}

/**
 * ADR-0099 step 3: the account is suspended. While it is, the restricted
 * session may draw `blob-read` tokens only (files in chats still open);
 * every other context answers `403 account-suspended`. Not a failure to
 * retry: "not now", until the suspension ends.
 */
export class AnonTokensSuspendedError extends AnonTokenUnavailableError {
  override name = "AnonTokensSuspendedError";
  constructor(context: AnonTokenContext) {
    super(context, "account suspended");
  }
}

/** The one context a suspended account's restricted session may draw. */
export const SUSPENDED_TOKEN_CONTEXT: AnonTokenContext = "blob-read";

/** Walk `cause` / `variant.cause` chains (the same shape the other
 *  `is*` helpers in this app walk) looking for an instance of `ctor`. */
function findInChain<T>(err: unknown, name: string): T | null {
  let current: unknown = err;
  for (let depth = 0; depth < 8 && current !== undefined && current !== null; depth += 1) {
    if (typeof current !== "object") return null;
    const r = current as { name?: unknown; cause?: unknown; variant?: { cause?: unknown } };
    if (r.name === name) return current as T;
    current = r.cause ?? r.variant?.cause;
  }
  return null;
}

/** The quota refusal, if `err` is or wraps one. */
export function anonTokenQuotaExhausted(err: unknown): AnonTokenQuotaExhaustedError | null {
  return findInChain<AnonTokenQuotaExhaustedError>(err, "AnonTokenQuotaExhaustedError");
}

/** The reason a take gives when the context holds no token and no refill is
 *  running: the device used the tokens it had, and a take never starts an
 *  issuance (ADR-0078 section 8). */
export const POOL_EMPTY_REASON = "pool empty until the next scheduled refill";

/** `true` when `err` is or wraps a take that found the context's pool empty
 *  with no refill running ({@link POOL_EMPTY_REASON}). The search field reads
 *  it as "out of searches" (`docs/ux/who-can-find-me.md` section 6.1). */
export function anonTokenPoolEmpty(err: unknown): boolean {
  const e = findInChain<AnonTokenUnavailableError>(err, "AnonTokenUnavailableError");
  return e !== null && e.reason === POOL_EMPTY_REASON;
}

/** `true` when `err` is or wraps a failure the UI shows as the generic
 *  network failure: tokens switched off, or no token to be had. */
export function isAnonTokenNetworkFailure(err: unknown): boolean {
  return (
    findInChain(err, "AnonTokensDisabledError") !== null ||
    findInChain(err, "AnonTokenUnavailableError") !== null
  );
}

// ---------------------------------------------------------------------------
// The wasm surface the pool needs (injectable for the tests)
// ---------------------------------------------------------------------------

/** One finalized token as the wasm core returns it. */
export interface WasmAnonToken {
  readonly context: string;
  readonly epoch: bigint;
  readonly headerValue: string;
  free(): void;
}

/** A pending blinded batch as the wasm core returns it. */
export interface WasmAnonTokenRequest {
  issueBodyJson(): string;
  finalize(evaluations: string[], proof: string, publicKey: string): WasmAnonToken[];
  free(): void;
}

/** The subset of the wasm core the pool uses. */
export interface AnonTokenWasm {
  readonly AnonTokenRequest: new (
    context: string,
    epoch: bigint,
    count: number,
  ) => WasmAnonTokenRequest;
  anonTokenEpochAt(unixSecs: bigint, windowSecs: bigint): bigint;
  anonTokenDefaultWindowSecs(): bigint;
  anonTokenIsLive(tokenEpoch: bigint, currentEpoch: bigint): boolean;
  anonTokenHeaderName(): string;
  anonTokenMaxBatch(): number;
}

// ---------------------------------------------------------------------------
// Anonymous requests
// ---------------------------------------------------------------------------

/**
 * The `RequestInit` of a token route: built from nothing, so no
 * `Authorization` can ride along, with the browser's credentials and referrer
 * withheld. `headers` must not name `authorization` or `cookie`; a caller that
 * tries is refused rather than silently stripped, because it is a bug.
 */
export function anonymousFetchInit(init: {
  readonly method: string;
  readonly headers: Readonly<Record<string, string>>;
  readonly body?: BodyInit | null;
  readonly signal?: AbortSignal;
}): RequestInit {
  for (const name of Object.keys(init.headers)) {
    const lower = name.toLowerCase();
    if (lower === "authorization" || lower === "cookie") {
      throw new Error(`anonymous request must not carry ${name}`);
    }
  }
  const out: RequestInit = {
    method: init.method,
    headers: { ...init.headers },
    credentials: "omit",
    referrerPolicy: "no-referrer",
    cache: "no-store",
  };
  if (init.body !== undefined) out.body = init.body;
  if (init.signal !== undefined) out.signal = init.signal;
  return out;
}

// ---------------------------------------------------------------------------
// The pool
// ---------------------------------------------------------------------------

/** A token ready to spend. */
export interface PooledToken {
  readonly context: AnonTokenContext;
  readonly epoch: bigint;
  readonly headerValue: string;
}

/** Dependencies, all defaulted; the tests inject their own. */
export interface AnonTokenPoolOptions {
  /** `/api/auth` in the app. */
  readonly authBaseUrl?: string;
  readonly fetchImpl?: typeof fetch;
  readonly loadWasm?: () => Promise<AnonTokenWasm>;
  /** Wall clock, unix ms. */
  readonly now?: () => number;
  readonly takeWaitMs?: number;
  /** Where the pool survives a relaunch; `null` = memory only. Defaults to
   *  IndexedDB where the environment has it. */
  readonly store?: AnonTokenStore | null;
  /** A uniform random number in [0, 1); defaults to `crypto.getRandomValues`. */
  readonly random?: () => number;
  /** The timer the refill schedule runs on; defaults to `setTimeout`. */
  readonly timers?: RefillTimers;
  /** A take that finds its bucket empty starts the issuance and waits for it.
   *  Test builds only ({@link anonTokenRefillOnDemandInThisBuild}); the default
   *  is the release rule, a take never starts one (ADR-0078 section 8). */
  readonly refillOnDemand?: boolean;
}

/**
 * Whether a take on an empty bucket may start an issuance. A build made with
 * `VITE_LAKKO_ANON_TOKEN_REFILL=on-demand` (the test stack's and the local
 * stack's) refills at once, so a walk never stops on a limit (limits have
 * tests of their own); any other build keeps the release rule.
 */
export function anonTokenRefillOnDemandInThisBuild(): boolean {
  const env = import.meta.env as Record<string, string | undefined>;
  return env["VITE_LAKKO_ANON_TOKEN_REFILL"] === "on-demand";
}

/** The timer the refill schedule runs on (injectable for the tests). */
export interface RefillTimers {
  set(fn: () => void, ms: number): unknown;
  clear(handle: unknown): void;
}

const DEFAULT_TIMERS: RefillTimers = {
  set: (fn, ms) => setTimeout(fn, ms),
  clear: (handle) => clearTimeout(handle as ReturnType<typeof setTimeout>),
};

function cryptoRandom(): number {
  const buf = new Uint32Array(1);
  crypto.getRandomValues(buf);
  return (buf[0] ?? 0) / 4_294_967_296;
}

interface Bucket {
  tokens: PooledToken[];
  refill: Promise<void> | null;
  /** Unix ms until which the quota is known to be used up. */
  exhaustedUntil: number;
  exhaustedRetryAfter: number | null;
  /** Unix ms until which the background refill is not retried. */
  backoffUntil: number;
  disabledUntil: number;
  /** ADR-0099: unix ms until which issuance for this context is not asked
   *  again, after it answered `403 account-suspended`. */
  suspendedUntil: number;
  /** The pending scheduled refill of this context. */
  timer: unknown;
  /** Why the last refill of this context failed, for the error a take
   *  reports when it finds the bucket empty. */
  lastFailure: string | null;
  /** Settles once this session's first scheduled refill of the context has
   *  run (whatever its outcome); a take on an empty bucket waits for it. */
  initial: Promise<void> | null;
  resolveInitial: (() => void) | null;
}

interface IssueResponse {
  context: string;
  epoch: number;
  evaluations: string[];
  proof: string;
  public_key: string;
  remaining: number;
  epoch_ends_at: number;
}

/** Parse `{ "error": code, ... }`, tolerating anything else. */
async function readErrorBody(
  response: Response,
): Promise<{ error?: string; current_epoch?: number; retry_after_secs?: number }> {
  try {
    const text = await response.text();
    const parsed = JSON.parse(text) as unknown;
    if (typeof parsed === "object" && parsed !== null) {
      return parsed as { error?: string; current_epoch?: number; retry_after_secs?: number };
    }
  } catch {
    /* not JSON */
  }
  return {};
}

function retryAfterOf(response: Response, body: { retry_after_secs?: number }): number | null {
  if (typeof body.retry_after_secs === "number" && Number.isFinite(body.retry_after_secs)) {
    return Math.max(0, Math.ceil(body.retry_after_secs));
  }
  const header = response.headers.get("retry-after");
  if (header === null) return null;
  const secs = Number(header.trim());
  return Number.isFinite(secs) && secs >= 0 ? Math.ceil(secs) : null;
}

/** The account segment of a session token (`v2.<account>.<gen>.<exp>.<tag>`
 *  since ADR-0073; the `v1.<account>.<exp>.<tag>` shape reads the same), or
 *  the whole token when it has no such shape. Used only to tell one account
 *  from another - a refreshed token of the SAME account must not read as a
 *  new one, or every refresh would drop the held tokens; it never leaves
 *  this module. */
function accountKey(sessionToken: string): string {
  return tokenAccountSegment(sessionToken);
}

export class AnonTokenPool {
  private readonly authBaseUrl: string;
  private readonly fetchImpl: typeof fetch;
  private readonly wasmLoader: () => Promise<AnonTokenWasm>;
  private readonly now: () => number;
  private readonly takeWaitMs: number;
  private readonly store: AnonTokenStore | null;
  private readonly random: () => number;
  private readonly timers: RefillTimers;
  private readonly refillOnDemand: boolean;

  private sessionToken: string | null = null;
  /** ADR-0099: the account is suspended; only `blob-read` is issued. */
  private suspended = false;
  /** The load of the stored tokens for the current account, once started. */
  private hydration: Promise<void> | null = null;
  /** Store operations run in order: a write queued before an account's
   *  rows are deleted lands before the delete, and a load sees every write
   *  queued ahead of it. */
  private storeChain: Promise<void> = Promise.resolve();
  private accountKey: string | null = null;
  /** Bumped whenever the account's tokens leave memory (unbind, account
   *  switch, forget): a refill or a take that started under an older
   *  generation hands nothing over. */
  private generation = 0;
  /** Server epoch minus the locally computed one, from `epoch-not-issuable`. */
  private epochCorrection = 0n;
  private readonly buckets = new Map<AnonTokenContext, Bucket>();
  private readonly keyCache = new Map<string, { key: string; fetchedAt: number }>();

  constructor(options: AnonTokenPoolOptions = {}) {
    this.authBaseUrl = options.authBaseUrl ?? "/api/auth";
    // ADR-0073: the issuance is a bearer route, so it goes through the
    // session-aware fetch (refresh ahead of expiry, one retry on a 401).
    this.fetchImpl = options.fetchImpl ?? ((input, init) => sessionFetch(input, init));
    this.wasmLoader = options.loadWasm ?? (loadWasm as unknown as () => Promise<AnonTokenWasm>);
    this.now = options.now ?? (() => Date.now());
    this.takeWaitMs = options.takeWaitMs ?? TAKE_WAIT_MS;
    this.store = options.store === undefined ? defaultAnonTokenStore() : options.store;
    this.random = options.random ?? cryptoRandom;
    this.timers = options.timers ?? DEFAULT_TIMERS;
    this.refillOnDemand = options.refillOnDemand ?? false;
  }

  // ------------------------------------------------------------------
  // Session
  // ------------------------------------------------------------------

  /**
   * The session tokens are drawn with. A different account's session drops
   * the held tokens from memory (they stay stored under their account) and
   * loads the new account's own; `null` stops drawing but keeps what is held,
   * so a sign-out can still spend one token on its push unregister before it
   * calls {@link unbindIfNoSession}.
   */
  setSessionToken(token: string | null): void {
    if (token === null) {
      this.sessionToken = null;
      return;
    }
    const key = accountKey(token);
    if (this.accountKey !== null && this.accountKey !== key) this.dropMemory();
    const fresh = this.accountKey !== key;
    this.accountKey = key;
    this.sessionToken = token;
    if (fresh || this.hydration === null) this.startHydration(key);
    if (fresh) this.startSchedule();
  }

  /**
   * ADR-0099 step 3: the account is (or is no longer) suspended. While it
   * is, the session is the restricted one and only `blob-read` is drawn: no
   * other context is refilled, and a take of one finds what is held or is
   * refused at once ("not now"), never waiting on an issuance.
   */
  setSuspended(suspended: boolean): void {
    if (this.suspended === suspended) return;
    this.suspended = suspended;
    if (!suspended) {
      for (const bucket of this.buckets.values()) bucket.suspendedUntil = 0;
    }
  }

  /** Whether issuance of `context` is held back right now. */
  private heldBack(context: AnonTokenContext, bucket: Bucket, now: number): boolean {
    return (this.suspended && context !== SUSPENDED_TOKEN_CONTEXT) || bucket.suspendedUntil > now;
  }

  /** Drop the held tokens, every refill in flight and the session from
   *  memory. The store keeps the account's rows (Log out), so its next
   *  sign-in loads them again. */
  unbind(): void {
    this.dropMemory();
  }

  /**
   * {@link unbind}, then delete `accountIdB64`'s stored rows (standard
   * base64 of the account id, the form `AuthState` carries) after every
   * write already queued (Forget device, Delete account: what the sign-out
   * spent after the wipe is written back, and must go too). Another
   * account's rows stay.
   */
  forgetAccount(accountIdB64: string): void {
    this.dropMemory();
    const store = this.store;
    let account: string;
    try {
      account = base64UrlEncodeNoPad(base64Decode(accountIdB64));
    } catch {
      return;
    }
    if (store !== null && account.length > 0) {
      void this.enqueueStore(() => store.deleteAccount(account), "delete");
    }
  }

  private dropMemory(): void {
    this.generation += 1;
    for (const bucket of this.buckets.values()) {
      if (bucket.timer !== null) this.timers.clear(bucket.timer);
      bucket.resolveInitial?.();
    }
    this.buckets.clear();
    this.sessionToken = null;
    this.accountKey = null;
    this.hydration = null;
  }

  /** Resolves once every store write queued so far has landed (tests,
   *  diagnostics). */
  async whenPersisted(): Promise<void> {
    await this.storeChain;
  }

  /** Resolves once the stored tokens of the current account are loaded (or
   *  there is nothing to load). */
  async ready(): Promise<void> {
    if (this.hydration !== null) await this.hydration;
  }

  /** {@link unbind}, unless a session has been installed since the sign-out
   *  (a sign-in that landed while the sign-out's push unregister ran). */
  unbindIfNoSession(): void {
    if (this.sessionToken === null) this.unbind();
  }

  /** How many tokens of `context` are held right now (tests, diagnostics). */
  size(context: AnonTokenContext): number {
    return this.buckets.get(context)?.tokens.length ?? 0;
  }

  // ------------------------------------------------------------------
  // Taking and spending
  // ------------------------------------------------------------------

  /**
   * Arm the refill schedule of every context for a newly installed account:
   * a first refill at once ({@link INITIAL_REFILL_SPREAD_MS} is zero).
   */
  private startSchedule(): void {
    const generation = this.generation;
    for (const context of ANON_TOKEN_CONTEXTS) {
      const bucket = this.bucket(context);
      if (bucket.timer !== null) this.timers.clear(bucket.timer);
      bucket.initial = new Promise<void>((resolve) => {
        bucket.resolveInitial = resolve;
      });
      this.armTimer(context, Math.floor(this.random() * INITIAL_REFILL_SPREAD_MS), generation);
    }
  }

  private armTimer(context: AnonTokenContext, delayMs: number, generation: number): void {
    const bucket = this.bucket(context);
    bucket.timer = this.timers.set(() => {
      bucket.timer = null;
      if (generation !== this.generation) return;
      void this.scheduledRefill(context).finally(() => {
        bucket.resolveInitial?.();
        bucket.resolveInitial = null;
        if (generation !== this.generation) return;
        const span = REFILL_INTERVAL_MAX_MS - REFILL_INTERVAL_MIN_MS;
        this.armTimer(context, REFILL_INTERVAL_MIN_MS + Math.floor(this.random() * span), generation);
      });
    }, delayMs);
  }

  /** One scheduled top-up of `context` to its target. Never throws; a
   *  failure is recorded on the bucket and the next take reports it. */
  private async scheduledRefill(context: AnonTokenContext): Promise<void> {
    await this.ready().catch(() => undefined);
    if (this.sessionToken === null) return;
    const bucket = this.bucket(context);
    const now = this.now();
    if (bucket.exhaustedUntil > now || bucket.disabledUntil > now) return;
    if (this.heldBack(context, bucket, now)) return;
    try {
      const wasm = await this.wasmLoader();
      const current = this.currentEpoch(wasm);
      bucket.tokens = bucket.tokens.filter((t) => wasm.anonTokenIsLive(t.epoch, current));
    } catch {
      return;
    }
    if (bucket.tokens.length >= poolTarget(context)) return;
    await this.refill(context).catch(() => undefined);
  }

  /** Run a scheduled top-up of every context now (what the schedule does
   *  one context at a time; tests and diagnostics). */
  async runScheduledRefills(): Promise<void> {
    await Promise.all(ANON_TOKEN_CONTEXTS.map((c) => this.scheduledRefill(c)));
  }

  /**
   * One live token of `context`, waiting for a refill if none is held.
   * Throws {@link AnonTokenQuotaExhaustedError}, {@link AnonTokensDisabledError}
   * or {@link AnonTokenUnavailableError}.
   */
  async take(context: AnonTokenContext): Promise<PooledToken> {
    // The account the take is for: if the pool moves to another account (or
    // to none) while this waits, it hands nothing over.
    const generation = this.generation;
    const moved = (): AnonTokenUnavailableError =>
      new AnonTokenUnavailableError(context, "account changed");
    await this.ready();
    const wasm = await this.wasmLoader();
    if (generation !== this.generation) throw moved();
    const bucket = this.bucket(context);
    const current = this.currentEpoch(wasm);
    bucket.tokens = bucket.tokens.filter((t) => wasm.anonTokenIsLive(t.epoch, current));
    const held = bucket.tokens.shift();
    if (held !== undefined) {
      this.persist(context);
      return held;
    }
    const now = this.now();
    if (bucket.exhaustedUntil > now) {
      throw new AnonTokenQuotaExhaustedError(context, bucket.exhaustedRetryAfter);
    }
    if (bucket.disabledUntil > now) throw new AnonTokensDisabledError(context);
    if (this.heldBack(context, bucket, now)) throw new AnonTokensSuspendedError(context);
    if (this.sessionToken === null) {
      throw new AnonTokenUnavailableError(context, "no session");
    }
    // ADR-0078 section 8: a take never starts an issuance. It may wait for
    // one the schedule already runs: this session's first refill of the
    // context, or a scheduled refill in flight. A test build alone starts
    // one here once the session's first refill has run.
    const pending =
      this.refillOnDemand && bucket.refill === null && bucket.resolveInitial === null
        ? this.refill(context)
        : (bucket.refill ?? bucket.initial);
    if (pending === null) {
      throw new AnonTokenUnavailableError(context, POOL_EMPTY_REASON);
    }
    let timer: ReturnType<typeof setTimeout> | undefined;
    const timeout = new Promise<"timeout">((resolve) => {
      timer = setTimeout(() => resolve("timeout"), this.takeWaitMs);
    });
    try {
      const outcome = await Promise.race([pending.then(() => "done" as const), timeout]);
      if (outcome === "timeout") throw new AnonTokenUnavailableError(context, "refill timed out");
      bucket.initial = null;
      const refusedNow = this.now();
      if (bucket.tokens.length === 0 && bucket.exhaustedUntil > refusedNow) {
        throw new AnonTokenQuotaExhaustedError(context, bucket.exhaustedRetryAfter);
      }
      if (bucket.tokens.length === 0 && bucket.disabledUntil > refusedNow) {
        throw new AnonTokensDisabledError(context);
      }
      if (bucket.tokens.length === 0 && bucket.lastFailure !== null) {
        throw new AnonTokenUnavailableError(context, bucket.lastFailure);
      }
    } catch (err) {
      if (
        err instanceof AnonTokenQuotaExhaustedError ||
        err instanceof AnonTokensDisabledError ||
        err instanceof AnonTokenUnavailableError
      ) {
        throw err;
      }
      throw new AnonTokenUnavailableError(context, err instanceof Error ? err.message : String(err));
    } finally {
      if (timer !== undefined) clearTimeout(timer);
    }
    if (generation !== this.generation) throw moved();
    const next = bucket.tokens.shift();
    this.persist(context);
    if (next === undefined) throw new AnonTokenUnavailableError(context, "refill produced no token");
    return next;
  }

  /**
   * Spend one token on a request: `send(headerName, headerValue)` performs
   * it (with {@link anonymousFetchInit}). A refusal that means "this token is
   * no good" drops it and retries ONCE with another. `503
   * anon-tokens-disabled` throws {@link AnonTokensDisabledError}; every
   * other response is returned for the caller to interpret.
   */
  async spend(
    context: AnonTokenContext,
    send: (headerName: string, headerValue: string) => Promise<Response>,
  ): Promise<Response> {
    const wasm = await this.wasmLoader();
    const header = wasm.anonTokenHeaderName();
    for (let attempt = 0; ; attempt += 1) {
      const token = await this.take(context);
      const response = await send(header, token.headerValue);
      if (response.status === 503 || (response.status === 401 && attempt === 0)) {
        const code = (await readErrorBody(response.clone())).error;
        if (response.status === 503 && code === "anon-tokens-disabled") {
          this.bucket(context).disabledUntil = this.now() + DISABLED_BACKOFF_MS;
          throw new AnonTokensDisabledError(context);
        }
        if (response.status === 401 && code !== undefined && RETRYABLE_TOKEN_REFUSALS.has(code)) {
          continue;
        }
      }
      return response;
    }
  }

  // ------------------------------------------------------------------
  // Internals
  // ------------------------------------------------------------------

  /** Run one store operation after the ones already queued. A failure is
   *  logged by kind only (never a token) and leaves the pool in memory. */
  private enqueueStore(op: () => Promise<void>, what: string): Promise<void> {
    const next = this.storeChain.then(op).catch((err: unknown) => {
      // eslint-disable-next-line no-console
      console.warn(`anon-token store ${what} failed`, err instanceof Error ? err.name : "unknown");
    });
    this.storeChain = next;
    return next;
  }

  /** Load the stored tokens of `account` into the buckets. */
  private startHydration(account: string): void {
    const store = this.store;
    if (store === null) {
      this.hydration = null;
      return;
    }
    const generation = this.generation;
    const run = this.enqueueStore(async () => {
      const stored = await store.load(account);
      if (generation !== this.generation) return;
      const wasm = await this.wasmLoader();
      if (generation !== this.generation) return;
      const current = this.currentEpoch(wasm);
      for (const [context, rows] of stored) {
        const live: PooledToken[] = [];
        for (const row of rows) {
          let epoch: bigint;
          try {
            epoch = BigInt(row.epoch);
          } catch {
            continue;
          }
          if (wasm.anonTokenIsLive(epoch, current)) {
            live.push({ context, epoch, headerValue: row.headerValue });
          }
        }
        const bucket = this.bucket(context);
        // Stored tokens are older than anything drawn since: spend them first.
        bucket.tokens = [...live, ...bucket.tokens].slice(0, MAX_POOL);
        // Write back, which drops the dead-epoch tokens from the store.
        if (live.length !== rows.length) this.persist(context);
      }
    }, "load");
    this.hydration = run;
  }

  /** Write one context of the current account through to the store. */
  private persist(context: AnonTokenContext): void {
    const store = this.store;
    const account = this.accountKey;
    if (store === null || account === null) return;
    // The snapshot is this account's, taken now, and is written under this
    // account even if the pool has moved on by the time it lands; a delete
    // of the account's rows queued after it still wins.
    const snapshot = (this.buckets.get(context)?.tokens ?? []).map((t) => ({
      epoch: t.epoch.toString(),
      headerValue: t.headerValue,
    }));
    void this.enqueueStore(() => store.save(account, context, snapshot), "save");
  }

  private bucket(context: AnonTokenContext): Bucket {
    let bucket = this.buckets.get(context);
    if (bucket === undefined) {
      bucket = {
        tokens: [],
        refill: null,
        exhaustedUntil: 0,
        exhaustedRetryAfter: null,
        backoffUntil: 0,
        disabledUntil: 0,
        suspendedUntil: 0,
        timer: null,
        lastFailure: null,
        initial: null,
        resolveInitial: null,
      };
      this.buckets.set(context, bucket);
    }
    return bucket;
  }

  private currentEpoch(wasm: AnonTokenWasm): bigint {
    const nowSecs = BigInt(Math.floor(this.now() / 1000));
    return wasm.anonTokenEpochAt(nowSecs, wasm.anonTokenDefaultWindowSecs()) + this.epochCorrection;
  }

  /** Single-flight refill of one context. */
  private refill(context: AnonTokenContext): Promise<void> {
    const bucket = this.bucket(context);
    if (bucket.refill !== null) return bucket.refill;
    const run = this.runRefill(context, bucket).finally(() => {
      bucket.refill = null;
    });
    bucket.refill = run;
    return run;
  }

  /**
   * Hold at least `count` live tokens of `context`, issuing now as many
   * batches as that takes (the pool cap is raised to `count` for this).
   * For a known burst of spends that cannot wait for a refill: an account
   * deletion unsubscribes every push subscription after auth no longer
   * issues. Returns how many are held after; never throws.
   */
  async ensureAtLeast(context: AnonTokenContext, count: number): Promise<number> {
    const bucket = this.bucket(context);
    try {
      await this.ready();
      if (bucket.refill !== null) await bucket.refill.catch(() => undefined);
      const wasm = await this.wasmLoader();
      const current = this.currentEpoch(wasm);
      bucket.tokens = bucket.tokens.filter((t) => wasm.anonTokenIsLive(t.epoch, current));
      while (bucket.tokens.length < count) {
        const before = bucket.tokens.length;
        const run = this.runRefill(context, bucket, count).finally(() => {
          bucket.refill = null;
        });
        bucket.refill = run;
        await run;
        if (bucket.tokens.length <= before) break;
      }
    } catch {
      /* best-effort: the caller proceeds with what is held */
    }
    return bucket.tokens.length;
  }

  private async runRefill(
    context: AnonTokenContext,
    bucket: Bucket,
    want?: number,
  ): Promise<void> {
    const session = this.sessionToken;
    if (session === null) throw new AnonTokenUnavailableError(context, "no session");
    if (this.heldBack(context, bucket, this.now())) throw new AnonTokensSuspendedError(context);
    const generation = this.generation;
    const wasm = await this.wasmLoader();
    const cap = Math.max(MAX_POOL, want ?? 0);
    // A top-up to the target, in one issuance.
    const count = Math.min(
      (want ?? poolTarget(context)) - bucket.tokens.length,
      cap - bucket.tokens.length,
      wasm.anonTokenMaxBatch(),
    );
    if (count <= 0) return;

    try {
      let epoch = this.currentEpoch(wasm);
      for (let attempt = 0; ; attempt += 1) {
        const request = new wasm.AnonTokenRequest(context, epoch, count);
        try {
          const response = await this.issue(session, request.issueBodyJson(), context);
          if (response.kind === "epoch") {
            // Our clock disagrees with the issuer's epoch: correct once.
            if (attempt > 0) throw new AnonTokenUnavailableError(context, "epoch disagreement");
            const local = epoch - this.epochCorrection;
            this.epochCorrection = response.currentEpoch - local;
            epoch = response.currentEpoch;
            continue;
          }
          const body = response.body;
          if (BigInt(body.epoch) !== epoch || body.context !== context) {
            throw new AnonTokenUnavailableError(context, "issuance for another epoch or context");
          }
          const key = await this.publicKey(context, epoch);
          let tokens: WasmAnonToken[];
          try {
            tokens = request.finalize(body.evaluations, body.proof, key);
          } catch (err) {
            // A proof that does not verify under the published key discards
            // the batch; forget the cached key so the next refill re-reads it.
            this.keyCache.delete(`${context}/${epoch}`);
            const message = err instanceof Error ? err.message : String(err);
            throw new AnonTokenUnavailableError(context, `batch discarded: ${message}`);
          }
          const pooled: PooledToken[] = tokens.map((t) => {
            const out = { context, epoch: t.epoch, headerValue: t.headerValue };
            t.free();
            return out;
          });
          if (generation !== this.generation) return; // signed out meanwhile
          bucket.lastFailure = null;
          bucket.tokens.push(...pooled);
          if (bucket.tokens.length > cap) bucket.tokens.length = cap;
          this.persist(context);
          return;
        } finally {
          request.free();
        }
      }
    } catch (err) {
      const now = this.now();
      bucket.lastFailure =
        err instanceof AnonTokenUnavailableError ? err.reason : err instanceof Error ? err.message : String(err);
      if (err instanceof AnonTokenQuotaExhaustedError) {
        bucket.exhaustedUntil = now + Math.max(1, err.retryAfterSecs ?? 60) * 1000;
        bucket.exhaustedRetryAfter = err.retryAfterSecs;
      } else if (err instanceof AnonTokensDisabledError) {
        bucket.disabledUntil = now + DISABLED_BACKOFF_MS;
      } else if (err instanceof AnonTokensSuspendedError) {
        // "Not now": not asked again before the backoff, and a take in the
        // meantime is refused at once instead of waiting on an issuance.
        bucket.suspendedUntil = now + DISABLED_BACKOFF_MS;
      } else {
        bucket.backoffUntil = now + FAILURE_BACKOFF_MS;
      }
      throw err;
    }
  }

  /** `POST /auth/tokens` WITH the session. */
  private async issue(
    session: string,
    body: string,
    context: AnonTokenContext,
  ): Promise<{ kind: "ok"; body: IssueResponse } | { kind: "epoch"; currentEpoch: bigint }> {
    let response: Response;
    try {
      response = await this.fetchImpl(`${this.authBaseUrl}/auth/tokens`, {
        method: "POST",
        headers: { "content-type": "application/json", authorization: `Bearer ${session}` },
        body,
      });
    } catch (err) {
      throw new AnonTokenUnavailableError(context, `network: ${String(err)}`);
    }
    if (response.ok) return { kind: "ok", body: (await response.json()) as IssueResponse };
    const error = await readErrorBody(response);
    if (response.status === 429 && error.error === "anon-token-quota-exhausted") {
      throw new AnonTokenQuotaExhaustedError(context, retryAfterOf(response, error));
    }
    if (response.status === 503 && error.error === "anon-tokens-disabled") {
      throw new AnonTokensDisabledError(context);
    }
    if (response.status === 403 && error.error === "account-suspended") {
      throw new AnonTokensSuspendedError(context);
    }
    if (
      response.status === 400 &&
      error.error === "epoch-not-issuable" &&
      typeof error.current_epoch === "number"
    ) {
      return { kind: "epoch", currentEpoch: BigInt(error.current_epoch) };
    }
    // A 401 here is the issuer refusing the session; the session itself is
    // judged elsewhere, never by the token pool.
    throw new AnonTokenUnavailableError(context, `issuer answered ${response.status}`);
  }

  /** `GET /auth/tokens/key/{context}/{epoch}`, anonymous, cached. */
  private async publicKey(context: AnonTokenContext, epoch: bigint): Promise<string> {
    const cacheKey = `${context}/${epoch}`;
    const cached = this.keyCache.get(cacheKey);
    if (cached !== undefined && this.now() - cached.fetchedAt < KEY_CACHE_MS) return cached.key;
    let response: Response;
    try {
      response = await this.fetchImpl(
        `${this.authBaseUrl}/auth/tokens/key/${encodeURIComponent(context)}/${epoch}`,
        anonymousFetchInit({ method: "GET", headers: {} }),
      );
    } catch (err) {
      throw new AnonTokenUnavailableError(context, `key network: ${String(err)}`);
    }
    if (!response.ok) {
      const error = await readErrorBody(response);
      if (response.status === 503 && error.error === "anon-tokens-disabled") {
        throw new AnonTokensDisabledError(context);
      }
      throw new AnonTokenUnavailableError(context, `key route answered ${response.status}`);
    }
    const json = (await response.json()) as { context?: string; epoch?: number; public_key?: string };
    if (
      typeof json.public_key !== "string" ||
      json.context !== context ||
      json.epoch === undefined ||
      BigInt(json.epoch) !== epoch
    ) {
      throw new AnonTokenUnavailableError(context, "key route answered another key");
    }
    this.keyCache.set(cacheKey, { key: json.public_key, fetchedAt: this.now() });
    return json.public_key;
  }
}

/** The app's pool. `AuthContext` installs and unbinds its session. */
export const anonTokens = new AnonTokenPool({ refillOnDemand: anonTokenRefillOnDemandInThisBuild() });
