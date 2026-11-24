// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Who sends the outbox (`docs/ux/connection-state.md` sections 7.2 and 7.3):
 * the app, not the conversation screen.
 *
 * One {@link WaitingSendPass} per conversation, for the whole session: the
 * chat view of that conversation and the app-level trigger (the servers
 * answering again, the session landing) run the SAME pass, so a row is never
 * sent twice from this device. Each send of a row goes to whoever can show
 * it: the open chat view, when one is attached for the conversation (it draws
 * the upload ring and the sent bubble), else the headless sender
 * ({@link sendUnsentHeadless}), which sends with no screen and files the sent
 * message in the message log.
 *
 * Kept free of Solid and of the wasm core so it runs in a test.
 */

import {
  ackAfterAutoNetworkFailure,
  WaitingSendPass,
  type SendOutcome,
} from "./waiting-sends";
import type { AttachmentMediaMeta, AttachmentRefBytes, ThumbRefBytes } from "./attachments";
import {
  trayPartOf,
  type PreparedFile,
  type UnsentEntry,
  type UnsentOutbox,
} from "./unsent-outbox";
import type { TrayPart } from "./tray-upload";

/** How a headless send of an entry ended. */
export type HeadlessOutcome =
  /** Published and filed: the entry goes. */
  | "sent"
  /** No answer: the entry waits (counted). */
  | "network"
  /** Refused, or the conversation cannot take it (ended, behind): `Not sent`. */
  | "refused"
  /** The entry has nothing more to do here (its conversation is gone, or it
   *  was handed to another queue): it goes, nothing is filed. */
  | "dropped"
  /** The conversation is not loaded on this device yet: nothing happened,
   *  nothing is counted, the pass stops for now. */
  | "not-ready";

/** The chat view's sender for its own rows. `undefined`: the view does not
 *  hold the row (the runner then sends it headless). */
export type ViewSender = (tempId: number) => Promise<SendOutcome | undefined>;

/** The app's outbox runner. */
export class UnsentRunner {
  private readonly passes = new Map<string, WaitingSendPass>();
  private readonly views = new Map<string, ViewSender>();

  constructor(
    private readonly outbox: UnsentOutbox,
    private readonly headless: (entry: UnsentEntry) => Promise<HeadlessOutcome>,
  ) {}

  /** The conversation's one pass. */
  pass(groupIdB64: string): WaitingSendPass {
    let p = this.passes.get(groupIdB64);
    if (p === undefined) {
      p = new WaitingSendPass({
        waitingInOrder: () => this.outbox.waitingIds(groupIdB64),
        send: (id) => this.sendOne(groupIdB64, id),
      });
      this.passes.set(groupIdB64, p);
    }
    return p;
  }

  /** A chat view of `groupIdB64` takes the sends of its rows while mounted. */
  attachView(groupIdB64: string, send: ViewSender): () => void {
    this.views.set(groupIdB64, send);
    return () => {
      if (this.views.get(groupIdB64) === send) this.views.delete(groupIdB64);
    };
  }

  /** Every conversation with waiting entries gets its pass (section 7.3). */
  runAll(): void {
    for (const g of this.outbox.groupsWithWaiting()) void this.pass(g).run();
  }

  /** One automatic send of a waiting entry, for the pass. Never throws. */
  private async sendOne(groupIdB64: string, tempId: number): Promise<SendOutcome> {
    const entry = this.outbox.get(groupIdB64, tempId);
    if (entry === undefined || entry.state !== "waiting") return "sent";
    const view = this.views.get(groupIdB64);
    if (view !== undefined) {
      try {
        const viaView = await view(tempId);
        if (viaView !== undefined) return viaView;
      } catch {
        return "refused";
      }
    }
    // Re-read: the view may have changed it.
    const current = this.outbox.get(groupIdB64, tempId);
    if (current === undefined || current.state !== "waiting") return "sent";
    await this.outbox.update(groupIdB64, tempId, { state: "pending" });
    let outcome: HeadlessOutcome;
    try {
      outcome = await this.headless(current);
    } catch {
      outcome = "refused";
    }
    switch (outcome) {
      case "sent":
      case "dropped":
        await this.outbox.remove(groupIdB64, tempId);
        return "sent";
      case "network": {
        const after = this.outbox.get(groupIdB64, tempId);
        const spent = (after?.autoSends ?? current.autoSends) + 1;
        await this.outbox.update(groupIdB64, tempId, {
          state: ackAfterAutoNetworkFailure(spent),
          autoSends: spent,
        });
        return "network";
      }
      case "not-ready":
        await this.outbox.update(groupIdB64, tempId, { state: "waiting" });
        return "network";
      case "refused":
        await this.outbox.update(groupIdB64, tempId, { state: "failed" });
        return "refused";
    }
  }
}

