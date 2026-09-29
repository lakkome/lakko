// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `DsClient` — fetch-based client for `lakko-svc-ds`.
 *
 * The Delivery Service is a thin append-only relay: every
 * message goes into either a per-group journal
 * (`/ds/group/{group_hex}`) or a per-mailbox journal
 * (`/ds/mailbox/{device_hex}`). Both paths key the journal by a
 * 32-byte pseudonym encoded as **lowercase hex** in the URL.
 *
 * Two endpoints per journal kind:
 *
 *   * `POST /ds/{kind}/{pseudonym_hex}/messages`
 *     Body: raw bytes (`application/octet-stream`),
 *     capped server-side at `MAX_MESSAGE_BYTES`.
 *     Response: `{ id, created_at }` — the assigned monotonic
 *     id is the cursor every reader uses on follow-up GETs.
 *
 *   * `GET /ds/{kind}/{pseudonym_hex}/messages?after=&limit=&wait=`
 *     Response: `{ messages: [{ id, payload_b64, created_at }],
 *                  has_more }`. `wait` is the long-poll budget
 *     (seconds) — 0 returns immediately whatever's there.
 *
 * ADR-0053 - a CLAIMED journal answers only a request that carries an
 * `x-lakko-ds-proof` header signed under a key the journal was claimed with:
 * a per-epoch key derived from the MLS group for a group journal, the
 * identity key for a mailbox read. The signer comes from the registry in
 * `./access` (or is passed explicitly); the path signed is the service path,
 * never the `/api/ds` prefix, and the query is signed exactly as sent. A
 * refused group request is retried under the previous epoch's key, then once
 * after a claim (contract point 5). Mailbox writes and the knock stay
 * unsigned; the knock spends one anonymous `ds-knock` token instead
 * (ADR-0054 S3) and carries no credentials.
 *
 * ADR-0054 - a group publish that should wake the recipients carries an
 * `x-lakko-wake` header (built by `./wake`); the DS forwards the wake to
 * push-gw after the proof gate passed and the row was appended. No header, no
 * wake. The client never calls push-gw to wake.
 */
import { base64Decode, base64Encode } from "../auth/account-id";
import {
  DS_PROOF_HEADER,
  dsAccess,
  isClockSkewed,
  parseHttpDate,
  type DsAccessRegistry,
  type ProofSigner,
} from "./access";
import { DS_WAKE_HEADER, dsWakeHeaderValue, type DsWake } from "./wake";
import {
  anonTokens,
  anonymousFetchInit,
  type AnonTokenPool,
} from "../tokens/anon-token-pool";

export { DS_WAKE_HEADER, dsWakeHeaderValue, type DsWake, type DsWakeKind } from "./wake";

/** The header a reach-key delegation travels in (`delegationHeaderName()` on
 *  the wasm surface; pinned against it by `client.reach.test.ts`). */
export const DS_DELEGATION_HEADER = "x-lakko-ds-delegation";

/**
 * How a write to someone else's identity mailbox is authorised (ADR-0058
 * section 2). `signer` is a reach signer (`DsProofSigner.forReach(seed)`) of a
 * key the owner registered, or the delegate's signer next to `delegation`.
 * Absent: the write is unsigned, which an open (Everyone) or unclaimed
 * mailbox stores and a closed one silently drops.
 */
export interface MailboxWriteAuth {
  readonly signer?: ProofSigner | null;
  /** The `x-lakko-ds-delegation` header value. */
  readonly delegation?: string | null;
}

/** One change to the reach keys of this device's own mailbox. */
export interface MailboxReachUpdate {
  readonly add?: readonly { readonly publicKey: Uint8Array; readonly expiresAtMs?: number | null }[];
  readonly remove?: readonly Uint8Array[];
  /** Set the Everyone flag; absent leaves it unchanged. */
  readonly open?: boolean;
}

/** The JSON body of `PUT /ds/mailbox/{hex}/reach`. Exported for the test. */
export function reachUpdateBody(update: MailboxReachUpdate): {
  add: { reach_pk_b64: string; expires_at_ms?: number }[];
  remove: string[];
  open?: boolean;
} {
  const body: {
    add: { reach_pk_b64: string; expires_at_ms?: number }[];
    remove: string[];
    open?: boolean;
  } = {
    add: (update.add ?? []).map((a) =>
      a.expiresAtMs === undefined || a.expiresAtMs === null
        ? { reach_pk_b64: base64Encode(a.publicKey) }
        : { reach_pk_b64: base64Encode(a.publicKey), expires_at_ms: a.expiresAtMs },
    ),
    remove: (update.remove ?? []).map((k) => base64Encode(k)),
  };
  if (update.open !== undefined) body.open = update.open;
  return body;
}

