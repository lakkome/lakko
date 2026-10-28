// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0060 - the session's reach keys: deriving them, handing them out, and
 * keeping the account mailbox's registered set in step with the setting.
 *
 * One {@link ReachSession} lives per signed-in session (started by
 * `ReachProvider` when the account identity is set). It holds a COPY of the
 * account identity's 32-byte Ed25519 seed - the "account seed" every reach
 * seed is derived from, identical on every device of the account - and the
 * encrypted rows of `reach-store.ts`.
 *
 * `sync()` is the one place keys reach the DS: it computes the wanted set
 * ({@link desiredReachKeys}), resolves each to its public key through the
 * wasm core, diffs against the registered set and sends one
 * `PUT /ds/mailbox/{hex}/reach`. Syncs are serialized and coalesced; a sync
 * that fails leaves the book untouched (the next one retries the same diff),
 * so the registered set on this device never claims more than the DS holds.
 *
 * The version counters are shared by the account's devices (ADR-0060
 * section 7): every local change of a counter moves the book's
 * `stateUpdatedAtMs` forward and marks the ReachState envelope dirty in
 * `service-envelope-outbox.ts`, which sends it over the DeviceGroup body
 * `ServiceEnvelope`; {@link ReachSession.receiveReachState} folds a state
 * from another device by the core's `mergeReachState` rule. Without that, two devices writing one
 * mailbox's key set would each keep re-registering the key the other rotated
 * away.
 */
import { createSignal, type Accessor } from "solid-js";

import type {
  ReachGrant as WasmReachGrant,
  ReachState as WasmReachState,
  VisibilitySetting as WasmVisibilitySetting,
} from "../../wasm/lakko_ffi_wasm";
import { base64Decode, base64Encode } from "../auth/account-id";
import { DsClient, DsHttpError, DsNetworkError } from "../ds/client";
import { journalHex } from "../ds/access";
import { personKeysHex } from "../contacts/address-resolver";
import { loadWasm, type Wasm } from "../wasm";
import {
  REACH_CLASS_BYTE,
  REPLY_TTL_MS,
  addLink,
  addReply,
  applyAcceptedDiff,
  applyVersionChecks,
  adoptReachState,
  bindReachBook,
  departuresOf,
  rotateClasses,
  bookHoldsNoVersionState,
  bumpVersion,
  decodeReachBook,
  desiredReachKeys,
  dropLink,
  dropReply,
  dropRepliesTo,
  encodeReachBook,
  fofRotationDue,
  isAccountWideClass,
  isEmptyDiff,
  pruneExpired,
  reachClassName,
  reachStateEntries,
  reachDiff,
  rotateFof,
  versionChecksDue,
  versionOf,
  type ReachBook,
  type ReachStateEntryModel,
  type ReachClassName,
  type ReplyRecord,
  type ResolvedReachKey,
} from "./reach-model";
import {
  decodeReceivedGrants,
  dropContactGrant,
  encodeReceivedGrants,
  type ReceivedGrants,
} from "./received-grants";
import { loadReachRow, saveReachRow } from "./reach-store";
import { markServiceEnvelopeDirty, setServiceEnvelopeComposer } from "./service-envelope-outbox";
import { nextStamp } from "./visibility-store";

/** The scopes the account is in, as the sync reads them. */
export interface ReachScopes {
  readonly groupIdsHex: readonly string[];
  readonly channels: readonly { readonly idHex: string; readonly epoch: number }[];
}

/** Where the last attempt to apply the keys stands (UX section 8). */
export type ReachApplyStatus = "idle" | "applying" | "applied" | "offline" | "failed";

const [applyStatus, setApplyStatus] = createSignal<ReachApplyStatus>("idle");
/** Reactive: the state of the last sync, for the settings screen. */
export const reachApplyStatus: Accessor<ReachApplyStatus> = applyStatus;

const [versionsTick, setVersionsTick] = createSignal(0);
/** Reactive: moves whenever a reach key version counter changed, here or on
 *  another device of the account. What a profile or an introduction record
 *  carries depends on the counters, so their publishers re-check on it. */
export const reachVersionsTick: Accessor<number> = versionsTick;

function hexToBytes(hex: string): Uint8Array {
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i++) out[i] = Number.parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  return out;
}

