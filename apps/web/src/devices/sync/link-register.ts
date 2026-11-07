// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0087 phase B — the account's link register (ADR-0079 section 4) for
 * code that is not under the DeviceGroup sync provider: the channel and
 * group link modules record a `/g/` link here as they mint it and revoke it
 * here, so every device of the account lists and revokes it.
 *
 * `DeviceGroupSyncContext` installs the port while it runs. Reads are
 * reactive (the port is a signal, and its `records` an accessor).
 *
 * The revoke follows the contact links' order (`InviteLinksPanel`): the row
 * deletion is marked owed, the register revokes, then this device deletes the
 * ledger row with the seed the record carries; an unreachable ledger leaves
 * it owed, retried by `MailboxContext` at every change of the register.
 */
import { createSignal, type Accessor } from "solid-js";

import { DiscoveryClient } from "../../discovery/client";
import { sessionTokens } from "../../auth/session-token-provider";
import { deleteRevokedLinkRow, setOwedRowDeletion } from "../../contacts/own-links";
import { loadWasm, wasmSync } from "../../wasm";
import { composeSealedInviteLink, inviteLinkWidths } from "../../contacts/invite-link";
import { GROUP_INVITE_URL_PREFIX } from "../../contacts/group-invite";
import { publicOrigin } from "../../platform";
import { hexOf, listedTargetLinks, type LinkRecordModel, type NewLinkRecord } from "./link-records";

/** What the register offers outside the provider. */
export interface LinkRegisterPort {
  readonly records: Accessor<readonly LinkRecordModel[]>;
  /** The account the register belongs to, or `null`. */
  accountIdB64(): string | null;
  /** Record a link just minted; `false` when nothing was kept. */
  set(record: NewLinkRecord): Promise<boolean>;
  /** Mark a link revoked (final); `false` when nothing changed. */
  revoke(linkId: Uint8Array): Promise<boolean>;
}

const [port, setPort] = createSignal<LinkRegisterPort | null>(null);

/** `DeviceGroupSyncContext` installs (or removes) the register. */
export function installLinkRegister(p: LinkRegisterPort | null): void {
  setPort(() => p);
}

/** The register, or `null` while no sync provider runs. Reactive. */
export function linkRegister(): LinkRegisterPort | null {
  return port();
}

/** Reactive: every record of the register (none while it is not running). */
export function registeredLinks(): readonly LinkRecordModel[] {
  return port()?.records() ?? [];
}

/** The register key of a `/g/` link: `groupLinkRecordId(ledgerId)`. */
export async function groupLinkRecordIdOf(ledgerId: Uint8Array): Promise<Uint8Array> {
  const wasm = await loadWasm();
  return wasm.groupLinkRecordId(ledgerId);
}

/**
 * Record a `/g/` link just minted. `false` when no register runs or it kept
 * nothing (the link still works; it is then listed only where its local
 * record is). Never throws.
 */
export async function recordGroupLink(
  record: Omit<NewLinkRecord, "linkId"> & { readonly kind: "group" | "channel" | "publisher" },
): Promise<boolean> {
  const p = port();
  if (p === null) return false;
  try {
    const linkId = await groupLinkRecordIdOf(record.ledgerId);
    return await p.set({ ...record, linkId });
  } catch {
    return false;
  }
}

/** How a revoke through the register ended. */
export type RegisteredRevokeOutcome = "done" | "unreachable" | "no-register";

/**
 * Revoke a `/g/` link through the register and delete its ledger row with
 * the record's revocation seed. Run the link's own service step (a private
 * channel's invite key) BEFORE this. Throws only for a ledger refusal that
 * is not the network.
 */
export async function revokeRegisteredLink(record: LinkRecordModel): Promise<RegisteredRevokeOutcome> {
  const p = port();
  const account = p?.accountIdB64() ?? null;
  if (p === null || account === null) return "no-register";
  const linkIdHex = hexOf(record.linkId);
  // Owed before the register write: a crash in between still deletes it.
  setOwedRowDeletion(account, linkIdHex, true);
  await p.revoke(record.linkId);
  const discovery = new DiscoveryClient(undefined, { sessionToken: () => sessionTokens.peek() });
  const outcome = await deleteRevokedLinkRow(discovery, account, { ...record, linkIdHex });
  return outcome === "unreachable" ? "unreachable" : "done";
}

/** A live `/g/` link of the account, with its URL. */
export interface RegisteredGroupLink {
  readonly record: LinkRecordModel;
  readonly url: string;
}

/**
 * Reactive: the account's live `/g/` links of one kind for one target (a
 * group id or a channel id, hex), every device's, newest first, each with
 * its `<origin>/g/<id>#<secret>` URL. Empty until the core is loaded.
 */
export function registeredGroupLinks(
  kind: "group" | "channel" | "publisher",
  targetIdHex: string,
): RegisteredGroupLink[] {
  const records = listedTargetLinks(registeredLinks(), kind, targetIdHex, Date.now());
  if (records.length === 0) return [];
  let widths;
  try {
    widths = inviteLinkWidths(wasmSync());
  } catch {
    return [];
  }
  const out: RegisteredGroupLink[] = [];
  for (const record of records) {
    try {
      const url = composeSealedInviteLink({
        origin: publicOrigin(),
        prefix: GROUP_INVITE_URL_PREFIX,
        blobId: record.ledgerId,
        secret: record.linkSecret,
        widths,
      });
      out.push({ record, url });
    } catch {
      // A record whose fields do not make a link is left out.
    }
  }
  return out;
}