/** Default base URL — Vite dev proxy `/api/ds/*` →
 *  `127.0.0.1:8082`. Prod nginx routes the same path. */
export const DEFAULT_BASE_URL = "/api/ds";

/** Lowercase-hex encode a byte array (no `0x` prefix). The DS
 *  path expects exactly 64 lowercase hex chars for a 32-byte
 *  pseudonym. */
export function pseudonymHex(bytes: Uint8Array): string {
  let out = "";
  for (let i = 0; i < bytes.length; i++) {
    const v = bytes[i] ?? 0;
    out += v.toString(16).padStart(2, "0");
  }
  return out;
}

/** Result of `DsClient.publishMailbox`. */
export interface PublishResult {
  /** Server-assigned monotonic id. Use as the `after` cursor on
   *  subsequent `fetchMailbox` calls. */
  id: number;
  /** ISO-8601 timestamp from the server. */
  createdAt: string;
}

/** Decoded message from the mailbox cursor. */
export interface MailboxMessage {
  id: number;
  /** Raw payload bytes (decoded from the server's `payload_b64`). */
  payload: Uint8Array;
  createdAt: string;
}

/** Result of `DsClient.fetchMailbox`. */
export interface FetchResult {
  messages: MailboxMessage[];
  /** `true` if there are more rows past the last returned id;
   *  client should call again with `after = lastMessage.id`. */
  hasMore: boolean;
}

/** Programmer error — surfaced before any network call. */
export class DsClientArgError extends Error {
  override name = "DsClientArgError";
}

/** Server returned a non-2xx. */
export class DsHttpError extends Error {
  override name = "DsHttpError";
  constructor(
    public readonly status: number,
    public readonly endpoint: string,
    public readonly body: string,
    /** ADR-0053 contract point 5: a `401` whose response `Date` was more than
     *  a minute from the device clock - the proof was refused because this
     *  device's clock is wrong, and the user is told so. */
    public readonly clockSkewed: boolean = false,
  ) {
    super(`ds ${endpoint} → ${status}: ${body}`);
  }
}

/** Network-level failure. */
export class DsNetworkError extends Error {
  override name = "DsNetworkError";
  constructor(public readonly endpoint: string, cause: unknown) {
    super(`ds ${endpoint} network error: ${String(cause)}`);
  }
}

/**
 * `true` when `err` is, or wraps, a transport-level DS failure. The receive
 * loops use it to pick the short network backoff. Wrapping differs by caller —
 * `ChatError` keeps `cause`, `ContactsError` keeps `variant.cause` — so the
 * walk follows both, bounded so a cyclic chain cannot hang it.
 */
export function isDsNetworkFailure(err: unknown): boolean {
  let current: unknown = err;
  for (let depth = 0; depth < 8 && current !== undefined && current !== null; depth += 1) {
    if (current instanceof DsNetworkError) return true;
    if (typeof current !== "object") return false;
    const record = current as {
      cause?: unknown;
      variant?: { cause?: unknown };
    };
    current = record.cause ?? record.variant?.cause;
  }
  return false;
}

/** How one request is proven (ADR-0053). */
type SigningPlan =
  /** Never signed: a rendezvous id, the knock, a proofless mailbox write. */
  | { readonly kind: "none" }
  /** Signed by this signer (or unsigned when `null`), one attempt: the caller
   *  owns the group and knows exactly which key the request needs. */
  | { readonly kind: "explicit"; readonly signer: ProofSigner | null }
  /** A group journal: the registry's current signer, the contract-point-5
   *  retries on a refusal. */
  | { readonly kind: "group"; readonly groupId: Uint8Array }
  /** One of this device's own mailboxes: the registry's mailbox signer, one
   *  claim-and-retry on a refusal. */
  | { readonly kind: "mailbox"; readonly pseudonym: Uint8Array };

/** Per-request signing override accepted by the journal methods. `undefined`
 *  keeps the registry's choice; `null` sends the request unsigned. */