/** The wasm class value for a class name. */
function wasmClass(wasm: Wasm, cls: ReachClassName): number {
  switch (cls) {
    case "contact":
      return wasm.ReachClass.Contact;
    case "link":
      return wasm.ReachClass.Link;
    case "reply":
      return wasm.ReachClass.Reply;
    case "group":
      return wasm.ReachClass.Group;
    case "member":
      return wasm.ReachClass.Member;
    case "channel":
      return wasm.ReachClass.Channel;
    case "fof":
      return wasm.ReachClass.Fof;
  }
}

export class ReachSession {
  private book: ReachBook;
  private grants: ReceivedGrants;
  private chain: Promise<void> = Promise.resolve();
  private pending = false;
  private scopes: () => ReachScopes = () => ({ groupIdsHex: [], channels: [] });
  private setting: () => WasmVisibilitySetting | null = () => null;
  private stopped = false;
  private predecessors: { readonly accountKey: Uint8Array; readonly seed: Uint8Array }[] = [];

  private constructor(
    readonly accountIdB64: string,
    /** The account identity's Ed25519 seed (32 bytes). Secret. */
    private readonly accountSeed: Uint8Array,
    /** The account identity's public key. */
    readonly accountKey: Uint8Array,
    /** The address of the account's own mailbox (ADR-0078: of the mailbox
     *  key derived from the account seed). */
    readonly accountMailbox: Uint8Array,
    private readonly wasm: Wasm,
    private readonly ds: DsClient,
    private readonly now: () => number,
    book: ReachBook,
    grants: ReceivedGrants,
  ) {
    this.book = book;
    this.grants = grants;
  }

  static async open(
    accountIdB64: string,
    accountSeed: Uint8Array,
    accountKey: Uint8Array,
    options: {
      /** The address of the account's own mailbox:
       *  `MailboxKey.fromAccountIdentity(accountIdentity).address()`
       *  (ADR-0078). The session never derives an address from a key. */
      readonly accountMailbox: Uint8Array;
      readonly ds?: DsClient;
      readonly now?: () => number;
    },
  ): Promise<ReachSession> {
    const wasm = await loadWasm();
    if (options.accountMailbox.length !== 32) throw new Error("account mailbox address must be 32 bytes");
    const mailbox = options.accountMailbox.slice();
    const [bookJson, grantsJson] = await Promise.all([
      loadReachRow(accountIdB64, "book"),
      loadReachRow(accountIdB64, "grants"),
    ]);
    // ADR-0074 section 3.5, "Reach" - the registrations the book records
    // were made on the mailbox of the key it was bound to. Under another key
    // of this account (a rotation) they describe the mailbox that was left:
    // forgotten here, so the first sync registers everything on the new one.
    // The versions, the links, the open requests and the grants held for
    // other people are kept. Nothing is sent to the old mailbox.
    const loaded = decodeReachBook(bookJson);
    const book = bindReachBook(loaded, journalHex(accountKey));
    if (book !== loaded) await saveReachRow(accountIdB64, "book", encodeReachBook(book));
    return new ReachSession(
      accountIdB64,
      accountSeed.slice(),
      accountKey.slice(),
      mailbox,
      wasm,
      options.ds ?? new DsClient(),
      options.now ?? (() => Date.now()),
      book,
      decodeReceivedGrants(grantsJson),
    );
  }

  /** The account key as the hex every grant store keys on. */
  get accountKeyHex(): string {
    return journalHex(this.accountKey);
  }

  stop(): void {
    this.stopped = true;
    this.accountSeed.fill(0);
    for (const p of this.predecessors) p.seed.fill(0);
    this.predecessors = [];
  }

  /** Where the sync reads the account's groups and channels from. */
  setScopeProvider(provider: () => ReachScopes): void {
    this.scopes = provider;
  }

  /** Where the sync reads the setting from. */
  setSettingProvider(provider: () => WasmVisibilitySetting | null): void {
    this.setting = provider;
  }

  // ---------------------------------------------------------------------
  // Grants this account hands out
  // ---------------------------------------------------------------------

  /** The current version of `(cls, scope)`; every class but `link` and
   *  `reply` is account-wide (ADR-0078 section 9) and ignores `scope`. */
  currentVersion(cls: ReachClassName, scope: Uint8Array = new Uint8Array(0)): number {
    if (cls === "reply") return 0;
    return versionOf(this.book, cls, isAccountWideClass(cls) ? "" : journalHex(scope));
  }

