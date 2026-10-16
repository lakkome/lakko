// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `DsClient` — fetch-based client for `lakko-svc-ds`.
 *
 * The Delivery Service is a thin append-only relay: every
 * message goes into either a per-group journal
 * (`/ds/group/{group_hex}`) or a per-mailbox journal
 * (`/ds/mailbox/{address_hex}`). Both paths key the journal by a
 * 32-byte id encoded as **lowercase hex** in the URL. ADR-0078: a mailbox
 * address is `mailboxAddress(mailbox_pk)` for the account's mailbox key,
 * never a function of an identity key, and every row in a mailbox is a
 * sealed row (`contacts/mailbox-row.ts`).
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
 * mailbox key for a mailbox read. The signer comes from the registry in
 * `./access` (or is passed explicitly); the path signed is the service path,
 * never the `/api/ds` prefix, and the query is signed exactly as sent. A
 * refused group request is retried under the previous epoch's key (contract
 * point 5); then, ONLY when this account owns the journal (ADR-0057
 * obligation 2), the owner claims it with its root (the claim registers the
 * current epoch) and retries once; an already-claimed journal is left alone. A plain member
 * never claims and never registers: it waits for the committer's
 * registration. The knock stays unsigned and spends one anonymous `ds-knock`
 * token instead (ADR-0054 S3), carrying no credentials; a mailbox write with
 * no reach proof spends one `mailbox-write` token the same way (ADR-0078).
 *
 * ADR-0057 statements (claim, reset, register, registrars, root transfer) are
 * `PUT /ds/group/<hex>/<segment>` with the signer's JSON body and NO proof
 * header ({@link DsClient.putStatement}).
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
import { type OwnerClaimOutcome } from "./owner-claim";
import {
  NoJournalAuthorityError,
  claimAsOwner,
  isStatus,
  registerEpochWith,
  releaseAuthority,
  type StatementSender,
} from "./ownership";
import { journalRepair, type JournalRepair } from "./journal-repair";
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
 * How a write to someone else's identity mailbox is authorised (ADR-0060
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

/** The reach keys registered on a mailbox, as its owner reads them. */
export interface MailboxReachState {
  readonly keys: readonly { readonly publicKey: Uint8Array; readonly expiresAtMs: number | null }[];
  readonly open: boolean;
}

/** Parse the `GET .../reach` body; anything malformed throws (a read that
 *  cannot be understood must not pass for an empty one). Exported for the
 *  test. */
export function parseMailboxReachState(json: unknown, endpoint = "reach"): MailboxReachState {
  const bad = (): never => {
    throw new DsHttpError(200, endpoint, "malformed reach state");
  };
  if (json === null || typeof json !== "object") return bad();
  const { keys, open } = json as { keys?: unknown; open?: unknown };
  if (!Array.isArray(keys) || typeof open !== "boolean") return bad();
  const out: { publicKey: Uint8Array; expiresAtMs: number | null }[] = [];
  for (const k of keys as unknown[]) {
    if (k === null || typeof k !== "object") return bad();
    const { reach_pk_b64, expires_at_ms } = k as { reach_pk_b64?: unknown; expires_at_ms?: unknown };
    if (typeof reach_pk_b64 !== "string") return bad();
    let publicKey: Uint8Array;
    try {
      publicKey = base64Decode(reach_pk_b64 as string);
    } catch {
      return bad();
    }
    if (publicKey.length !== 32) return bad();
    if (expires_at_ms !== null && expires_at_ms !== undefined && typeof expires_at_ms !== "number") {
      return bad();
    }
    out.push({ publicKey, expiresAtMs: typeof expires_at_ms === "number" ? expires_at_ms : null });
  }
  return { keys: out, open };
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
  /** A signed read of a CLAIMED group journal only: the newest epoch
   *  registered on the DS for it (`latest_epoch`). Absent for an unclaimed
   *  journal and for a mailbox. */
  latestEpoch?: number;
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
  /** The journal repair scheduler a refusal notes. Defaults to the app's. */
  readonly repair?: Pick<JournalRepair, "note">;
}