export interface DsSigningOptions {
  readonly signer?: ProofSigner | null;
}

/** Options of {@link DsClient.publishGroup}. */
export interface DsGroupPublishOptions extends DsSigningOptions {
  /**
   * ADR-0054 - ask the DS to wake the group's subscribed devices after the
   * row is appended. Absent or `null` publishes silently: a control envelope,
   * or a device that does not hold the group's `push_gid` yet.
   */
  readonly wake?: DsWake | null;
}

/** Response of `POST /ds/group/<hex>/knock`. */
export interface KnockResult {
  readonly ids: readonly number[];
  /** The group's latest registered access epoch, `null` when unclaimed. */
  readonly epoch: number | null;
}

/** Options of the DS client beyond its base URL. */
export interface DsClientOptions {
  /** The signer registry. Defaults to the app's. */
  readonly access?: DsAccessRegistry;
  /** `fetch`, injectable for the host tests. */
  readonly fetchImpl?: typeof fetch;
  /** The device clock in unix ms (the skew check). */
  readonly now?: () => number;
  /** The anonymous-token pool the knock spends from. Defaults to the app's. */
  readonly tokens?: AnonTokenPool;
}

export class DsClient {
  private readonly access: DsAccessRegistry;
  private readonly fetchImpl: typeof fetch;
  private readonly now: () => number;
  private readonly tokens: AnonTokenPool;

  constructor(
    public readonly baseUrl: string = DEFAULT_BASE_URL,
    options: DsClientOptions = {},
  ) {
    this.access = options.access ?? dsAccess;
    this.fetchImpl =
      options.fetchImpl ?? ((input, init) => globalThis.fetch(input, init));
    this.now = options.now ?? (() => Date.now());
    this.tokens = options.tokens ?? anonTokens;
  }

  // -----------------------------------------------------------------
  // The one request path
  // -----------------------------------------------------------------

  /**
   * Send one request, proving it as `plan` says. `path` is the SERVICE path
   * (`/ds/group/<hex>/messages`), which is also what is signed - never the
   * `/api/ds` prefix - and `query` the raw query string sent verbatim and
   * signed byte-for-byte. Returns the 2xx response or throws.
   */
  private async send(req: {
    readonly method: "GET" | "POST" | "PUT";
    readonly path: string;
    readonly query: string;
    readonly body: Uint8Array;
    readonly contentType?: string;
    readonly extraHeaders?: Readonly<Record<string, string>>;
    readonly signal?: AbortSignal | undefined;
    readonly plan: SigningPlan;
  }): Promise<Response> {
    const plan = req.plan;
    switch (plan.kind) {
      case "none":
        return this.finish(req, await this.attempt(req, null));
      case "explicit":
        return this.finish(req, await this.attempt(req, plan.signer));
      case "mailbox": {
        const signer = this.access.mailboxSigner(plan.pseudonym);
        const first = await this.attempt(req, signer);
        if (first.status !== 401 || signer === null) return this.finish(req, first);
        // The claim may not have landed yet (it is fired, not awaited, at
        // session start): claim now and try once more.
        await this.claimMailbox(signer).catch(() => undefined);
        const again = this.access.mailboxSigner(plan.pseudonym);
        return this.finish(req, await this.attempt(req, again));
      }
      case "group": {
        const { current, previous } = this.access.groupSigners(plan.groupId);
        if (current !== null && this.access.markClaimAttempt(plan.groupId)) {
          // Contract point 2 - claim on open, once per group per session.
          // Fired, not awaited: an unclaimed journal answers the request below
          // either way, and a claimed one ignores a claim it already holds.
          void this.claimGroupWith(plan.groupId, current, previous).catch((err: unknown) => {
            // eslint-disable-next-line no-console
            console.warn("ds: claim on open failed; retried on the next refusal", err);
          });
        }
        const first = await this.attempt(req, current);
        if (first.status !== 401) return this.finish(req, first);
        // Contract point 5 - once under the previous epoch's key (a read
        // before a lagging registration landed), then once after a claim.
        let latest = first;
        if (previous !== null) {
          latest = await this.attempt(req, previous);
          if (latest.status !== 401) return this.finish(req, latest);
        }
        const again = this.access.groupSigners(plan.groupId);
        if (again.current !== null) {
          await this.claimGroupWith(plan.groupId, again.current, again.previous).catch(
            () => undefined,
          );
          latest = await this.attempt(req, again.current);
        }
        return this.finish(req, latest);
      }
    }
  }