  /**
   * This account's own 97-byte mailbox binding (ADR-0078): the mailbox key
   * signed by the account identity key, as every grant this account mints
   * carries it. A public record of a person in Everyone carries it so a
   * finder knows where to write.
   */
  mailboxBinding(): Uint8Array {
    const grant = this.grant("contact");
    try {
      return grant.binding;
    } finally {
      grant.free();
    }
  }

  /** The current grant of `(cls, scope)`. The caller frees it. Every grant
   *  carries this account's mailbox binding (ADR-0078). */
  grant(cls: ReachClassName, scope: Uint8Array = new Uint8Array(0)): WasmReachGrant {
    const scopeHex = isAccountWideClass(cls) ? "" : journalHex(scope);
    const version = cls === "reply" ? 0 : versionOf(this.book, cls, scopeHex);
    return this.wasm.deriveReachGrant(
      this.accountSeed,
      wasmClass(this.wasm, cls),
      isAccountWideClass(cls) ? new Uint8Array(0) : scope,
      version,
    );
  }

  /**
   * A reach signer over the account seed itself: a committer that is not the
   * added person's contact signs the Welcome write with its ACCOUNT key, the
   * delegate key a committer delegation names (ADR-0060 section 6). The
   * caller frees it.
   */
  async accountReachSigner(): Promise<import("../ds/access").ProofSigner> {
    return this.wasm.DsProofSigner.forReach(this.accountSeed);
  }

  /**
   * ADR-0074 section 3.5a - every own predecessor key this device holds
   * (the conversation key first, then the keys held between two rotations),
   * each with a copy of its Ed25519 seed. A proposer names the committer's
   * account key as IT knows it: the key the role table carries, or any key
   * it last observed. So the committer must be able to sign as each of them.
   * An empty list forgets them. Nothing is ever registered or derived for a
   * mailbox from these seeds.
   */
  setPredecessors(
    predecessors: readonly { readonly accountKey: Uint8Array; readonly seed: Uint8Array }[],
  ): void {
    for (const p of this.predecessors) p.seed.fill(0);
    this.predecessors = predecessors.map((p) => ({
      accountKey: p.accountKey.slice(),
      seed: p.seed.slice(),
    }));
  }

  /** The own account keys a committer delegation may name, the current one
   *  first, then the predecessors newest first, each with the reach signer
   *  of exactly that key. The caller frees a signer it takes. */
  delegateKeys(): { readonly accountKey: Uint8Array; signer(): Promise<import("../ds/access").ProofSigner> }[] {
    const out = [{ accountKey: this.accountKey, signer: () => this.accountReachSigner() }];
    for (let i = this.predecessors.length - 1; i >= 0; i--) {
      const predecessor = this.predecessors[i];
      if (predecessor === undefined) continue;
      out.push({
        accountKey: predecessor.accountKey,
        signer: () => Promise.resolve(this.wasm.DsProofSigner.forReach(predecessor.seed)),
      });
    }
    return out;
  }

  /**
   * Seal this account's member card (ADR-0060 section 3): the profile bound
   * to the account by the account identity key's signature, which a reader
   * verifies before filing the card under the account. Here so the account
   * seed never leaves the session.
   */
  sealMemberCard(
    epochSeed: Uint8Array,
    epoch: number,
    channelId: Uint8Array,
    subscriberPk: Uint8Array,
    profile: Uint8Array,
  ): Uint8Array {
    return this.wasm.sealMemberCard(epochSeed, epoch, channelId, subscriberPk, this.accountSeed, profile);
  }

  /** The grants a profile carries in a conversation (ADR-0060 section 3 and
   *  the section 6 contact grant): `member` always, `group` when the setting
   *  lets this group's members find the owner, `contact` in a one-to-one. */
  profileGrants(groupId: Uint8Array, oneToOne: boolean): WasmReachGrant[] {
    const out = [this.grant("member", groupId)];
    if (oneToOne) {
      out.push(this.grant("contact"));
    } else {
      const setting = this.setting();
      if (setting !== null && this.allowsScope(setting, "group", groupId)) {
        out.push(this.grant("group", groupId));
      }
    }
    return out;
  }