export class DsClient implements StatementSender {
  private readonly access: DsAccessRegistry;
  private readonly repair: Pick<JournalRepair, "note">;
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
    this.repair = options.repair ?? journalRepair;
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
        const first = await this.attempt(req, current);
        if (first.status !== 401) return this.finish(req, first);
        // Contract point 5 - once under the previous epoch's key (a read
        // before a lagging registration landed).
        let latest = first;
        if (previous !== null) {
          latest = await this.attempt(req, previous);
          if (latest.status !== 401) return this.finish(req, latest);
        }
        // ADR-0057 obligation 2 - the lazy claim, by the OWNER only. A plain
        // member (or an admin) never claims and never registers here: it waits
        // for the committer's registration and retries on the next poll.
        const again = this.access.groupSigners(plan.groupId);
        if (again.current !== null && this.access.ownsJournal(plan.groupId)) {
          const claimed = await this.ownerClaim(plan.groupId, again.current).catch((err: unknown) => {
            // eslint-disable-next-line no-console
            console.warn("ds: the owner's lazy claim failed", err);
            return false;
          });
          if (claimed) latest = await this.attempt(req, again.current);
        }
        // Second security pass, finding 1 - refused at this device's own epoch
        // after everything else was tried: an owner or admin schedules the
        // repair check (a member's note is dropped there).
        if (latest.status === 401 && again.current !== null) {
          const skewed = isClockSkewed(parseHttpDate(latest.headers.get("date")), this.now());
          if (!skewed) this.repair.note(plan.groupId, again.current.epoch, "refused-at-head");
        }
        return this.finish(req, latest);
      }
    }
  }

  /** ADR-0057 obligation 2: claim the journal with this account's root at the
   *  epoch `current` proves. `true` only when the claim landed (the journal
   *  was unclaimed), which is the one case a retry can now succeed; an
   *  already-claimed journal (409) means this device waits for the
   *  committer's registration like everyone else. */
  private async ownerClaim(groupId: Uint8Array, current: ProofSigner): Promise<boolean> {
    const authority = this.access.journalAuthority(groupId);
    try {
      if (authority.kind !== "root") return false;
      const outcome = await claimAsOwner(this, groupId, authority.signer, current.epoch, current.verifyingKey());
      if (outcome === "claimed") this.access.noteOwnerClaimed(groupId);
      return outcome === "claimed";
    } finally {
      releaseAuthority(authority);
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
  // ADR-0057 statements
  // -----------------------------------------------------------------

  /**
   * `PUT <path>` with a statement body the wasm signer built (`path` is the
   * service path `/ds/group/<hex>/<segment>`). No proof header: the
   * statement carries its own signature. Throws `DsHttpError` on a refusal.
   */
  async putStatement(path: string, bodyJson: string): Promise<void> {
    if (!path.startsWith("/ds/group/")) {
      throw new DsClientArgError("a statement path must name a group journal");
    }
    await this.send({
      method: "PUT",
      path,
      query: "",
      body: new TextEncoder().encode(bodyJson),
      contentType: "application/json",
      plan: { kind: "none" },
    });
  }

  /**
   * ADR-0057 obligation 2 - claim `groupId` with this account's root when this
   * account owns it, once per session (`./owner-claim`). Called when a group
   * is opened and before every registration, because an UNCLAIMED journal
   * never answers the `401` the lazy claim waits for. Never throws; a `204`
   * reaches {@link DsAccessRegistry.noteOwnerClaimed}.
   */
  ensureOwnerClaim(groupId: Uint8Array): Promise<OwnerClaimOutcome> {
    if (groupId.length === 0) return Promise.resolve("not-owner");
    return this.access.ownerClaims.run(groupId, {
      ownsJournal: (g) => this.access.ownsJournal(g),
      currentEpochKey: (g) => {
        const { current } = this.access.groupSigners(g);
        return current === null ? null : { epoch: current.epoch, key: current.verifyingKey() };
      },
      claim: async (g, epoch, key) => {
        // The ROOT, whatever the "root held" marker says: this is how an owner
        // whose journal lost its claim gets it back. A journal held by another
        // root answers 401 and is left to the takeover.
        const authority = this.access.rootAuthority(g);
        try {
          if (authority.kind !== "root") throw new NoJournalAuthorityError();
          const outcome = await claimAsOwner(this, g, authority.signer, epoch, key);
          if (outcome === "already-claimed") throw new DsHttpError(409, `/ds/group/${pseudonymHex(g)}/claim`, "");
        } finally {
          releaseAuthority(authority);
        }
      },
      onClaimed: (g) => this.access.noteOwnerClaimed(g),
      now: this.now,
    });
  }

  /** {@link EpochRegistrar} over this client: register `epochKey` at `epoch`
   *  signed with this device's authority over the journal (root or
   *  registrar), revoking `revokeKeys`. Throws `NoJournalAuthorityError`
   *  when this device holds neither.
   *
   *  The owner claims the journal first (once per session): a registration in
   *  an unclaimed journal is refused `401` and nothing would ever claim it. */
  async registerGroupEpoch(
    groupId: Uint8Array,
    epoch: bigint,
    epochKey: Uint8Array,
    revokeKeys: readonly Uint8Array[] = [],
  ): Promise<void> {
    if (groupId.length === 0) throw new DsClientArgError("groupId must be non-empty");
    await this.ensureOwnerClaim(groupId);
    const authority = this.access.journalAuthority(groupId);
    try {
      await registerEpochWith(this, groupId, authority, epoch, epochKey, revokeKeys);
    } catch (err) {
      // Append 409: another committer registered `epoch` first. The caller
      // abandons (never a Replace in a commit path); an owner or admin
      // schedules the repair check at the epoch it is at.
      if (isStatus(err, 409) && epoch > 0n) {
        this.repair.note(groupId, epoch - 1n, "append-conflict");
      }
      throw err;
    } finally {
      releaseAuthority(authority);
    }
  }

  /** `PUT /ds/mailbox/{hex}/access` - claim the mailbox `signer`'s mailbox
   *  key names (ADR-0078: the body is `{"mailbox_pk_b64"}`). Unsigned: the
   *  key is public, the DS checks that it hashes to the address, and the
   *  first claim wins. Idempotent. */
  async claimMailbox(signer: ProofSigner): Promise<void> {
    const pseudonym = signer.mailboxAddress();
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
   * `POST /ds/mailbox/{address_hex}/messages` — publish one sealed
   * mailbox row (ADR-0078, `contacts/mailbox-row.ts`). The body is sent as
   * `application/octet-stream`; the DS refuses a body that is not a
   * well-formed row with `400`. ADR-0060: a claimed mailbox is closed unless
   * its owner chose Everyone, so the write carries the reach grant (or the
   * delegation) that authorises it; an unauthorised write to a closed
   * mailbox is answered as stored and dropped.
   *
   * ADR-0078: a write with NO reach proof spends one anonymous
   * `mailbox-write` token and carries no credentials, whether the mailbox
   * turns out to be open, unclaimed or closed (the DS spends it in every
   * case, so the answer says nothing about the mailbox). A write with a
   * proof spends none.
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
    const path = `/ds/mailbox/${pseudonymHex(pseudonym)}/messages`;
    if (signer === null) {
      const req = {
        method: "POST" as const,
        path,
        query: "",
        body: payload,
        contentType: "application/octet-stream",
      };
      const response = await this.tokens.spend("mailbox-write", (headerName, headerValue) =>
        this.attempt({ ...req, extraHeaders: { [headerName]: headerValue }, anonymous: true }, null),
      );
      const ok = await this.finish(req, response);
      const json = (await ok.json()) as { id: number; created_at: string };
      return { id: json.id, createdAt: json.created_at };
    }
    return this.publish(
      path,
      payload,
      { kind: "explicit", signer },
      delegation === null ? undefined : { [DS_DELEGATION_HEADER]: delegation },
    );
  }

  /**
   * `POST /ds/rendezvous/{hex}/messages` - a random rendezvous id (pairing,
   * recovery, the welcome courier, the state bundle; ADR-0060 section 2).
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
   * `PUT /ds/mailbox/{hex}/reach` (ADR-0060 section 2): add and remove reach
   * public keys and set the Everyone flag, proven under the mailbox's
   * mailbox key (ADR-0078). `pseudonym` must be one of this device's own mailboxes (the
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
   * `GET /ds/mailbox/{hex}/reach` (ADR-0060 section 1, "Versions after a
   * restore"): the reach public keys registered on this device's OWN mailbox
   * and its Everyone flag, proven under the mailbox key exactly
   * like the owner's read of its messages. Public keys only. Throws on any
   * refusal or failure, so a failed read is never mistaken for "nothing
   * registered".
   */
  async getMailboxReach(pseudonym: Uint8Array): Promise<MailboxReachState> {
    if (pseudonym.length !== 32) throw new DsClientArgError("pseudonym must be 32 bytes");
    const path = `/ds/mailbox/${pseudonymHex(pseudonym)}/reach`;
    const response = await this.send({
      method: "GET",
      path,
      query: "",
      body: new Uint8Array(0),
      plan: { kind: "mailbox", pseudonym },
    });
    return parseMailboxReachState(await response.json(), path);
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
   * mailbox. Always proven under the mailbox key: only this account's own
   * mailboxes are read here (ADR-0060; a rendezvous id has its own route).
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
    // ADR-0060: every read of a personal mailbox carries the mailbox-key proof
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
      latest_epoch?: number;
    };
    return {
      messages: json.messages.map((m) => ({
        id: m.id,
        payload: base64Decode(m.payload_b64),
        createdAt: m.created_at,
      })),
      hasMore: json.has_more,
      ...(typeof json.latest_epoch === "number" ? { latestEpoch: json.latest_epoch } : {}),
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