  /** One HTTP attempt. The header is computed here, synchronously, so the
   *  signer is used while it is certainly alive and every retry carries a
   *  fresh nonce. */
  private async attempt(
    req: {
      readonly method: string;
      readonly path: string;
      readonly query: string;
      readonly body: Uint8Array;
      readonly contentType?: string;
      readonly extraHeaders?: Readonly<Record<string, string>>;
      readonly signal?: AbortSignal | undefined;
      /** An anonymous-token route: no credentials, no referrer. */
      readonly anonymous?: boolean;
    },
    signer: ProofSigner | null,
  ): Promise<Response> {
    const headers: Record<string, string> = { ...req.extraHeaders };
    if (req.contentType !== undefined) headers["content-type"] = req.contentType;
    if (signer !== null) {
      headers[DS_PROOF_HEADER] = signer.sign(req.method, req.path, req.query, req.body);
    }
    const url = `${this.baseUrl}${req.path}${req.query.length === 0 ? "" : `?${req.query}`}`;
    const endpoint = `${req.path}${req.query.length === 0 ? "" : `?${req.query}`}`;
    const init: RequestInit =
      req.anonymous === true
        ? anonymousFetchInit({ method: req.method, headers })
        : { method: req.method, headers };
    if (req.method !== "GET") {
      // `.slice()` ensures the buffer is an `ArrayBuffer` (not
      // SharedArrayBuffer) - TS strict mode demands the narrower type for
      // `BodyInit`.
      init.body = req.body.slice();
    }
    if (req.signal !== undefined) init.signal = req.signal;
    try {
      return await this.fetchImpl(url, init);
    } catch (err) {
      throw new DsNetworkError(endpoint, err);
    }
  }

  /** Turn a final response into the result or the error. */
  private async finish(
    req: { readonly path: string; readonly query: string },
    response: Response,
  ): Promise<Response> {
    if (response.ok) return response;
    const endpoint = `${req.path}${req.query.length === 0 ? "" : `?${req.query}`}`;
    const text = await safeReadBody(response);
    const serverDateMs = parseHttpDate(response.headers.get("date"));
    throw new DsHttpError(
      response.status,
      endpoint,
      text,
      response.status === 401 && isClockSkewed(serverDateMs, this.now()),
    );
  }

  private groupPlan(groupId: Uint8Array, options: DsSigningOptions | undefined): SigningPlan {
    if (options?.signer !== undefined) return { kind: "explicit", signer: options.signer };
    return { kind: "group", groupId };
  }

  // -----------------------------------------------------------------
  // Claims (ADR-0053 contract point 2)
  // -----------------------------------------------------------------

  /**
   * `PUT /ds/group/{hex}/access` registering `next`'s key at its epoch,
   * proven under `signedBy` (unsigned when `null`: the first claim of an
   * unclaimed journal, or re-sending a key the journal already holds). Throws
   * on a refusal - the register-before-publish caller aborts on it.
   */
  async claimGroupWith(
    groupId: Uint8Array,
    next: ProofSigner,
    signedBy: ProofSigner | null,
  ): Promise<void> {
    if (groupId.length === 0) throw new DsClientArgError("groupId must be non-empty");
    const body = new TextEncoder().encode(next.groupClaimBodyJson());
    await this.send({
      method: "PUT",
      path: `/ds/group/${pseudonymHex(groupId)}/access`,
      query: "",
      body,
      contentType: "application/json",
      plan: { kind: "explicit", signer: signedBy },
    });
  }

  /** {@link EpochRegistrar} over this client. */
  async registerGroupEpoch(
    groupId: Uint8Array,
    next: ProofSigner,
    signedBy: ProofSigner | null,
  ): Promise<void> {
    await this.claimGroupWith(groupId, next, signedBy);
  }

  /** `PUT /ds/mailbox/{hex}/access` - claim the mailbox `signer`'s identity
   *  key names. Unsigned: the key is public and registering it grants the
   *  registrant nothing. Idempotent. */
  async claimMailbox(signer: ProofSigner): Promise<void> {
    const pseudonym = signer.mailboxPseudonym();
    const body = new TextEncoder().encode(signer.mailboxClaimBodyJson());
    await this.send({
      method: "PUT",
      path: `/ds/mailbox/${pseudonymHex(pseudonym)}/access`,
      query: "",
      body,
      contentType: "application/json",
      plan: { kind: "none" },
    });
  }

