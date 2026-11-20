// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The one session-token provider (ADR-0073 section 3.3).
 *
 * Every authenticated client of this app (auth, discovery, storage,
 * gif-proxy, the call JWT and TURN mints, the backup head, the anonymous
 * token issuance, the WebAuthn routes) sends its bearer through
 * {@link SessionTokenProvider.fetch} (exported as `sessionFetch` in
 * `session-fetch.ts`). The wrapper is the only place the three rules of the
 * ADR are applied, so no client carries its own copy:
 *
 *   1. before a request the access token is refreshed when it has at most
 *      `REFRESH_AHEAD_MS` left (`tokenDecision`), one refresh in flight per
 *      account, callers queue behind it;
 *   2. a bearer route answering `401` makes the request try ONE refresh and
 *      go out once more; a second `401` is reported as it always was
 *      (`bearerRefusal`);
 *   3. `401 key-generation-superseded`, from the refresh route or from any
 *      bearer route, raises `superseded` once: the device signs out and
 *      wipes (the listener in `AuthContext` runs it); `refresh-invalid`
 *      raises `refresh-invalid`: the sign-in screen, nothing wiped;
 *   4. `401 device-revoked` (ADR-0074), from the refresh route or from any
 *      bearer route, raises `device-revoked` once: this device was removed
 *      from the account, signs out and wipes everything.
 *   5. `403 account-suspended` (ADR-0099 section 4): from a bearer route of
 *      auth it costs one refresh; the refresh answers with a restricted
 *      session, which the provider holds in place of the pair and announces
 *      as `suspended`. While it holds one, every request this account's
 *      tokens would carry is answered here, locally, with the same `403`,
 *      except the few routes a suspended account may use, which go out with
 *      the live restricted token. A refresh that answers a pair again ends
 *      it (`pair`).
 *
 * A caller may pass a token the provider has since replaced (a component
 * that captured `identitySecrets().sessionToken` before a refresh): the
 * wrapper substitutes the live one, so a stale capture never costs a 401.
 * A bearer the provider does not know (the refresh credential itself, a
 * token of another page's session) passes through untouched.
 *
 * The provider holds the pair in memory and persists every new pair through
 * the `persist` port (the device-key-wrapped session store), so a reload
 * resumes from the newest refresh credential. It imports nothing but pure
 * modules: the refresh transport and the persistence are injected by
 * `AuthContext`, which also subscribes to the events.
 */
import { bearerRefusal, CODE_ACCOUNT_SUSPENDED, CODE_KEY_GENERATION_SUPERSEDED, errorCodeOf, refreshRefusal } from "./session-401";
import { REFRESH_AHEAD_MS, tokenDecision, type SessionPair } from "./session-pair";
import type { RestrictedSession } from "./suspension";

/** What the refresh transport answers. A network failure throws. */
export type RefreshTransportResult =
  | { readonly kind: "pair"; readonly pair: SessionPair }
  /** ADR-0099: `403 account-suspended` with the restricted session. */
  | { readonly kind: "suspended"; readonly restricted: RestrictedSession }
  | { readonly kind: "refused"; readonly status: number; readonly errorCode: string | null };

/** The ports `AuthContext` injects. */
export interface SessionTokenProviderPorts {
  /** `POST /auth/session/refresh` with the credential. */
  readonly refresh: (refreshToken: string) => Promise<RefreshTransportResult>;
  /** Persist a refreshed pair under the device key. Best effort: a failure
   *  is logged by the port, the in-memory pair still serves this page. */
  readonly persist: (accountIdB64: string, pair: SessionPair) => Promise<void>;
  /** Persist a restricted session (ADR-0099) the same way. */
  readonly persistRestricted?: (accountIdB64: string, restricted: RestrictedSession) => Promise<void>;
  readonly now?: () => number;
  readonly fetchImpl?: typeof fetch;
}

/** What one refresh came to. */
export type RefreshOutcome =
  /** A new pair is installed. */
  | "refreshed"
  /** The account's keys were reset elsewhere: the pair is dropped and
   *  `superseded` was raised. */
  | "superseded"
  /** This device was removed from the account (ADR-0074): the pair is
   *  dropped and `device-revoked` was raised. */
  | "device-revoked"
  /** The credential is bad or expired: the pair is dropped and
   *  `refresh-invalid` was raised. */
  | "invalid"
  /** The request never got an answer; the pair is kept. */
  | "network"
  /** ADR-0099: the account is suspended. The restricted session is held in
   *  place of the pair and `suspended` was raised. */
  | "suspended"
  /** No pair is installed, or the refresh credential has expired. */
  | "none";

/** What the provider tells its subscribers. */
export type SessionEvent =
  | { readonly kind: "pair"; readonly accountIdB64: string; readonly pair: SessionPair }
  | { readonly kind: "superseded"; readonly accountIdB64: string }
  | { readonly kind: "device-revoked"; readonly accountIdB64: string }
  | { readonly kind: "refresh-invalid"; readonly accountIdB64: string }
  /** ADR-0099: the refresh answered `403 account-suspended`. Raised on every
   *  such answer (the periodic check included), with the newest restricted
   *  session; the listener decides whether anything changes. */
  | {
      readonly kind: "suspended";
      readonly accountIdB64: string;
      readonly restricted: RestrictedSession;
    };

/**
 * The routes a restricted session opens (ADR-0099 section 4), as `METHOD
 * path` with the `/api/<service>` prefix every client of this app uses (the
 * web through nginx, the desktop through its host). Every other request
 * carrying a token of a suspended account is answered locally.
 *
 * Auth: read the suspension, send one appeal, delete the account, sign out,
 * the passkey ceremony of the deletion, and the anonymous-token issuance
 * (`blob-read` only, decided by auth: files in chats still open). Discovery
 * and storage: the deletion of the account's rows and of its backup.
 */
export const RESTRICTED_ROUTES: readonly string[] = [
  "GET /api/auth/account/suspension",
  "POST /api/auth/account/suspension/appeal",
  "DELETE /api/auth/account",
  "POST /api/auth/auth/session/logout",
  "POST /api/auth/auth/action/start",
  "POST /api/auth/auth/action/finish",
  "POST /api/auth/auth/tokens",
  "DELETE /api/discovery/discovery/account",
  "DELETE /api/storage/backup",
];

/** The local answer to a request a suspended account may not make. */
function suspendedResponse(): Response {
  return new Response(JSON.stringify({ error: CODE_ACCOUNT_SUSPENDED }), {
    status: 403,
    headers: { "content-type": "application/json" },
  });
}

type Listener = (event: SessionEvent) => void;

export class SessionTokenProvider {
  private ports: SessionTokenProviderPorts | null = null;
  private account: string | null = null;
  private held: SessionPair | null = null;
  /** ADR-0099: the restricted session held in place of the pair while the
   *  account is suspended. Never both. */
  private restrictedHeld: RestrictedSession | null = null;
  /** Access tokens this page replaced through a refresh: a caller holding
   *  one still gets the live token. Bounded by the number of refreshes in a
   *  page lifetime (one per 15 minutes by design). */
  private readonly replaced = new Set<string>();
  private inflight: Promise<RefreshOutcome> | null = null;
  /** ADR-0074 section 3.3: the pair that was held when the server answered
   *  `key-generation-superseded`. A kept device exchanges its refresh
   *  credential for a pair at the new generation with a statement by the new
   *  account key; a device that cannot is signed out as before. */
  private supersededHeld: { readonly accountIdB64: string; readonly pair: SessionPair } | null = null;
  private readonly listeners = new Set<Listener>();
  /** ADR-0098: the account auth just deleted on this device's own request.
   *  From then on nothing refreshes and no refusal is raised: the deletion
   *  goes on to its cleanups (the conversations are told) and its own wipe,
   *  and a sign-out raised by a cleanup's `401` would stop it half-way. */
  private deletedAccount: string | null = null;

  /** Install (or replace) the ports. Called once by `AuthContext`. */
  configure(ports: SessionTokenProviderPorts): void {
    this.ports = ports;
  }

  /** The session landed (or a landing reinstalled it): hold this pair for
   *  `accountIdB64`. Replaces whatever was held; tokens of a previous pair
   *  of the same account keep resolving to the live one. */
  install(accountIdB64: string, pair: SessionPair): void {
    this.supersededHeld = null;
    this.deletedAccount = null;
    if (this.account !== accountIdB64) this.replaced.clear();
    if (this.held !== null && this.held.sessionToken !== pair.sessionToken) {
      this.replaced.add(this.held.sessionToken);
    }
    // The suspension ended: a capture of the restricted token resolves to
    // the live access token from here on.
    if (this.restrictedHeld !== null && this.account === accountIdB64) {
      this.replaced.add(this.restrictedHeld.restrictedToken);
    }
    this.restrictedHeld = null;
    this.account = accountIdB64;
    this.held = pair;
  }

  /**
   * ADR-0099: hold `restricted` for `accountIdB64` in place of a pair (a
   * landing answered `403 account-suspended`, or a refresh did). The access
   * token held until now, and an earlier restricted token, keep resolving to
   * the live restricted session on the routes it opens and are answered
   * locally everywhere else.
   */
  installRestricted(accountIdB64: string, restricted: RestrictedSession): void {
    this.supersededHeld = null;
    if (this.account !== accountIdB64) this.replaced.clear();
    else {
      if (this.held !== null) this.replaced.add(this.held.sessionToken);
      if (this.restrictedHeld !== null && this.restrictedHeld.restrictedToken !== restricted.restrictedToken) {
        this.replaced.add(this.restrictedHeld.restrictedToken);
      }
    }
    this.account = accountIdB64;
    this.held = null;
    this.restrictedHeld = restricted;
  }

  /** The restricted session held while the account is suspended, or `null`. */
  restricted(): RestrictedSession | null {
    return this.restrictedHeld;
  }

  /** Sign-out, wipe, or a session that ended: hold nothing. */
  clear(): void {
    this.deletedAccount = null;
    this.account = null;
    this.held = null;
    this.restrictedHeld = null;
    this.replaced.clear();
  }

  /** The held pair, or `null`. */
  pair(): SessionPair | null {
    return this.held;
  }

  /** The account the held pair belongs to, or `null`. */
  accountIdB64(): string | null {
    return this.account;
  }

  /** The live access token without any refresh, or `null`. For callers that
   *  must read synchronously; the fetch wrapper refreshes it on the way out. */
  peek(): string | null {
    return this.held?.sessionToken ?? null;
  }

  /** The generation the held pair was issued under, or `null`. */
  keyGeneration(): bigint | null {
    return this.held?.keyGeneration ?? null;
  }

  /** The pair that was superseded, for `accountIdB64` (ADR-0074 section
   *  3.3); `null` when none is kept. Dropped by the next `install`. */
  supersededPair(accountIdB64: string): SessionPair | null {
    return this.supersededHeld !== null && this.supersededHeld.accountIdB64 === accountIdB64
      ? this.supersededHeld.pair
      : null;
  }

  /** `true` for the live access token and for any access token of this
   *  account a refresh replaced in this page. */
  manages(token: string): boolean {
    if (this.held === null) return false;
    return this.held.sessionToken === token || this.replaced.has(token);
  }

  /** Whether a refresh could be attempted right now. */
  refreshAvailable(): boolean {
    if (this.restrictedHeld !== null) return this.restrictedHeld.refreshExpiresAtMs > this.now();
    return (
      this.held !== null && tokenDecision(this.held, this.now()) !== "refresh-expired"
    );
  }

  /**
   * ADR-0099: the restricted token to send now, refreshed when it has at
   * most `REFRESH_AHEAD_MS` left. `null` when none is held, when its refresh
   * credential expired, when the refresh was refused, or when the refresh
   * answered a pair (the suspension ended; the caller reads `peek()`).
   */
  async currentRestricted(): Promise<string | null> {
    const held = this.restrictedHeld;
    if (held === null) return null;
    const now = this.now();
    if (held.refreshExpiresAtMs <= now) return null;
    if (held.restrictedExpiresAtMs - now > REFRESH_AHEAD_MS) return held.restrictedToken;
    const outcome = await this.refreshOnce();
    if (outcome === "suspended") return this.restrictedHeld?.restrictedToken ?? null;
    if (outcome === "network" && held.restrictedExpiresAtMs > this.now()) return held.restrictedToken;
    return null;
  }

  /**
   * The access token to send now: the held one when it has more than
   * `REFRESH_AHEAD_MS` left, else the one a refresh yields. `null` when no
   * pair is held, when the refresh credential expired, when the refresh was
   * refused, or when it could not be reached and the held token has
   * expired (a still-valid held token is returned on a network failure, so
   * an offline device keeps working until it really expires).
   */
  async current(): Promise<string | null> {
    const held = this.held;
    if (held === null) return null;
    const decision = tokenDecision(held, this.now());
    if (decision === "use-current") return held.sessionToken;
    if (decision === "refresh-expired") return null;
    const outcome = await this.refreshOnce();
    if (outcome === "refreshed") return this.held?.sessionToken ?? null;
    if (outcome === "network" && held.sessionExpiresAtMs > this.now()) return held.sessionToken;
    return null;
  }

  /**
   * One refresh for the held pair, shared by every caller that asks while
   * it is in flight. Drops the pair and raises the matching event on a
   * refusal; keeps it on a network failure.
   */
  /**
   * ADR-0098: auth deleted the held account at this device's request. The
   * held pair stays (storage and discovery verify it statelessly for their
   * own deletions), but from now on no refresh is made and no refusal is
   * raised, so a cleanup that meets a `401` cannot sign the device out
   * before the account-deleted statement is posted and the wipe runs.
   * Cleared by the next `install` or `clear`.
   */
  markAccountDeleted(): void {
    this.deletedAccount = this.account;
  }

  /** Whether `markAccountDeleted` holds for the account held now. */
  private accountWasDeleted(): boolean {
    return this.deletedAccount !== null && this.deletedAccount === this.account;
  }

  refreshOnce(): Promise<RefreshOutcome> {
    if (this.accountWasDeleted()) return Promise.resolve("invalid");
    if (this.inflight !== null) return this.inflight;
    const run = this.runRefresh().finally(() => {
      this.inflight = null;
    });
    this.inflight = run;
    return run;
  }

  private async runRefresh(): Promise<RefreshOutcome> {
    const ports = this.ports;
    const held = this.held;
    const restricted = this.restrictedHeld;
    const account = this.account;
    if (ports === null || account === null) return "none";
    const credential = held ?? restricted;
    if (credential === null) return "none";
    if (credential.refreshExpiresAtMs <= this.now()) return "none";
    let result: RefreshTransportResult;
    try {
      result = await ports.refresh(credential.refreshToken);
    } catch {
      return "network";
    }
    // A sign-out or another landing while the refresh was out owns the
    // state now; a late answer must not overwrite it.
    if (this.held !== held || this.restrictedHeld !== restricted || this.account !== account) {
      if (this.held !== null) return "refreshed";
      return this.restrictedHeld !== null ? "suspended" : "none";
    }
    if (result.kind === "suspended") {
      this.installRestricted(account, result.restricted);
      void ports.persistRestricted?.(account, result.restricted).catch(() => {
        /* the port logs */
      });
      this.emit({ kind: "suspended", accountIdB64: account, restricted: result.restricted });
      return "suspended";
    }
    if (result.kind === "refused") {
      if (result.status !== 401) return "network";
      const verdict = refreshRefusal(result.errorCode);
      this.clear();
      if (verdict === "superseded" && held !== null) {
        this.supersededHeld = { accountIdB64: account, pair: held };
      }
      this.emit(
        verdict === "superseded"
          ? { kind: "superseded", accountIdB64: account }
          : verdict === "device-revoked"
            ? { kind: "device-revoked", accountIdB64: account }
            : { kind: "refresh-invalid", accountIdB64: account },
      );
      return verdict;
    }
    this.install(account, result.pair);
    void ports.persist(account, result.pair).catch(() => {
      /* the port logs */
    });
    this.emit({ kind: "pair", accountIdB64: account, pair: result.pair });
    return "refreshed";
  }

  /**
   * A bearer route of some service answered `401 key-generation-superseded`
   * to the held token: drop the pair and raise `superseded` once. The fetch
   * wrapper calls this; a client that parses its own responses may too.
   */
  reportSuperseded(): void {
    const account = this.account;
    if (account === null || this.accountWasDeleted()) return;
    const held = this.held;
    this.clear();
    if (held !== null) this.supersededHeld = { accountIdB64: account, pair: held };
    this.emit({ kind: "superseded", accountIdB64: account });
  }

  /**
   * A bearer route of auth answered `401 device-revoked` to the held token
   * (ADR-0074 section 0.4): this device was removed from the account. Drop
   * the pair and raise `device-revoked` once.
   */
  reportDeviceRevoked(): void {
    const account = this.account;
    if (account === null || this.accountWasDeleted()) return;
    this.clear();
    this.emit({ kind: "device-revoked", accountIdB64: account });
  }

  /** `true` for the live restricted token and for any token of this account
   *  it replaced (the access token held before the suspension, an older
   *  restricted token). */
  private ownsSuspendedToken(token: string): boolean {
    const r = this.restrictedHeld;
    if (r === null) return false;
    return r.restrictedToken === token || this.replaced.has(token);
  }

  /**
   * ADR-0099: a request carrying a token of the suspended account. The routes
   * a restricted session opens go out with the live restricted token (or,
   * when the refresh just ended the suspension, the new access token); every
   * other one is answered here with `403 account-suspended` and never
   * leaves the device.
   */
  private async fetchWhileSuspended(
    input: RequestInfo | URL,
    init: RequestInit | undefined,
    headers: Headers,
    fetchImpl: typeof fetch,
  ): Promise<Response> {
    const route = routeOf(input, init?.method);
    if (route === null || !RESTRICTED_ROUTES.includes(route)) return suspendedResponse();
    const token = (await this.currentRestricted()) ?? this.peek();
    if (token === null) return suspendedResponse();
    const h = new Headers(headers);
    h.set("authorization", `Bearer ${token}`);
    return fetchImpl(input, { ...init, headers: h });
  }

  subscribe(listener: Listener): () => void {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  }

  private emit(event: SessionEvent): void {
    for (const listener of [...this.listeners]) {
      try {
        listener(event);
      } catch (err) {
        // eslint-disable-next-line no-console
        console.warn("session listener threw", err);
      }
    }
  }

  private now(): number {
    return this.ports?.now?.() ?? Date.now();
  }

  // -------------------------------------------------------------------
  // The session-aware fetch
  // -------------------------------------------------------------------

  /**
   * `fetch` for a bearer request. A request whose `Authorization` carries a
   * token this provider manages goes out with the live token (refreshed
   * first when due) and is sent once more after one refresh on a `401`;
   * any other request passes through untouched.
   */
  async fetch(input: RequestInfo | URL, init?: RequestInit): Promise<Response> {
    const fetchImpl = this.ports?.fetchImpl ?? ((i, o) => globalThis.fetch(i, o));
    const headers = new Headers(init?.headers);
    const sent = bearerOf(headers.get("authorization"));
    if (sent !== null && this.restrictedHeld !== null && this.ownsSuspendedToken(sent)) {
      return this.fetchWhileSuspended(input, init, headers, fetchImpl);
    }
    if (sent === null || !this.manages(sent)) return fetchImpl(input, init);

    const send = (token: string): Promise<Response> => {
      const h = new Headers(headers);
      h.set("authorization", `Bearer ${token}`);
      return fetchImpl(input, { ...init, headers: h });
    };

    const live = (await this.current()) ?? this.peek() ?? sent;
    const first = await send(live);
    const verdict = bearerRefusal({
      status: first.status,
      errorCode:
        first.status === 401 || first.status === 403 ? errorCodeOf(await peekBody(first)) : null,
      retried: false,
      refreshAvailable: this.refreshAvailable(),
    });
    if (verdict === "pass" || verdict === "report") return first;
    if (verdict === "suspended") {
      // The refresh answers with the restricted session and raises
      // `suspended`; this request is refused as it is.
      await this.refreshOnce();
      return first;
    }
    if (verdict === "superseded") {
      // An answer to a token this device has since replaced says nothing
      // about the one it holds: this device's own key rotation looks exactly
      // like this. The held token decides.
      const held = this.peek();
      if (held === null || held === live) {
        this.reportSuperseded();
        return first;
      }
      const again = await send(held);
      if (
        again.status === 401 &&
        errorCodeOf(await peekBody(again)) === CODE_KEY_GENERATION_SUPERSEDED
      ) {
        this.reportSuperseded();
      }
      return again;
    }
    if (verdict === "device-revoked") {
      this.reportDeviceRevoked();
      return first;
    }
    const outcome = await this.refreshOnce();
    const next = this.peek();
    if (outcome !== "refreshed" || next === null) return first;
    const second = await send(next);
    if (second.status === 401) {
      const again = bearerRefusal({
        status: 401,
        errorCode: errorCodeOf(await peekBody(second)),
        retried: true,
        refreshAvailable: false,
      });
      if (again === "superseded") this.reportSuperseded();
      if (again === "device-revoked") this.reportDeviceRevoked();
    }
    return second;
  }
}

/** `METHOD path` of a request (the path without query), or `null` when the
 *  URL cannot be read. */
export function routeOf(input: RequestInfo | URL, method: string | undefined): string | null {
  let raw: string;
  if (typeof input === "string") raw = input;
  else if (input instanceof URL) raw = input.href;
  else raw = input.url;
  let path: string;
  try {
    path = new URL(raw, "http://local.invalid").pathname;
  } catch {
    return null;
  }
  return `${(method ?? "GET").toUpperCase()} ${path}`;
}

/** The token of a `Bearer <token>` header value, or `null`. */
function bearerOf(value: string | null): string | null {
  if (value === null) return null;
  const match = /^Bearer\s+(\S+)$/i.exec(value.trim());
  return match?.[1] ?? null;
}

/** The body of a response, read from a clone so the caller still can. */
async function peekBody(response: Response): Promise<string | null> {
  try {
    return await response.clone().text();
  } catch {
    return null;
  }
}

/** The app's provider: one per page, shared by every client. */
export const sessionTokens = new SessionTokenProvider();
