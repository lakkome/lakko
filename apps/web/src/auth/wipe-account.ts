// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The one local wipe behind Forget device and Delete account (2026-09-30).
 *
 * Log out keeps an account's local data for its next sign-in on this device.
 * Forget device and Delete account remove ALL of it: every store keyed by the
 * account (or holding rows tagged with it) and every retired device-wide row
 * that may still hold its data. Another account's data on this browser and
 * the genuine device settings stay.
 *
 * {@link wipeAccountLocalData} is the only list. `AuthProvider.forgetDevice`
 * (which Delete account calls after the server-side deletion) releases the
 * push wake binding and this app's delivered notifications first, then calls
 * it; nothing else keeps a private partial list. Each step is best effort: a
 * store that fails is logged by step name (plain ASCII, never an account id
 * or a secret) and the remaining steps still run.
 *
 * The device root key goes first so any ciphertext a later failed step
 * leaves behind is already unreadable; the plaintext restore marker goes
 * last, so a wipe interrupted half way still finds the account on the next
 * page load and can be run again.
 *
 * Left on purpose, and why:
 *
 * - `lakko.transparency.last-seen-root.v1`: the device's KT-log root floor.
 * - `lakko.push.device.v1`: the device push pseudonym; `unsubscribeFromPush`
 *   in `forgetDevice` handles it.
 * - `lakko-push-routing` / `wake_binding`: device-wide, released by
 *   `releaseWakeBinding` before this runs.
 * - theme, language and media preferences, the GIF privacy note: device
 *   settings. (The emoji Recent row and tone choices are account data since
 *   2026-10-01 and go with the `emoji-prefs` step.)
 * - the app-shell cache of the service worker: code, no account data.
 */
import { base64Decode, base64UrlEncodeNoPad } from "./account-id";
import { deleteDeviceKey } from "./key-store";
import { deleteSessionToken } from "./session-token-store";
import { deleteExportKey } from "./export-key-store";
import { deleteDeviceIdentitySeed } from "./device-identity-store";
import { deleteAccountIdentitySeed } from "./account-identity-seed-store";
import { clearSignupPhraseOwed } from "./phrase-gate";
import { clearPersistedAccountFor } from "./persisted-account";
import { deleteDeviceGroupSnapshot } from "../devices/device-group-snapshot-store";
import { deleteSyncState } from "../devices/sync/sync-state-idb";
import { clearKnownGroupIds } from "../devices/state-bundle";
import { clearDeviceLinkAnswers } from "../devices/device-link";
import { clearAddedNotice } from "../devices/added-notice";
import { clearAccountContacts } from "../contacts/local-store";
import { clearInboxForAccount } from "../contacts/inbox-store";
import { clearSentRequests } from "../contacts/sent-requests";
import { clearInviteLinkSecrets } from "../contacts/invite-link-secrets";
import { deleteMintedLinksForAccount } from "../contacts/minted-links";
import { deleteMlsSnapshot } from "../chat/mls-store";
import { deleteOutboxForAccount } from "../chat/commit-outbox";
import { deleteMessageLog } from "../chat/message-log-store";
import { clearObservationsForAccount } from "../chat/pending-requests";
import { clearUnappliedForAccount } from "../chat/unapplied-requests";
import { clearOwnRequestsForAccount } from "../chat/membership-retry";
import { clearBeaconObservationsForAccount } from "../chat/committer-beacon";
import { clearLastObservedOwnerForAccount } from "../chat/ownership-transfer";
import { clearConversationClosedForAccount } from "../chat/conversation-closed";
import { clearLeaveRequestedForAccount } from "../chat/membership-ended";
import { clearRemovedDevicesForAccount } from "../chat/device-readmission";
import { clearKnockCursorsForAccount } from "../chat/knock-cursor";
import { deleteGroupClosedRow } from "../chat/group-closed-idb";
import { deleteGroupNameRow } from "../chat/group-name-idb";
import { deletePinRow } from "../chat/pin-idb";
import { deleteReadWatermarkRow } from "../chat/read-watermark-idb";
import { clearSelectedTab } from "../chat/folders/selected-tab";
import { deleteCallLogRow } from "../calls/call-log-idb";
import { clearGroupCallsForAccount } from "../calls/group-call-store";
import { wipeChannelsForAccount } from "../state/channels-state";
import { clearOrphanedByResetForAccount } from "../channels/orphaned-by-reset";
import { clearRotationStatesForAccount } from "../channels/rotation-state-store";
import { clearUnconfirmedFoundingsForAccount } from "../channels/composer";
import { deleteProfileRow } from "../profile/profile-idb";
import { clearOwnProfile } from "../profile/own-profile";
import { clearMemberBaselinesForAccount } from "../profile/membership-baseline";
import { clearProfileRequestsForAccount } from "../profile/profile-request";
import { clearGrantSignatures } from "../profile/grant-signatures";
import { deleteReachRows } from "../reach/reach-store";
import { deleteFeedState } from "../feed/feed-store";
import { deleteFeedMarks } from "../feed/feed-marks-store";
import { deleteDiscoverRows } from "../discover/discover-store";
import { clearFeedSelection } from "../feed/feed-selection-store";
import { deletePendingRootSeedRow, deleteRegistrarSeedRow } from "../ds/registrar-seed-idb";
import { clearAppointmentRecords } from "../ds/appointments";
import { clearRegistrarLists } from "../ds/registrar-list";
import { clearOwedHandovers } from "../ds/root-handover";
import { clearRootHeldAll } from "../ds/root-held";
import { deletePhrase } from "../backup/phrase-store";
import { clearBudget, clearThreeWordsSuspended } from "./recover/three-words";
import { clearKeyResetOwed } from "./key-reset-owed";
import { deleteFloor } from "../backup/floor-store";
import { clearAutoBackupState } from "../backup/auto-backup";
import { clearGroupRoutingForAccount } from "../push/group-routing-store";
import { deleteAnonTokensForAccount } from "../tokens/anon-token-store";
import { clearEmojiPrefsForAccount } from "../media/emoji/emoji-index";