// ---------------------------------------------------------------------------
// A composer-tray message's items still to upload
// ---------------------------------------------------------------------------

/** What finishing a tray message's uploads needs; both senders give it. */
export interface PartsDeps {
  /** The entry's prepared file (the items' bytes, by slot). */
  preparedFile(entry: UnsentEntry): Promise<PreparedFile | null>;
  /** The tray's own upload of the item, when it was running at Send. */
  adoptTrayUpload(trayItemId: number): Promise<AttachmentRefBytes | null> | null;
  /** Upload one item. */
  uploadPart(part: TrayPart, index: number): Promise<AttachmentRefBytes>;
  /** One item landed: record it (the outbox, the row). */
  partUploaded(entry: UnsentEntry, index: number, ref: AttachmentRefBytes): Promise<void>;
}

/**
 * Upload what a tray message still needs, in its items' order, and return the
 * message's attachments (`docs/ux/media-messages.md` section 11, Send). An
 * upload the tray had running is adopted, never run twice. A failure throws
 * (a network one makes the message wait, section 7.2); the items that landed
 * are kept, so the next send resumes.
 */
export async function finishUnsentParts(entry: UnsentEntry, deps: PartsDeps): Promise<AttachmentRefBytes[]> {
  const parts = entry.parts ?? [];
  const out: AttachmentRefBytes[] = [];
  let prepared: PreparedFile | null | undefined;
  for (let i = 0; i < parts.length; i += 1) {
    const part = parts[i];
    if (part === undefined) continue;
    if (part.kind === "ref") {
      out.push(part.ref);
      continue;
    }
    let ref: AttachmentRefBytes | null = null;
    if (part.trayItemId !== undefined) {
      const adopted = deps.adoptTrayUpload(part.trayItemId);
      if (adopted !== null) ref = await adopted;
    }
    if (ref === null) {
      if (prepared === undefined) prepared = await deps.preparedFile(entry);
      const bytes = prepared?.parts?.[i];
      if (bytes === null || bytes === undefined) throw new Error("the prepared bytes of an unsent item are gone");
      ref = await deps.uploadPart(trayPartOf(part, bytes), i);
    }
    await deps.partUploaded(entry, i, ref);
    out.push(ref);
  }
  return out;
}

// ---------------------------------------------------------------------------
// The headless send
// ---------------------------------------------------------------------------

/** What the headless send needs; injected so a test can run it. */
export interface HeadlessDeps<G> extends PartsDeps {
  /** The conversation's live group, or `undefined` when not loaded. */
  group(groupIdB64: string): G | undefined;
  /** Whether this device's membership of the group ended. */
  ended(group: G): boolean;
  /** An anonymous poll may only be re-sent while its roster is unchanged. */
  contentSendable(entry: UnsentEntry, group: G): Promise<boolean>;
  /** The prepared file of an entry whose upload has not finished. */
  preparedFile(entry: UnsentEntry): Promise<{ bytes?: Uint8Array; posterBytes?: Uint8Array } | null>;
  /** Fetch a GIF's full rendition through the proxy. */
  loadGif(url: string): Promise<{ bytes: Uint8Array; mime: string }>;
  /** Encrypt and upload bytes as one attachment. */
  upload(
    bytes: Uint8Array,
    file: { readonly mime: string; readonly name: string },
    meta: AttachmentMediaMeta,
  ): Promise<AttachmentRefBytes>;
  /** Encrypt and upload a poster frame. */
  uploadPoster(bytes: Uint8Array, mime: string): Promise<{ blobId: Uint8Array; key: Uint8Array; nonce: Uint8Array }>;
  /** Every tray item landed: the entry carries the references from now. */
  partsDone(entry: UnsentEntry, attachments: readonly AttachmentRefBytes[]): Promise<void>;
  /** Record a finished upload in the outbox (the prepared file then goes). */
  uploaded(entry: UnsentEntry, ref: AttachmentRefBytes): Promise<void>;
  /** Compose the message's envelope. */
  compose(entry: UnsentEntry, attachments: readonly AttachmentRefBytes[], group: G): Promise<Uint8Array>;
  /** Publish into the group. */
  publish(group: G, envelope: Uint8Array): Promise<{ messageId: number; createdAt: string }>;
  /** File the sent message where an open view and a later one find it. */
  file(
    entry: UnsentEntry,
    attachments: readonly AttachmentRefBytes[],
    result: { messageId: number; createdAt: string },
    group: G,
  ): Promise<void>;
  /**
   * ADR-0073 R.2 / R.7 case 1 — a one-to-one's send decision, the one the
   * open view makes (`one-to-one-send-gate.ts` `oneToOneSendVerdict`): the
   * bounded peer identity check, then the route. A group answers `send`.
   */
  sendVerdict(entry: UnsentEntry, group: G): Promise<"send" | "wait" | "give-up">;
  /** Hand the message to the held queue (the verdict said `wait`). `true`
   *  when it was taken. */
  hold(entry: UnsentEntry, attachments: readonly AttachmentRefBytes[]): boolean;
  /** ADR-0073 R.7 case 1: the session is superseded; hand the message to
   *  the held queue. `true` when it was taken. */
  holdForSuccessor(entry: UnsentEntry, attachments: readonly AttachmentRefBytes[], err: unknown): boolean;
  /** Whether a failure got no answer (section 2). */
  isNetwork(err: unknown): boolean;
}