  /**
   * What the grants a profile into `groupId` would carry, as a comparable
   * string of classes and versions (no key material). A conversation whose
   * last published signature differs needs the profile republished: a new
   * setting, a rotation after a shrink or a contact removal, or a
   * conversation that never received grants (ADR-0060 is a flag day for
   * profiles already sent).
   */
  profileGrantSignature(groupId: Uint8Array, oneToOne: boolean): string {
    // ADR-0074 section 3.5a rule 3 - the account key the grants are derived
    // under is part of what was published: after a rotation every version is
    // unchanged and every grant is another key, so every conversation needs
    // the profile again. A prefix of the PUBLIC key; no key material.
    const parts = [`key:${this.accountKeyHex.slice(0, 16)}`, `member:${this.currentVersion("member", groupId)}`];
    if (oneToOne) {
      parts.push(`contact:${this.currentVersion("contact")}`);
    } else {
      const setting = this.setting();
      if (setting !== null && this.allowsScope(setting, "group", groupId)) {
        parts.push(`group:${this.currentVersion("group", groupId)}`);
      }
    }
    return parts.join("|");
  }

  private allowsScope(setting: WasmVisibilitySetting, kind: "group" | "channel", id: Uint8Array): boolean {
    const scope = new this.wasm.ScopeId(
      kind === "group" ? this.wasm.ScopeKind.Group : this.wasm.ScopeKind.Channel,
      id,
    );
    try {
      return this.wasm.allowsScope(setting, scope);
    } finally {
      scope.free();
    }
  }

  /**
   * A fresh `reply` key for one outgoing request, registered (with its 7-day
   * expiry) before the call returns, so the answer can land. Throws when the
   * registration failed: a request whose answer cannot arrive must not be
   * sent. The caller frees the grant.
   */
  async newReply(
    purpose: ReplyRecord["purpose"],
    peerHex?: string,
  ): Promise<{ scopeHex: string; grant: WasmReachGrant }> {
    const scope = crypto.getRandomValues(new Uint8Array(16));
    const scopeHex = journalHex(scope);
    this.book = addReply(this.book, {
      scopeHex,
      expiresAtMs: this.now() + REPLY_TTL_MS,
      purpose,
      ...(peerHex === undefined ? {} : { peerHex }),
    });
    await this.persistBook();
    await this.syncNow(true);
    return {
      scopeHex,
      grant: this.wasm.deriveReachGrant(this.accountSeed, this.wasm.ReachClass.Reply, scope, 0),
    };
  }

  /** The request is answered (or given up): remove its key. */
  async closeReply(scopeHex: string): Promise<void> {
    if (!this.book.replies.some((r) => r.scopeHex === scopeHex)) return;
    this.book = dropReply(this.book, scopeHex);
    await this.persistBook();
    this.requestSync();
  }

  /** Close every open request key that went to `peerHex` (an account key,
   *  a group or call id, as hex): they answered, and each was single-use. */
  async closeRepliesTo(peerHex: string): Promise<void> {
    const next = dropRepliesTo(this.book, peerHex);
    if (next === this.book) return;
    this.book = next;
    await this.persistBook();
    this.requestSync();
  }

  /**
   * A new `/i/` or `/c/` link: registers its `link` key before returning (a
   * link shared before its key is registered is a link that does not work).
   * The caller frees the grant.
   */
  async newLink(linkId: Uint8Array, expiresAtMs: number | null): Promise<WasmReachGrant> {
    const linkIdHex = journalHex(linkId);
    this.book = addLink(this.book, { linkIdHex, expiresAtMs });
    await this.persistBook();
    await this.syncNow(true);
    return this.grant("link", linkId);
  }

  /** A link was revoked: its key goes, exactly its key. */
  async revokeLink(linkId: Uint8Array): Promise<void> {
    this.book = dropLink(this.book, journalHex(linkId));
    await this.persistBook();
    this.requestSync();
  }

  /**
   * A group shrank (ADR-0043 membership diff): new `group` and `member`
   * versions. Since ADR-0078 section 9 they are the account's one key per
   * class, so the profile carrying them is republished in EVERY group (the
   * grant signature of each changes). Call strictly after the removal commit
   * merged (the C-61 shape). The old keys go at the next sync. `_groupId` is
   * kept for the callers.
   */
  async rotateOnShrink(_groupId?: Uint8Array): Promise<void> {
    this.book = this.restamp(rotateClasses(this.book, ["group", "member"]));
    await this.persistBook();
    this.versionsChanged(true);
    await this.syncNow(false);
  }