/**
 * Retired device-wide `localStorage` rows that may still hold the wiped
 * account's data and that no per-account clear owns. (The retired rows of
 * the contacts, knock-cursor, known-groups, rotation-state, founding and
 * emoji-preference stores are dropped by those stores' own clears above.)
 */
const RETIRED_DEVICE_WIDE_KEYS = [
  // ADR-0060 section 6: the device mailbox cursor, closed and no longer read.
  "lakko.contacts.device-mailbox.v1",
  // The plaintext subscriber keypairs, migrated into the channel log.
  "lakko.channels.subscriber-keys.v1",
] as const;

/** One store of the wipe. */
export interface WipeStep {
  /** Plain ASCII, logged on failure. */
  readonly name: string;
  readonly run: () => void | Promise<void>;
}

/** What {@link wipeAccountLocalData} could not remove. */
export interface WipeReport {
  /** Names of the steps that threw; empty when every store was wiped. */
  readonly failed: readonly string[];
}

/** base64url (no padding) of the account id, the key two stores file under;
 *  `null` when `accountIdB64` does not decode. */
function urlKeyOf(accountIdB64: string): string | null {
  try {
    return base64UrlEncodeNoPad(base64Decode(accountIdB64));
  } catch {
    return null;
  }
}

function removeLocal(key: string): void {
  if (typeof localStorage === "undefined") return;
  localStorage.removeItem(key);
}

/**
 * Every step of the wipe for `accountIdB64` (standard base64 of the account
 * id, the form `AuthState` carries), in the order they run. Exported so the
 * test can name what it covers.
 */
