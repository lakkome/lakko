// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0089 section 6, UX `reporting-and-moderation.md` sections 1.2 and 1.3 -
 * blocking and unblocking a person, the one way each runs on this client.
 *
 * Block: the register first (it takes effect on this device at once, and on
 * the account's other devices when they sync), then the existing Remove
 * contact, which also stops their way to this account's inbox by rotating
 * the `contact` key. Unblock writes the register only: it does not make the
 * person a contact again.
 */

import { base64Decode } from "../auth/account-id";
import { removeContactByKey } from "../contacts/remove-contact";
import { requestConfirm } from "../ui/confirm-dialog";
import { t } from "../l10n";
import { writeBlock } from "./block-registry";

/** The ports a block needs, so a test runs it without the app. */
export interface BlockPorts {
  readonly write: (accountKey: Uint8Array, blocked: boolean) => Promise<boolean>;
  readonly removeContact: (keyB64: string) => void;
}

const DEFAULT_PORTS: BlockPorts = {
  write: writeBlock,
  removeContact: (keyB64) => {
    removeContactByKey(keyB64);
  },
};

/** Block the person whose account key is `keyB64` (standard base64).
 *  Throws when the register cannot be written (nothing changed then). */
export async function blockPerson(keyB64: string, ports: BlockPorts = DEFAULT_PORTS): Promise<void> {
  const key = base64Decode(keyB64);
  if (key.length !== 32) throw new Error("an account key is 32 bytes");
  await ports.write(key, true);
  ports.removeContact(keyB64);
}

/** Unblock the person whose account key is `keyB64`. Throws when the register
 *  cannot be written. */
export async function unblockPerson(keyB64: string, ports: BlockPorts = DEFAULT_PORTS): Promise<void> {
  const key = base64Decode(keyB64);
  if (key.length !== 32) throw new Error("an account key is 32 bytes");
  await ports.write(key, false);
}

/** The block confirmation (UX 1.2): `true` when the person chose Block. */
export function confirmBlock(name: string): Promise<boolean> {
  return requestConfirm({
    title: t("block.confirm.title", { name }),
    body: t("block.confirm.body"),
    confirmLabel: t("block.action"),
    cancelLabel: t("common.cancel"),
    tone: "danger",
  });
}