  /**
   * The "who can find me" setting was changed on this device (ADR-0078
   * section 9, decision 4): new `group` and `channel` versions, so a group
   * or channel the setting no longer allows stops holding a working key, and
   * the profile carrying the new grants is republished in every group. A
   * change received from another device arrives with that device's bumped
   * counters and is not rotated again here.
   */
  async rotateOnSettingChange(): Promise<void> {
    this.book = this.restamp(rotateClasses(this.book, ["group", "channel"]));
    await this.persistBook();
    this.versionsChanged(true);
    await this.syncNow(false);
  }

  /** A contact was removed: new `contact` and `fof` versions. */
  async rotateOnContactRemoved(): Promise<void> {
    this.book = this.restamp(bumpVersion(bumpVersion(this.book, "contact", ""), "fof", ""));
    await this.persistBook();
    this.versionsChanged(true);
    await this.syncNow(false);
  }

  // ---------------------------------------------------------------------
  // Grants this account received
  // ---------------------------------------------------------------------

  receivedGrants(): ReceivedGrants {
    return this.grants;
  }

  async updateReceivedGrants(fn: (g: ReceivedGrants) => ReceivedGrants): Promise<void> {
    const next = fn(this.grants);
    if (next === this.grants) return;
    this.grants = next;
    await saveReachRow(this.accountIdB64, "grants", encodeReceivedGrants(next));
    notifyReceivedGrantsChanged();
  }

  /** Decode a kept grant wire. The caller frees it. */
  decodeGrant(wireB64: string): WasmReachGrant {
    return this.wasm.decodeReachGrant(base64Decode(wireB64));
  }

  /** Encode a grant for keeping. */
  encodeGrant(grant: WasmReachGrant): string {
    return base64Encode(this.wasm.encodeReachGrant(grant));
  }

  // ---------------------------------------------------------------------
  // The sync
  // ---------------------------------------------------------------------

  /** Queue a sync; coalesces with one already queued. Never throws. */
  requestSync(): void {
    if (this.pending) return;
    this.pending = true;
    this.chain = this.chain.then(async () => {
      this.pending = false;
      try {
        await this.runSync();
      } catch {
        /* reported through the status */
      }
    });
  }

  /** Run a sync now (after any queued one). With `mustApply` a failure
   *  throws, for the callers that must not proceed unregistered. */
  async syncNow(mustApply: boolean): Promise<void> {
    let error: unknown = null;
    const run = this.chain.then(async () => {
      try {
        await this.runSync();
      } catch (err) {
        error = err;
      }
    });
    this.chain = run;
    await run;
    if (mustApply && error !== null) throw error;
  }

  private publicKeyHex(cls: ReachClassName, scopeHex: string, version: number): string {
    const scope = isAccountWideClass(cls) ? new Uint8Array(0) : hexToBytes(scopeHex);
    const seed = this.wasm.reachSeed(this.accountSeed, wasmClass(this.wasm, cls), scope, version);
    try {
      return journalHex(this.wasm.reachVerifyingKey(seed));
    } finally {
      seed.fill(0);
    }
  }

  /**
   * ADR-0060 section 1, "Versions after a restore": before a recovering
   * device registers any key without a known version, it reads the keys its
   * mailbox holds (owner proof) and continues each `(class, scope)` from the
   * highest version registered there, so a key rotated away (a removed
   * contact's, an evicted member's) is never registered again. A failed read
   * throws: the sync registers NOTHING this time and is retried at the next
   * sync (a setting change, a scope change, coming back online, the next
   * reply or link). Meanwhile the keys already on the DS keep working for
   * whoever holds the current ones, and this device adds none.
   */
  private async recoverVersions(
    book: ReachBook,
    scopes: ReachScopes,
  ): Promise<{ book: ReachBook; raised: boolean }> {
    let next = book;
    if (next.recovery === null && bookHoldsNoVersionState(next)) {
      next = { ...next, recovery: { checked: {} } };
    }
    const checks = versionChecksDue(next, scopes);
    if (checks.length === 0) return { book: next, raised: false };
    const state = await this.ds.getMailboxReach(this.accountMailbox);
    const dsKeys = new Map<string, number | null>();
    for (const k of state.keys) dsKeys.set(journalHex(k.publicKey), k.expiresAtMs);
    // One check at a time, yielding between them: each derives up to 1025
    // keys, and a restored account with many groups must not freeze the page.
    let raised = false;
    for (const check of checks) {
      const one = applyVersionChecks(next, [check], dsKeys, (cls, scopeHex, version) =>
        this.publicKeyHex(cls, scopeHex, version),
      );
      next = one.book;
      raised = raised || one.raised;
      await new Promise<void>((resolve) => setTimeout(resolve, 0));
      if (this.stopped) throw new Error("reach session stopped");
    }
    const applied = { book: next, raised };
    if (applied.raised) {
      // eslint-disable-next-line no-console
      console.info("[reach] resumed reach key versions from the keys the mailbox holds");
      return { book: this.restamp(applied.book), raised: true };
    }
    return { book: applied.book, raised: false };
  }