  // -----------------------------------------------------------------
  // The knock (ADR-0053 section 4, contract points 6 + 7)
  // -----------------------------------------------------------------

  /**
   * `POST /ds/group/{hex}/knock` - a non-member's request to join. Never
   * signed; spends one anonymous `ds-knock` token (ADR-0054 S3) and carries
   * no credentials, retried once with another token when the DS refuses the
   * first as spent / invalid / expired. An empty `payloads` is a probe:
   * nothing is stored and the response carries the epoch.
   */
  async knock(groupId: Uint8Array, payloads: readonly Uint8Array[]): Promise<KnockResult> {
    if (groupId.length === 0) throw new DsClientArgError("groupId must be non-empty");
    const body = new TextEncoder().encode(
      JSON.stringify({ payloads_b64: payloads.map((p) => base64Encode(p)) }),
    );
    const path = `/ds/group/${pseudonymHex(groupId)}/knock`;
    const req = { method: "POST" as const, path, query: "", body, contentType: "application/json" };
    const response = await this.tokens.spend("ds-knock", (headerName, headerValue) =>
      this.attempt({ ...req, extraHeaders: { [headerName]: headerValue }, anonymous: true }, null),
    );
    const ok = await this.finish(req, response);
    const json = (await ok.json()) as { ids: number[]; epoch: number | null };
    return { ids: json.ids ?? [], epoch: json.epoch ?? null };
  }

  /** `GET /ds/group/{hex}/knocks?after=&limit=&wait=` - the members read what
   *  was knocked; proven exactly like a read of the group's messages. */
  async fetchKnocks(
    groupId: Uint8Array,
    options: {
      after?: number;
      limit?: number;
      waitSecs?: number;
      signal?: AbortSignal;
    } & DsSigningOptions = {},
  ): Promise<FetchResult> {
    if (groupId.length === 0) throw new DsClientArgError("groupId must be non-empty");
    return this.fetchJournal(
      `/ds/group/${pseudonymHex(groupId)}/knocks`,
      options,
      this.groupPlan(groupId, options),
    );
  }

  // -----------------------------------------------------------------
  // Journals
  // -----------------------------------------------------------------

  /**
   * `POST /ds/mailbox/{pseudonym_hex}/messages` — publish a raw
   * payload to a mailbox. The body is sent as
   * `application/octet-stream`; the server caps the size at
   * `MAX_MESSAGE_BYTES`. ADR-0058: a claimed mailbox is closed unless its
   * owner chose Everyone, so the write carries the reach grant (or the
   * delegation) that authorises it; an unauthorised write to a closed
   * mailbox is answered as stored and dropped.
   */
  async publishMailbox(
    pseudonym: Uint8Array,
    payload: Uint8Array,
    auth?: MailboxWriteAuth | null,
  ): Promise<PublishResult> {
    if (pseudonym.length !== 32) {
      throw new DsClientArgError("pseudonym must be 32 bytes");
    }
    if (payload.length === 0) {
      throw new DsClientArgError("payload must be non-empty");
    }
    const signer = auth?.signer ?? null;
    const delegation = auth?.delegation ?? null;
    if (delegation !== null && signer === null) {
      // The DS refuses a delegation without the delegate's proof; say so here
      // rather than as a 401.
      throw new DsClientArgError("a delegation needs the delegate's proof signer");
    }
    return this.publish(
      `/ds/mailbox/${pseudonymHex(pseudonym)}/messages`,
      payload,
      { kind: "explicit", signer },
      delegation === null ? undefined : { [DS_DELEGATION_HEADER]: delegation },
    );
  }

  /**
   * `POST /ds/rendezvous/{hex}/messages` - a random rendezvous id (pairing,
   * recovery, the welcome courier, the state bundle; ADR-0058 section 2).
   * Never signed and never carries a delegation or wake header: the DS
   * refuses all three there.
   */
  async publishRendezvous(id: Uint8Array, payload: Uint8Array): Promise<PublishResult> {
    if (id.length !== 32) throw new DsClientArgError("rendezvous id must be 32 bytes");
    if (payload.length === 0) throw new DsClientArgError("payload must be non-empty");
    return this.publish(`/ds/rendezvous/${pseudonymHex(id)}/messages`, payload, {
      kind: "none",
    });
  }

