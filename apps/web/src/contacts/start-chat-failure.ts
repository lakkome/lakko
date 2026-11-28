// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Why starting a one-to-one from a contact's Message failed, as plain text
 * for the log. Built from the error's TYPE and status only, never from its
 * message: those carry endpoints with journal ids and account ids.
 */
import { JournalClaimFailedError } from "../ds/ownership";
import { DsHttpError, DsNetworkError } from "../ds/client";
import { DiscoveryNetworkError } from "../discovery/client";
import { MailboxUnreachableError } from "../reach/mailbox-write";
import {
  ContactsError,
  ContactStartedOver,
  IdentityCheckInconclusive,
  SupersessionRefused,
} from "./client";
import { findInChain } from "./errors";

export function startChatFailureReason(err: unknown): string {
  const unreachable = findInChain(err, (v) => (v instanceof MailboxUnreachableError ? v : null));
  if (unreachable !== null) {
    return unreachable.tried === 0
      ? "no reach grant is held for this person, so the Welcome cannot be written"
      : `every reach grant held for this person was refused (${unreachable.tried})`;
  }
  if (findInChain(err, (v) => (v instanceof SupersessionRefused ? v : null)) !== null) {
    return "the person's identity key changed without a proof";
  }
  if (findInChain(err, (v) => (v instanceof IdentityCheckInconclusive ? v : null)) !== null) {
    return "the identity check could not finish";
  }
  if (findInChain(err, (v) => (v instanceof ContactStartedOver ? v : null)) !== null) {
    return "the person started over with a new account this device could not confirm";
  }
  const claim = findInChain(err, (v) => (v instanceof JournalClaimFailedError ? v : null));
  const http = findInChain(err, (v) => (v instanceof DsHttpError ? v : null));
  if (http !== null) {
    return `${claim !== null ? "the new chat's journal claim" : "the delivery service"} answered ${http.status}`;
  }
  if (findInChain(err, (v) => (v instanceof DsNetworkError ? v : null)) !== null) {
    return "the delivery service could not be reached";
  }
  if (findInChain(err, (v) => (v instanceof DiscoveryNetworkError ? v : null)) !== null) {
    return "the directory could not be reached";
  }
  if (claim !== null) return "the new chat's journal could not be claimed";
  const contacts = findInChain(err, (v) => (v instanceof ContactsError ? v : null));
  switch (contacts?.variant.kind) {
    case "no-identity-secrets":
      return "the session has no identity secrets";
    case "no-identity-bundle":
      return "the person has no identity bundle in the directory";
    default:
      break;
  }
  // The innermost error's type, never its message.
  let inner: unknown = err;
  findInChain(err, (v) => {
    inner = v;
    return null;
  });
  return inner instanceof Error ? `failed with ${inner.name}` : "failed with a non-error value";
}