  private async runSync(): Promise<void> {
    if (this.stopped) return;
    const now = this.now();
    const scopes = this.scopes();
    let book = pruneExpired(this.book, now);
    // Before anything is rotated or registered (a due `fof` rotation would
    // otherwise establish a version without looking at the DS).
    let recovered = false;
    try {
      const r = await this.recoverVersions(book, scopes);
      book = r.book;
      recovered = r.raised;
    } catch (err) {
      setApplyStatus(err instanceof DsNetworkError ? "offline" : "failed");
      // eslint-disable-next-line no-console
      console.warn(
        "[reach] reading the registered reach keys failed; nothing registered",
        err instanceof DsHttpError ? err.status : "network",
      );
      throw err;
    }
    let rotated = false;
    if (fofRotationDue(book, now)) {
      book = this.restamp(rotateFof(book, now));
      rotated = true;
    }
    // ADR-0078 section 9: leaving a group (or being removed from it) or a
    // channel rotates that class's one key, since its former members hold it.
    const departures = departuresOf(book, scopes);
    const leftClasses: ReachClassName[] = [
      ...(departures.leftGroup ? (["group", "member"] as const) : []),
      ...(departures.leftChannel ? (["channel"] as const) : []),
    ];
    if (leftClasses.length > 0) {
      book = this.restamp(rotateClasses(book, leftClasses));
      rotated = true;
    }
    const memberOf = departures.memberOf;
    const sameMembership =
      book.memberOf !== undefined &&
      book.memberOf !== null &&
      book.memberOf.groups.join(",") === memberOf.groups.join(",") &&
      book.memberOf.channels.join(",") === memberOf.channels.join(",");
    if (!sameMembership) book = { ...book, memberOf };
    const setting = this.setting();
    const everyone =
      setting !== null && setting.mode === this.wasm.VisibilityMode.Everyone;
    const wanted = desiredReachKeys(book, {
      groupIdsHex: scopes.groupIdsHex,
      channels: scopes.channels,
      allowsGroup: (hex) => setting !== null && this.allowsScope(setting, "group", hexToBytes(hex)),
      allowsChannel: (hex) =>
        setting !== null && this.allowsScope(setting, "channel", hexToBytes(hex)),
      allowsContactsOfContacts: setting !== null && this.wasm.allowsContactsOfContacts(setting),
      everyone,
      nowMs: now,
    });
    const resolved: ResolvedReachKey[] = wanted.map((w) => ({
      publicKeyHex: this.publicKeyHex(w.cls, w.scopeHex, w.version),
      expiresAtMs: w.expiresAtMs,
    }));
    // ADR-0078: the ADR-0073 R.4 continuity key is no longer registered on
    // this account's mailbox after a reset. The mailbox address is not
    // derivable from the account id or the identity key any more, so no peer
    // can address a write under that key; a registration would be dead.
    const diff = reachDiff(resolved, book.registered, everyone, book.open);
    if (book !== this.book) {
      this.book = book;
      await this.persistBook();
    }
    if (rotated || recovered) this.versionsChanged(true);
    if (isEmptyDiff(diff)) {
      setApplyStatus("applied");
      return;
    }
    setApplyStatus("applying");
    try {
      await this.ds.putMailboxReach(this.accountMailbox, {
        add: diff.add.map((a) => ({ publicKey: hexToBytes(a.publicKeyHex), expiresAtMs: a.expiresAtMs })),
        remove: diff.remove.map(hexToBytes),
        ...(diff.open === undefined ? {} : { open: diff.open }),
      });
    } catch (err) {
      setApplyStatus(err instanceof DsNetworkError ? "offline" : "failed");
      // eslint-disable-next-line no-console
      console.warn(
        "[reach] registering reach keys failed",
        err instanceof DsHttpError ? err.status : "network",
      );
      throw err;
    }
    this.book = applyAcceptedDiff(this.book, diff);
    await this.persistBook();
    setApplyStatus("applied");
  }