  /** `GET /ds/rendezvous/{hex}/messages` - unsigned, like its write. */
  async fetchRendezvous(
    id: Uint8Array,
    options: { after?: number; limit?: number; waitSecs?: number; signal?: AbortSignal } = {},
  ): Promise<FetchResult> {
    if (id.length !== 32) throw new DsClientArgError("rendezvous id must be 32 bytes");
    return this.fetchJournal(`/ds/rendezvous/${pseudonymHex(id)}/messages`, options, {
      kind: "none",
    });
  }

  /**
   * `PUT /ds/mailbox/{hex}/reach` (ADR-0058 section 2): add and remove reach
   * public keys and set the Everyone flag, proven under the mailbox's
   * identity key. `pseudonym` must be one of this device's own mailboxes (the
   * registry holds its signer); the mailbox is claimed first when the DS says
   * it is not.
   */
  async putMailboxReach(pseudonym: Uint8Array, update: MailboxReachUpdate): Promise<void> {
    if (pseudonym.length !== 32) throw new DsClientArgError("pseudonym must be 32 bytes");
    const body = new TextEncoder().encode(JSON.stringify(reachUpdateBody(update)));
    await this.send({
      method: "PUT",
      path: `/ds/mailbox/${pseudonymHex(pseudonym)}/reach`,
      query: "",
      body,
      contentType: "application/json",
      plan: { kind: "mailbox", pseudonym },
    });
  }

  /**
   * `POST /ds/mailbox/{hex}/reach/check` with a reach proof: `true` when the
   * key is registered and unexpired (204), `false` on the uniform refusal
   * (401). Any other answer throws, so a network failure is never read as
   * "not found" (UX section 6.1).
   */
  async reachCheck(pseudonym: Uint8Array, signer: ProofSigner): Promise<boolean> {
    if (pseudonym.length !== 32) throw new DsClientArgError("pseudonym must be 32 bytes");
    const req = {
      method: "POST" as const,
      path: `/ds/mailbox/${pseudonymHex(pseudonym)}/reach/check`,
      query: "",
      body: new Uint8Array(0),
    };
    const response = await this.attempt(req, signer);
    if (response.status === 401) return false;
    await this.finish(req, response);
    return true;
  }

  /**
   * `POST /ds/group/{group_id_hex}/messages` — publish a raw
   * payload to a group journal. The body is sent as
   * `application/octet-stream`; the server caps the size at
   * `MAX_MESSAGE_BYTES`. Proven under the group's access key (ADR-0053).
   *
   * Used by `ChatClient.sendMessage` to ship MLS Application
   * messages produced by `Group.createApplicationMessage`.
   *
   * `options.wake` (ADR-0054) adds the `x-lakko-wake` header to every attempt
   * of this publish, the contract-point-5 retries included; the value is built
   * before any request, so a malformed wake fails here rather than as a 400.
   */
  async publishGroup(
    groupId: Uint8Array,
    payload: Uint8Array,
    options?: DsGroupPublishOptions,
  ): Promise<PublishResult> {
    if (groupId.length === 0) {
      throw new DsClientArgError("groupId must be non-empty");
    }
    if (payload.length === 0) {
      throw new DsClientArgError("payload must be non-empty");
    }
    const wake = options?.wake;
    let extraHeaders: Record<string, string> | undefined;
    if (wake !== undefined && wake !== null) {
      try {
        extraHeaders = { [DS_WAKE_HEADER]: dsWakeHeaderValue(wake) };
      } catch (err) {
        throw new DsClientArgError(err instanceof Error ? err.message : String(err));
      }
    }
    return this.publish(
      `/ds/group/${pseudonymHex(groupId)}/messages`,
      payload,
      this.groupPlan(groupId, options),
      extraHeaders,
    );
  }

  private async publish(
    path: string,
    payload: Uint8Array,
    plan: SigningPlan,
    extraHeaders?: Readonly<Record<string, string>>,
  ): Promise<PublishResult> {
    const response = await this.send({
      method: "POST",
      path,
      query: "",
      body: payload,
      contentType: "application/octet-stream",
      ...(extraHeaders === undefined ? {} : { extraHeaders }),
      plan,
    });
    const json = (await response.json()) as { id: number; created_at: string };
    return { id: json.id, createdAt: json.created_at };
  }