/**
 * Send one entry with no screen: upload its file if the upload has not
 * finished, publish, file the sent message. Never throws.
 */
export async function sendUnsentHeadless<G>(entry: UnsentEntry, deps: HeadlessDeps<G>): Promise<HeadlessOutcome> {
  const group = deps.group(entry.groupIdB64);
  if (group === undefined) return "not-ready";
  if (deps.ended(group)) return "refused";
  let attachments: readonly AttachmentRefBytes[] = entry.attachments;
  try {
    if (!(await deps.contentSendable(entry, group))) return "refused";
    const media = entry.media;
    if (media !== undefined) {
      let bytes: Uint8Array;
      let mime = media.mime;
      let name = media.name;
      const prepared = await deps.preparedFile(entry);
      if (prepared?.bytes !== undefined) {
        bytes = prepared.bytes;
      } else if (media.gifUrl !== undefined) {
        const loaded = await deps.loadGif(media.gifUrl);
        bytes = loaded.bytes;
        mime = loaded.mime;
        if (media.meta.kind === "gif" && loaded.mime === "image/webp") name = name.replace(/\.gif$/, ".webp");
      } else {
        // Nothing left to send: the file is gone with the store.
        return "refused";
      }
      let thumb: ThumbRefBytes | undefined = media.thumb;
      if (thumb === undefined && media.poster !== undefined && prepared?.posterBytes !== undefined) {
        try {
          const t = await deps.uploadPoster(prepared.posterBytes, media.poster.mime);
          thumb = {
            blobId: t.blobId,
            key: t.key,
            nonce: t.nonce,
            mime: media.poster.mime,
            size: prepared.posterBytes.length,
            width: media.poster.width,
            height: media.poster.height,
          };
        } catch (err) {
          // As the chat view does: a poster that did not upload is left out,
          // unless the network is the reason, when the whole send waits.
          if (deps.isNetwork(err)) throw err;
        }
      }
      const ref = await deps.upload(bytes, { mime, name }, { ...media.meta, ...(thumb === undefined ? {} : { thumb }) });
      attachments = [ref];
      await deps.uploaded(entry, ref);
    }
    if (entry.parts !== undefined) {
      attachments = await finishUnsentParts(entry, deps);
      await deps.partsDone(entry, attachments);
    }
    // As the open view: checked right before the publish, after any upload,
    // so a held message always carries finished references.
    const verdict = await deps.sendVerdict(entry, group);
    if (verdict === "give-up") return "refused";
    if (verdict === "wait") return deps.hold(entry, attachments) ? "dropped" : "refused";
    const envelope = await deps.compose(entry, attachments, group);
    const result = await deps.publish(group, envelope);
    await deps.file(entry, attachments, result, group);
    return "sent";
  } catch (err) {
    if (deps.holdForSuccessor(entry, attachments, err)) return "dropped";
    return deps.isNetwork(err) ? "network" : "refused";
  }
}
