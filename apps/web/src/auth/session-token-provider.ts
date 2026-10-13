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
import { bearerRefusal, errorCodeOf, refreshRefusal } from "./session-401";
import { tokenDecision, type SessionPair } from "./session-pair";

/** What the refresh transport answers. A network failure throws. */
export type RefreshTransportResult =
  | { readonly kind: "pair"; readonly pair: SessionPair }
  | { readonly kind: "refused"; readonly status: number; readonly errorCode: string | null };

/** The ports `AuthContext` injects. */
export interface SessionTokenProviderPorts {
  /** `POST /auth/session/refresh` with the credential. */
  readonly refresh: (refreshToken: string) => Promise<RefreshTransportResult>;
  /** Persist a refreshed pair under the device key. Best effort: a failure
   *  is logged by the port, the in-memory pair still serves this page. */
  readonly persist: (accountIdB64: string, pair: SessionPair) => Promise<void>;
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
  /** No pair is installed, or the refresh credential has expired. */
  | "none";

/** What the provider tells its subscribers. */
export type SessionEvent =
  | { readonly kind: "pair"; readonly accountIdB64: string; readonly pair: SessionPair }
  | { readonly kind: "superseded"; readonly accountIdB64: string }
  | { readonly kind: "device-revoked"; readonly accountIdB64: string }
  | { readonly kind: "refresh-invalid"; readonly accountIdB64: string };

type Listener = (event: SessionEvent) => void;

export class SessionTokenProvider {
  private ports: SessionTokenProviderPorts | null = null;
  private account: string | null = null;
  private held: SessionPair | null = null;
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

  /** Install (or replace) the ports. Called once by `AuthContext`. */
  configure(ports: SessionTokenProviderPorts): void {
    this.ports = ports;
  }

  /** The session landed (or a landing reinstalled it): hold this pair for
   *  `accountIdB64`. Replaces whatever was held; tokens of a previous pair
   *  of the same account keep resolving to the live one. */
  install(accountIdB64: string, pair: SessionPair): void {
    this.supersededHeld = null;
    if (this.account !== accountIdB64) this.replaced.clear();
    if (this.held !== null && this.held.sessionToken !== pair.sessionToken) {
      this.replaced.add(this.held.sessionToken);
    }
    this.account = accountIdB64;
    this.held = pair;
  }

  /** Sign-out, wipe, or a session that ended: hold nothing. */
  clear(): void {
    this.account = null;
    this.held = null;
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
    return (
      this.held !== null && tokenDecision(this.held, this.now()) !== "refresh-expired"
    );
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
  refreshOnce(): Promise<RefreshOutcome> {
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
    const account = this.account;
    if (ports === null || held === null || account === null) return "none";
    if (held.refreshExpiresAtMs <= this.now()) return "none";
    let result: RefreshTransportResult;
    try {
      result = await ports.refresh(held.refreshToken);
    } catch {
      return "network";
    }
    // A sign-out or another landing while the refresh was out owns the
    // state now; a late answer must not overwrite it.
    if (this.held !== held || this.account !== account) {
      return this.held === null ? "none" : "refreshed";
    }
    if (result.kind === "refused") {
      if (result.status !== 401) return "network";
      const verdict = refreshRefusal(result.errorCode);
      this.clear();
      if (verdict === "superseded") this.supersededHeld = { accountIdB64: account, pair: held };
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
    if (account === null) return;
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
    if (account === null) return;
    this.clear();
    this.emit({ kind: "device-revoked", accountIdB64: account });
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
      errorCode: first.status === 401 ? errorCodeOf(await peekBody(first)) : null,
      retried: false,
      refreshAvailable: this.refreshAvailable(),
    });
    if (verdict === "pass" || verdict === "report") return first;
    if (verdict === "superseded") {
      this.reportSuperseded();
      return first;
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