  // ---------------------------------------------------------------------
  // The counters shared with the account's other devices (section 7)
  // ---------------------------------------------------------------------

  /** The book with its counters stamped as changed now, strictly after the
   *  previous stamp. */
  private restamp(book: ReachBook): ReachBook {
    const stamp = nextStamp(BigInt(book.stateUpdatedAtMs), this.now());
    return { ...book, stateUpdatedAtMs: Number(stamp) };
  }

  /** A counter changed: let the publishers re-check, and (for a change made
   *  or merged here) send the state on to the other devices. */
  private versionsChanged(send: boolean): void {
    setVersionsTick((t) => t + 1);
    if (send) markServiceEnvelopeDirty("reachState");
  }

  /** The stamp of the counters as this device holds them. */
  reachStateUpdatedAtMs(): bigint {
    return BigInt(this.book.stateUpdatedAtMs);
  }

  /** The counters as a wasm `ReachState`, under the stamp of the last
   *  change (never re-stamped). The caller frees it. */
  private reachState(): WasmReachState {
    const entries = reachStateEntries(this.book).map(
      (e) =>
        new this.wasm.ReachStateEntry(
          wasmClass(this.wasm, e.cls),
          isAccountWideClass(e.cls) ? new Uint8Array(0) : hexToBytes(e.scopeHex),
          e.version,
        ),
    );
    const max = this.wasm.maxReachStateEntries();
    if (entries.length > max) {
      // eslint-disable-next-line no-console
      console.warn("[reach] more version counters than one ReachState carries; the rest stay on this device");
      for (const e of entries.slice(max)) e.free();
      entries.length = max;
    }
    return new this.wasm.ReachState(entries, BigInt(this.book.stateUpdatedAtMs));
  }

  /** The ReachState envelope: every counter, under the stamp of the last
   *  change. Composed as the state is, never re-stamped. `null` when it does
   *  not compose (logged). */
  reachStateEnvelope(): Uint8Array | null {
    const state = this.reachState();
    try {
      return this.wasm.composeReachStateEnvelope(state);
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("[reach] the ReachState envelope did not compose", err instanceof Error ? err.message : "");
      return null;
    } finally {
      state.free();
    }
  }

  /**
   * A ReachState from another device of this account, folded by the core's
   * one rule, `mergeReachState(stored, incoming, now)` (ADR-0060 section 7):
   * this device never decides the merge itself. When the result says
   * `store`, the returned state is kept exactly as returned (its counters
   * and its stamp) and everything a local change triggers runs: the keys are
   * re-synced and the publishers re-check. When it says `send`, the state
   * goes to the other devices as it is (the outbox composes the book, which
   * now equals it, under the returned stamp). Returns whether it was stored.
   */
  async receiveReachState(incoming: WasmReachState): Promise<boolean> {
    if (this.stopped) return false;
    const stored = this.reachState();
    let merge: ReturnType<Wasm["mergeReachState"]>;
    try {
      merge = this.wasm.mergeReachState(stored, incoming, BigInt(Math.floor(this.now())));
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("[reach] a ReachState from another device did not merge", err instanceof Error ? err.message : "");
      return false;
    } finally {
      stored.free();
    }
    try {
      if (!merge.store) return false;
      const state = merge.state;
      const entries: ReachStateEntryModel[] = [];
      let stamp: bigint;
      try {
        stamp = state.updatedAtMs;
        for (const e of state.entries) {
          try {
            const cls = reachClassName(e.class);
            if (cls !== null) {
              entries.push({
                cls,
                scopeHex: isAccountWideClass(cls) ? "" : journalHex(e.scopeId),
                version: e.version,
              });
            }
          } finally {
            e.free();
          }
        }
      } finally {
        state.free();
      }
      const fofBefore = versionOf(this.book, "fof", "");
      let book = adoptReachState(this.book, entries, Number(stamp));
      // The scheduled `fof` rotation happened on the other device: this one
      // starts its own schedule from here instead of rotating a second time.
      // Client-local bookkeeping, not part of the shared state.
      if (versionOf(book, "fof", "") > fofBefore) {
        book = { ...book, fofRotatedAtMs: Math.max(book.fofRotatedAtMs, this.now()) };
      }
      this.book = book;
      await this.persistBook();
      this.versionsChanged(merge.send);
      this.requestSync();
      return true;
    } finally {
      merge.free();
    }
  }