export function accountWipeSteps(accountIdB64: string): WipeStep[] {
  const urlKey = urlKeyOf(accountIdB64);
  const storage = typeof localStorage === "undefined" ? undefined : localStorage;
  return [
    // --- the device root key first (see the module doc) -------------------
    { name: "device-key", run: () => deleteDeviceKey(accountIdB64) },
    // --- IndexedDB, one row (or row set) per account ----------------------
    { name: "mls-snapshot", run: () => deleteMlsSnapshot(accountIdB64) },
    { name: "commit-outbox", run: () => deleteOutboxForAccount(accountIdB64) },
    { name: "message-log", run: () => deleteMessageLog(accountIdB64) },
    // The channel log (it holds the publisher signing seeds) and then the
    // account's channel publish counters, plus the retired device-wide
    // counter rows: a counter goes only with the key it protects, so it is
    // removed inside this step, after the log delete succeeded.
    { name: "channel-log", run: () => wipeChannelsForAccount(accountIdB64) },
    // ADR-0065 phase 2 (UX 16.6) — which channels this device owned under
    // keys since reset. Its own step: a plain localStorage row that must go
    // whether or not the channel log's IndexedDB delete did.
    { name: "channels-orphaned", run: () => clearOrphanedByResetForAccount(accountIdB64) },
    { name: "profiles", run: () => deleteProfileRow(accountIdB64) },
    { name: "group-names", run: () => deleteGroupNameRow(accountIdB64) },
    { name: "group-closed", run: () => deleteGroupClosedRow(accountIdB64) },
    { name: "registrar-seeds", run: () => deleteRegistrarSeedRow(accountIdB64) },
    { name: "pending-root-seeds", run: () => deletePendingRootSeedRow(accountIdB64) },
    { name: "pins", run: () => deletePinRow(accountIdB64) },
    { name: "reach", run: () => deleteReachRows(accountIdB64) },
    // The person's own feeds on this device (ADR-0063 section 6).
    { name: "feeds", run: () => deleteFeedState(accountIdB64) },
    // What the feed remembers (docs/ux/feed.md section 5): hidden
    // conversations, the "New" points, the channels' newest-read posts.
    { name: "feed-marks", run: () => deleteFeedMarks(accountIdB64) },
    // Discover (ADR-0059): picked topics, decoys, cached buckets,
    // not-interested channels, the share switches and the lists members
    // shared.
    { name: "discover", run: () => deleteDiscoverRows(accountIdB64) },
    { name: "read-watermarks", run: () => deleteReadWatermarkRow(accountIdB64) },
    { name: "call-log", run: () => deleteCallLogRow(accountIdB64) },
    { name: "device-sync", run: () => deleteSyncState(accountIdB64) },
    { name: "minted-links", run: () => deleteMintedLinksForAccount(accountIdB64) },
    { name: "push-routing", run: () => clearGroupRoutingForAccount(accountIdB64) },
    {
      name: "anon-tokens",
      run: () => (urlKey === null ? undefined : deleteAnonTokensForAccount(urlKey)),
    },
    { name: "session-token", run: () => deleteSessionToken(accountIdB64) },
    { name: "backup-phrase", run: () => deletePhrase(accountIdB64) },
    { name: "backup-floor", run: () => deleteFloor(accountIdB64) },
    { name: "export-key", run: () => deleteExportKey(accountIdB64) },
    { name: "device-identity-seed", run: () => deleteDeviceIdentitySeed(accountIdB64) },
    { name: "account-identity-seed", run: () => deleteAccountIdentitySeed(accountIdB64) },
    { name: "device-group-snapshot", run: () => deleteDeviceGroupSnapshot(accountIdB64) },
    // --- localStorage, keyed by the account --------------------------------
    { name: "contacts", run: () => clearAccountContacts(accountIdB64) },
    { name: "contact-inbox", run: () => clearInboxForAccount(accountIdB64) },
    { name: "sent-requests", run: () => clearSentRequests(accountIdB64) },
    {
      name: "invite-link-secrets",
      run: () => (urlKey === null ? undefined : clearInviteLinkSecrets(urlKey)),
    },
    { name: "readmission-removed", run: () => clearRemovedDevicesForAccount(accountIdB64) },
    { name: "signup-phrase-owed", run: () => clearSignupPhraseOwed(storage, accountIdB64) },
    // The local three-word guess budget and the ADR-0065 "publish owed"
    // record (a reset whose attestation was not yet carried to the directory
    // and the device list).
    { name: "three-words-budget", run: () => clearBudget(storage, accountIdB64) },
    {
      name: "three-words-suspended",
      run: () => clearThreeWordsSuspended(storage, accountIdB64),
    },
    {
      name: "key-reset-owed",
      run: () => clearKeyResetOwed(storage, accountIdB64),
    },
    { name: "pending-requests", run: () => clearObservationsForAccount(accountIdB64) },
    { name: "unapplied-requests", run: () => clearUnappliedForAccount(accountIdB64) },
    { name: "own-requests", run: () => clearOwnRequestsForAccount(accountIdB64) },
    { name: "committer-beacons", run: () => clearBeaconObservationsForAccount(accountIdB64) },
    { name: "last-observed-owner", run: () => clearLastObservedOwnerForAccount(accountIdB64) },
    { name: "leave-requested", run: () => clearLeaveRequestedForAccount(accountIdB64) },
    { name: "conversation-closed", run: () => clearConversationClosedForAccount(accountIdB64) },
    // The per-account conversation-kind map of builds before ADR-0062 (the
    // kind now lives in the group); nothing writes it any more.
    {
      name: "conversation-kinds-retired",
      run: () => removeLocal(`lakko.conversation-kind.v1.${accountIdB64}`),
    },
    { name: "folder-tab", run: () => clearSelectedTab(accountIdB64) },
    { name: "feed-selection", run: () => clearFeedSelection(accountIdB64) },
    { name: "own-profile", run: () => clearOwnProfile(accountIdB64) },
    { name: "member-baseline", run: () => clearMemberBaselinesForAccount(accountIdB64) },
    { name: "profile-requests", run: () => clearProfileRequestsForAccount(accountIdB64) },
    { name: "profile-grants", run: () => clearGrantSignatures(accountIdB64) },
    { name: "device-link-answers", run: () => clearDeviceLinkAnswers(accountIdB64) },
    // The "a device was added" notice state (identity-recovery UX 17.5).
    { name: "device-added-notice", run: () => clearAddedNotice(accountIdB64) },
    { name: "ds-appointments", run: () => clearAppointmentRecords(accountIdB64) },
    { name: "ds-registrar-list", run: () => clearRegistrarLists(accountIdB64) },
    { name: "ds-root-handover", run: () => clearOwedHandovers(accountIdB64) },
    { name: "ds-root-held", run: () => clearRootHeldAll(accountIdB64) },
    { name: "auto-backup", run: () => clearAutoBackupState(accountIdB64) },
    { name: "knock-cursors", run: () => clearKnockCursorsForAccount(accountIdB64) },
    { name: "known-group-ids", run: () => clearKnownGroupIds(accountIdB64) },
    { name: "channel-rotation", run: () => clearRotationStatesForAccount(accountIdB64) },
    { name: "unconfirmed-foundings", run: () => clearUnconfirmedFoundingsForAccount(accountIdB64) },
    { name: "emoji-prefs", run: () => clearEmojiPrefsForAccount(accountIdB64) },
    {
      name: "retired-device-wide",
      run: () => {
        for (const key of RETIRED_DEVICE_WIDE_KEYS) removeLocal(key);
      },
    },
    // --- sessionStorage -----------------------------------------------------
    { name: "group-calls", run: () => clearGroupCallsForAccount(accountIdB64) },
    // --- last: the plaintext restore marker (see the module doc) -----------
    { name: "restore-marker", run: () => clearPersistedAccountFor(accountIdB64) },
  ];
}

/**
 * Remove every piece of `accountIdB64`'s local data on this device. Never
 * throws; the report names the steps that failed (each is logged too).
 */
export async function wipeAccountLocalData(accountIdB64: string): Promise<WipeReport> {
  const failed: string[] = [];
  for (const step of accountWipeSteps(accountIdB64)) {
    try {
      await step.run();
    } catch (err) {
      failed.push(step.name);
      // eslint-disable-next-line no-console
      console.warn(
        `[wipe] ${step.name} failed: ${err instanceof Error ? err.name : "unknown error"}`,
      );
    }
  }
  return { failed };
}