  /**
   * `GET /ds/mailbox/{pseudonym_hex}/messages?after=&limit=&wait=`
   * — pull all messages newer than `after` for the given
   * mailbox. Always proven under the identity key: only this device's own
   * mailboxes are read here (ADR-0058; a rendezvous id has its own route).
   *
   * `wait` is the long-poll budget in seconds (0 returns
   * immediately whatever's there; non-zero holds the connection
   * up to that many seconds and returns as soon as at least one
   * row arrives). Server clamps to its own `MAX_WAIT_SECS`.
   */
  async fetchMailbox(
    pseudonym: Uint8Array,
    options: {
      after?: number;
      limit?: number;
      waitSecs?: number;
      /** Aborts the (possibly long-polling) request. An aborted fetch
       *  rejects with `DsNetworkError`; callers that abort on purpose
       *  check their own signal first. */
      signal?: AbortSignal;
    } & DsSigningOptions = {},
  ): Promise<FetchResult> {
    if (pseudonym.length !== 32) {
      throw new DsClientArgError("pseudonym must be 32 bytes");
    }
    // ADR-0058: every read of a personal mailbox carries the identity proof
    // (an unproven read is 401 whether the mailbox is claimed or not);
    // rendezvous ids are read through `fetchRendezvous`.
    const plan: SigningPlan =
      options.signer !== undefined
        ? { kind: "explicit", signer: options.signer }
        : { kind: "mailbox", pseudonym };
    return this.fetchJournal(`/ds/mailbox/${pseudonymHex(pseudonym)}/messages`, options, plan);
  }

  /**
   * `GET /ds/group/{group_id_hex}/messages?after=&limit=&wait=`
   * — pull all messages newer than `after` for the given
   * group journal. Proven under the group's access key (ADR-0053).
   *
   * Mirror of `fetchMailbox`; the server uses the same FIFO
   * journal shape on either side of the `/group/` vs
   * `/mailbox/` route, so the response decoder is identical.
   */
  async fetchGroup(
    groupId: Uint8Array,
    options: {
      after?: number;
      limit?: number;
      waitSecs?: number;
      /** Aborts the (possibly long-polling) request. An aborted fetch
       *  rejects with `DsNetworkError`; callers that abort on purpose
       *  check their own signal first. */
      signal?: AbortSignal;
    } & DsSigningOptions = {},
  ): Promise<FetchResult> {
    if (groupId.length === 0) {
      throw new DsClientArgError("groupId must be non-empty");
    }
    return this.fetchJournal(
      `/ds/group/${pseudonymHex(groupId)}/messages`,
      options,
      this.groupPlan(groupId, options),
    );
  }

  private async fetchJournal(
    path: string,
    options: { after?: number; limit?: number; waitSecs?: number; signal?: AbortSignal },
    plan: SigningPlan,
  ): Promise<FetchResult> {
    const query = journalQuery(options);
    const response = await this.send({
      method: "GET",
      path,
      query,
      body: new Uint8Array(0),
      signal: options.signal,
      plan,
    });
    const json = (await response.json()) as {
      messages: { id: number; payload_b64: string; created_at: string }[];
      has_more: boolean;
    };
    return {
      messages: json.messages.map((m) => ({
        id: m.id,
        payload: base64Decode(m.payload_b64),
        createdAt: m.created_at,
      })),
      hasMore: json.has_more,
    };
  }
}

/**
 * The query string of a journal read, EXACTLY as it is sent and signed: the
 * proof covers the raw query byte-for-byte, so the string that goes in the URL
 * and the one handed to `sign` must be the same value, built once.
 */
export function journalQuery(options: {
  after?: number;
  limit?: number;
  waitSecs?: number;
}): string {
  const params = new URLSearchParams();
  if (options.after !== undefined) params.set("after", String(options.after));
  if (options.limit !== undefined) params.set("limit", String(options.limit));
  if (options.waitSecs !== undefined) params.set("wait", String(options.waitSecs));
  return params.toString();
}

async function safeReadBody(response: Response): Promise<string> {
  try {
    const text = await response.text();
    return text.length > 512 ? `${text.slice(0, 512)}…` : text;
  } catch {
    return "<unreadable body>";
  }
}