  private async persistBook(): Promise<void> {
    await saveReachRow(this.accountIdB64, "book", encodeReachBook(this.book));
  }

  /** For diagnostics and tests: the class byte table. */
  static readonly classBytes = REACH_CLASS_BYTE;
}

// ----------------------------------------------------------------------------
// Received-grant changes (ADR-0050 section 2.5a item 7)
// ----------------------------------------------------------------------------

const grantListeners = new Set<() => void>();

/**
 * Subscribe to changes of the grants this account received (a grant arrived,
 * was replaced or withdrawn). A Welcome kept owed because no grant reached its
 * addressee's mailbox is retried on this, not only on the next poll. Returns
 * the unsubscriber.
 */
export function onReceivedGrantsChanged(listener: () => void): () => void {
  grantListeners.add(listener);
  return () => {
    grantListeners.delete(listener);
  };
}

function notifyReceivedGrantsChanged(): void {
  for (const listener of [...grantListeners]) {
    try {
      listener();
    } catch {
      /* a listener's failure is its own */
    }
  }
}

// ----------------------------------------------------------------------------
// The session singleton
// ----------------------------------------------------------------------------

let current: ReachSession | null = null;
let generation = 0;
const [sessionSignal, setSessionSignal] = createSignal<ReachSession | null>(null);

/** Reactive: the running session, `null` when signed out. */
export const reachSessionSignal: Accessor<ReachSession | null> = sessionSignal;

/** The running session, or `null`. */
export function reachSession(): ReachSession | null {
  return current;
}

/** Start the session for an account identity (replacing any previous one). */
export async function startReachSession(
  accountIdB64: string,
  accountSeed: Uint8Array,
  accountKey: Uint8Array,
  accountMailbox: Uint8Array,
): Promise<ReachSession | null> {
  generation += 1;
  const mine = generation;
  current?.stop();
  current = null;
  setSessionSignal(null);
  const session = await ReachSession.open(accountIdB64, accountSeed, accountKey, { accountMailbox });
  if (mine !== generation) {
    session.stop();
    return null;
  }
  current = session;
  setSessionSignal(session);
  return session;
}

// The ReachState envelope the other devices get: the running session's.
setServiceEnvelopeComposer("reachState", () => Promise.resolve(current?.reachStateEnvelope() ?? null));

/** Sign-out: stop the session and forget its seed. */
export function stopReachSession(): void {
  generation += 1;
  current?.stop();
  current = null;
  setSessionSignal(null);
  setApplyStatus("idle");
}

/** A reach signer for a kept grant wire (`DsProofSigner.forReach`). The
 *  caller frees it. */
export async function reachSignerForGrantWire(wireB64: string): Promise<import("../ds/access").ProofSigner> {
  const wasm = await loadWasm();
  const grant = wasm.decodeReachGrant(base64Decode(wireB64));
  try {
    const seed = grant.seed;
    try {
      return wasm.DsProofSigner.forReach(seed);
    } finally {
      seed.fill(0);
    }
  } finally {
    grant.free();
  }
}

/** A reach signer for a wasm grant (not consumed). The caller frees it. */
export async function reachSignerForGrant(grant: WasmReachGrant): Promise<import("../ds/access").ProofSigner> {
  const wasm = await loadWasm();
  const seed = grant.seed;
  try {
    return wasm.DsProofSigner.forReach(seed);
  } finally {
    seed.fill(0);
  }
}

/**
 * A contact was removed (ADR-0060 section 1): rotate our `contact` and `fof`
 * keys so the removed person's copy stops working at once, and forget the
 * contact grant they handed us. Never throws.
 */
export async function onContactRemoved(accountKey: Uint8Array): Promise<void> {
  const session = reachSession();
  if (session === null) return;
  try {
    // ADR-0074 section 3.5a rule 2 - under every key the person has had.
    const keys = personKeysHex(journalHex(accountKey));
    await session.updateReceivedGrants((g) => keys.reduce((acc, hex) => dropContactGrant(acc, hex), g));
    await session.rotateOnContactRemoved();
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("[reach] contact-key rotation after a removal failed; retried at the next sync", err);
  }
}
