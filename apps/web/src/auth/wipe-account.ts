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
 * {@link wipeAccountLocalData} is the only list (the staged wipe of ADR-0074
 * is that list minus two named steps, never a second list). `AuthProvider.forgetDevice`
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
 * - `lakko.push.token.v1`: the browser's Web Push token (device-wide, ADR-0078);
 *   `unsubscribeFromPush` in `forgetDevice` handles it, after it removed the
 *   account's sealed push subscriptions.
 * - `lakko-push-routing` / `wake_binding`: device-wide, released by
 *   `releaseWakeBinding` before this runs.
 * - theme, text size, language and media preferences, the GIF privacy note: device
 *   settings. (The emoji Recent row and tone choices are account data since
 *   2026-10-01 and go with the `emoji-prefs` step.)
 * - the app-shell cache of the service worker: code, no account data.
 */
import { deletePersonStatusRow } from "../people/account-status-idb";
import { clearPendingPublisherRequests } from "../channels/pending-publisher-requests";
import { base64Decode, base64UrlEncodeNoPad } from "./account-id";
import {
  WIPED_MARKER_TTL_MS,
  finishAccountWipe,
  pruneWipedMarker,
  sealAccountStoresForWipe,
} from "./account-store-gate";
import { deleteDeviceKey } from "./key-store";
import { deleteSessionToken } from "./session-token-store";
import { deleteExportKey } from "./export-key-store";
import { deleteDeviceIdentitySeed } from "./device-identity-store";
import { deleteAccountIdentitySeed } from "./account-identity-seed-store";
import { deleteKeyRotationState } from "./key-rotation-store";
import { clearRotatedNotice, clearRotation } from "../devices/key-rotation";
import { clearGuestAccount, guestPhraseKey } from "./guest-account";
import { clearSignupPhraseOwed } from "./phrase-gate";
import { clearProtectionFacts } from "./protect/account-protection";
import { clearNoticeStores } from "./protect/notices";
import { clearFirstPasswordOwed } from "./username-claim";
import { clearRenameStores } from "./username-change";
import { clearPersistedAccountFor } from "./persisted-account";
import { deleteDeviceGroupSnapshot } from "../devices/device-group-snapshot-store";
import { deleteSyncState } from "../devices/sync/sync-state-idb";
import { clearKnownGroupIds } from "../devices/state-bundle";
import { clearDeviceLinkAnswers } from "../devices/device-link";
import { clearAddedNotice } from "../devices/added-notice";
import { clearAccountContacts } from "../contacts/local-store";
import {
  predecessorAdoptionStorageKey,
  predecessorCursorStorageKey,
} from "../contacts/address-resolver";
import { clearInboxForAccount } from "../contacts/inbox-store";
import { clearSentRequests } from "../contacts/sent-requests";
import { deleteSentIntrosForAccount } from "../contacts/sent-request-intros";
import { clearSpendOwed } from "../contacts/rotation-spent";
import { clearInviteLinkSecrets } from "../contacts/invite-link-secrets";
import { deleteMintedLinksForAccount } from "../contacts/minted-links";
import { deletePendingContactsForAccount } from "../contacts/pending-contacts";
import { deleteCarriedCopiesForAccount } from "../contacts/carried-copies";
import { deleteMlsSnapshot } from "../chat/mls-store";
import { deleteOutboxForAccount } from "../chat/commit-outbox";
import { deleteMessageLog } from "../chat/message-log-store";
import { clearObservationsForAccount } from "../chat/pending-requests";
import { clearUnappliedForAccount } from "../chat/unapplied-requests";
import { clearOwnRequestsForAccount } from "../chat/membership-retry";
import { clearBeaconObservationsForAccount } from "../chat/committer-beacon";
import { clearLastObservedOwnerForAccount } from "../chat/ownership-transfer";
import { clearLeaveRequestedForAccount } from "../chat/membership-ended";
import { clearRemovedDevicesForAccount } from "../chat/device-readmission";
import { clearKnockCursorsForAccount } from "../chat/knock-cursor";
import { clearCursorEpochsForAccount } from "../chat/cursor-epoch";
import { conversationBehind } from "../chat/conversation-behind";
import { clearReentryRowsForAccount } from "../chat/reentry-store";
import { deleteMembershipEndRow } from "../chat/membership-end-idb";
import { deleteGroupClosedRow } from "../chat/group-closed-idb";
import { deleteGroupNameRow } from "../chat/group-name-idb";
import { deletePinRow } from "../chat/pin-idb";
import { deleteTranslationRows } from "../translation/result-idb";
import { deleteReadWatermarkRow } from "../chat/read-watermark-idb";
import { clearSelectedTab } from "../chat/folders/selected-tab";
import { deleteCallLogRow } from "../calls/call-log-idb";
import { deleteBackgroundPhotos } from "../calls/background-photos";
import { clearGroupCallsForAccount } from "../calls/group-call-store";
import { dropTraysForAccount } from "../chat/tray-store";
import { wipeChannelsForAccount } from "../state/channels-state";
import { clearClaimedHandles } from "../channels/claimed-handles";
import { clearClaimedGroupHandles } from "../chat/claimed-group-handles";
import { clearOrphanedByResetForAccount } from "../channels/orphaned-by-reset";
import { clearOwnerMoved } from "../channels/owner-moved";
import { clearRotationStatesForAccount } from "../channels/rotation-state-store";
import { clearUnconfirmedFoundingsForAccount } from "../channels/composer";
import { deleteProfileRow } from "../profile/profile-idb";
import { deleteAvatarCache } from "../profile/avatar-cache-idb";
import { deleteAccountKv } from "../storage/account-kv";
import { clearOwnProfile } from "../profile/own-profile";
import { clearMemberBaselinesForAccount } from "../profile/membership-baseline";
import { clearProfileRequestsForAccount } from "../profile/profile-request";
import { clearGrantSignatures } from "../profile/grant-signatures";
import { deleteReachRows } from "../reach/reach-store";
import { clearContinuityStateForAccount } from "../chat/continuity-store";
import { clearVerifiedBindingsForAccount } from "../contacts/verified-binding";
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
import { clearGenesisRecorded, clearKnownDeviceId } from "./session-device";
import { clearNoPrfPasskeys } from "./passkey-no-prf";
import { clearLogoutOwed } from "./logout-owed";
import { clearRemovals } from "../devices/removal";
import { deleteRecallRecord } from "./recall-record";
import { deleteFloor } from "../backup/floor-store";
import { clearAutoBackupState } from "../backup/auto-backup";
import { clearRestoredKnownGroups } from "../backup/restored-known-groups";
import { clearBackupRestoreOwed } from "../backup/restore-owed";
import { clearRestoreMarker } from "../backup/restored-marker";
import { clearGroupRoutingForAccount } from "../push/group-routing-store";
import { clearSupersededMirrorForAccount } from "../push/superseded-mirror";
import { deleteAnonTokensForAccount } from "../tokens/anon-token-store";
import { clearEmojiPrefsForAccount } from "../media/emoji/emoji-index";
import { clearChannelNotified } from "../feed/channel-notified-store";
import { cardsStorageKey, cityStorageKey, showCityStorageKey } from "../devices/device-cards";

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

/** Every form `accountIdB64`'s rows are filed under: the standard base64
 *  the auth state carries and the base64url two stores use. What the store
 *  gate (`account-store-gate.ts`) seals and opens. */
export function accountStoreIds(accountIdB64: string): string[] {
  const urlKey = urlKeyOf(accountIdB64);
  return urlKey === null || urlKey === accountIdB64 ? [accountIdB64] : [accountIdB64, urlKey];
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
    // ADR-0074 section 1.1: the recall record a device keeps after the
    // account's keys were reset elsewhere. Sealed under the key above, so it
    // is unreadable from here on; the row goes too. The full wipe never
    // leaves one (a removed device has nothing to take back).
    { name: "recall-record", run: () => deleteRecallRecord(accountIdB64) },
    // --- IndexedDB, one row (or row set) per account ----------------------
    { name: "mls-snapshot", run: () => deleteMlsSnapshot(accountIdB64) },
    { name: "commit-outbox", run: () => deleteOutboxForAccount(accountIdB64) },
    { name: "message-log", run: () => deleteMessageLog(accountIdB64) },
    // The channel log (it holds the publisher signing seeds) and then the
    // account's channel publish counters, plus the retired device-wide
    // counter rows: a counter goes only with the key it protects, so it is
    // removed inside this step, after the log delete succeeded.
    { name: "channel-log", run: () => wipeChannelsForAccount(accountIdB64) },
    // ADR-0073 phase 2 (UX 16.6) — which channels this device owned under
    // keys since reset. Its own step: a plain localStorage row that must go
    // whether or not the channel log's IndexedDB delete did.
    { name: "channels-orphaned", run: () => clearOrphanedByResetForAccount(accountIdB64) },
    { name: "channels-claimed-handles", run: () => clearClaimedHandles(accountIdB64) },
    { name: "groups-claimed-handles", run: () => clearClaimedGroupHandles(accountIdB64) },
    // ADR-0074 section 3.5: which channels' owner key moved to the key of a
    // rotated identity.
    { name: "channels-owner-moved", run: () => clearOwnerMoved(accountIdB64) },
    { name: "profiles", run: () => deleteProfileRow(accountIdB64) },
    { name: "avatars", run: () => deleteAvatarCache(accountIdB64) },
    { name: "group-names", run: () => deleteGroupNameRow(accountIdB64) },
    { name: "account-kv", run: () => deleteAccountKv(accountIdB64) },
    { name: "group-closed", run: () => deleteGroupClosedRow(accountIdB64) },
    // ADR-0098: which people this device learned were deleted.
    { name: "person-status", run: () => deletePersonStatusRow(accountIdB64) },
    // ADR-0087 section 9: ends under way, the notices not yet closed and the
    // Welcomes held.
    { name: "membership-end", run: () => deleteMembershipEndRow(accountIdB64) },
    { name: "registrar-seeds", run: () => deleteRegistrarSeedRow(accountIdB64) },
    { name: "pending-root-seeds", run: () => deletePendingRootSeedRow(accountIdB64) },
    { name: "pins", run: () => deletePinRow(accountIdB64) },
    // ADR-0082 section 5: translations, transcripts and the per-conversation
    // translation choices this device made.
    { name: "translations", run: () => deleteTranslationRows(accountIdB64) },
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
    { name: "call-backgrounds", run: () => deleteBackgroundPhotos(accountIdB64) },
    { name: "device-sync", run: () => deleteSyncState(accountIdB64) },
    { name: "minted-links", run: () => deleteMintedLinksForAccount(accountIdB64) },
    // The intro text of the unanswered contact requests this device sent,
    // kept under the device key for a re-send after a key rotation.
    { name: "sent-request-intros", run: () => deleteSentIntrosForAccount(accountIdB64) },
    // ADR-0079: the inviters added through a link, and what was written to
    // them before their app answered.
    { name: "pending-contacts", run: () => deletePendingContactsForAccount(accountIdB64) },
    // The display-only copies of what this account wrote to a pending
    // contact on another of its devices (pairing state bundle v3).
    { name: "carried-copies", run: () => deleteCarriedCopiesForAccount(accountIdB64) },
    { name: "push-routing", run: () => clearGroupRoutingForAccount(accountIdB64) },
    // ADR-0073 R.8: the service worker's copy of the keys a reset retired.
    { name: "superseded-mirror", run: () => clearSupersededMirrorForAccount(accountIdB64) },
    {
      name: "anon-tokens",
      run: () => (urlKey === null ? undefined : deleteAnonTokensForAccount(urlKey)),
    },
    { name: "session-token", run: () => deleteSessionToken(accountIdB64) },
    { name: "backup-phrase", run: () => deletePhrase(accountIdB64) },
    // A guest account's phrase is kept under a key of its own
    // (`guest-account.ts`), so it has a step of its own.
    { name: "guest-phrase", run: () => deletePhrase(guestPhraseKey(accountIdB64)) },
    { name: "backup-floor", run: () => deleteFloor(accountIdB64) },
    { name: "export-key", run: () => deleteExportKey(accountIdB64) },
    { name: "device-identity-seed", run: () => deleteDeviceIdentitySeed(accountIdB64) },
    { name: "account-identity-seed", run: () => deleteAccountIdentitySeed(accountIdB64) },
    { name: "device-group-snapshot", run: () => deleteDeviceGroupSnapshot(accountIdB64) },
    // --- localStorage, keyed by the account --------------------------------
    { name: "contacts", run: () => clearAccountContacts(accountIdB64) },
    // ADR-0073 R.2: the (account id -> key) bindings this device verified
    // itself for group members it never pinned.
    { name: "verified-bindings", run: () => clearVerifiedBindingsForAccount(accountIdB64) },
    { name: "contact-inbox", run: () => clearInboxForAccount(accountIdB64) },
    { name: "sent-requests", run: () => clearSentRequests(accountIdB64) },
    { name: "pending-publisher-requests", run: () => clearPendingPublisherRequests(accountIdB64) },
    {
      name: "invite-link-secrets",
      run: () => (urlKey === null ? undefined : clearInviteLinkSecrets(urlKey)),
    },
    { name: "readmission-removed", run: () => clearRemovedDevicesForAccount(accountIdB64) },
    { name: "signup-phrase-owed", run: () => clearSignupPhraseOwed(storage, accountIdB64) },
    // ADR-0075: the "phrase-protected" flag, the no-protection banner's
    // dismissal, the owed "Protect your account" step, and what this device
    // saw and did of the account's notices.
    { name: "protection-facts", run: () => clearProtectionFacts(storage, accountIdB64) },
    { name: "account-notices", run: () => clearNoticeStores(storage, accountIdB64) },
    // The local three-word guess budget and the ADR-0073 "publish owed"
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
    // ADR-0074: the id of the device identity wiped above (the next setup
    // mints a NEW device key; the old id is tombstoned in the directory once
    // its row was deleted), the "account key recorded at auth" mark, the
    // logout still owed to the server, and the device removals this device
    // was finishing (they need the account seed, which is gone).
    { name: "known-device-id", run: () => clearKnownDeviceId(storage, accountIdB64) },
    { name: "genesis-recorded", run: () => clearGenesisRecorded(storage, accountIdB64) },
    // UX 10.2: the passkeys this device found cannot carry keys.
    { name: "passkey-no-prf", run: () => clearNoPrfPasskeys(storage, accountIdB64) },
    { name: "logout-owed", run: () => clearLogoutOwed(storage, accountIdB64) },
    { name: "device-removals-owed", run: () => clearRemovals(storage, accountIdB64) },
    // ADR-0074 part 3: a key rotation this device was finishing, the "keys
    // were changed on another device" line, and the sealed row holding the
    // predecessor seeds, the pending envelopes, a prepared rotation and (G-56)
    // the id of the DeviceGroup this device joined by pairing.
    { name: "key-rotation-owed", run: () => clearRotation(storage, accountIdB64) },
    { name: "key-rotation-notice", run: () => clearRotatedNotice(storage, accountIdB64) },
    { name: "key-rotation-state", run: () => deleteKeyRotationState(accountIdB64) },
    // What a kept device still owes after adopting rotated keys (its own
    // links and requests, ADR-0074 section 3.5a rule 5).
    { name: "rotation-spend-owed", run: () => clearSpendOwed(accountIdB64, storage) },
    {
      name: "predecessor-mailbox-cursor",
      run: () => removeLocal(predecessorCursorStorageKey(accountIdB64)),
    },
    {
      name: "predecessor-mailbox-adopted",
      run: () => removeLocal(predecessorAdoptionStorageKey(accountIdB64)),
    },
    // docs/ux/channel-notifications.md section 5: the last post notified per
    // channel on this device.
    { name: "channel-notified", run: () => clearChannelNotified(accountIdB64, storage) },
    { name: "guest-account", run: () => clearGuestAccount(storage, accountIdB64) },
    { name: "pending-requests", run: () => clearObservationsForAccount(accountIdB64) },
    { name: "unapplied-requests", run: () => clearUnappliedForAccount(accountIdB64) },
    { name: "own-requests", run: () => clearOwnRequestsForAccount(accountIdB64) },
    { name: "committer-beacons", run: () => clearBeaconObservationsForAccount(accountIdB64) },
    { name: "last-observed-owner", run: () => clearLastObservedOwnerForAccount(accountIdB64) },
    { name: "leave-requested", run: () => clearLeaveRequestedForAccount(accountIdB64) },
    // ADR-0073 R.5: the "a lineage register is not confirmed sent" marker.
    { name: "lineage-unsent", run: () => removeLocal(`lakko.lineage.unsent.v1.${accountIdB64}`) },
    // Reactions D3 and section 12: the quick-reaction send marker and the
    // unseen-reactions list.
    { name: "quick-reaction-unsent", run: () => removeLocal(`lakko.quick-reaction.unsent.v1.${accountIdB64}`) },
    // ADR-0087 section 13a: own channel reactions and comments not yet sent
    // to the account's other devices.
    { name: "channel-feedback-unsent", run: () => removeLocal(`lakko.channel-feedback.unsent.v1.${accountIdB64}`) },
    {
      name: "channel-subscriptions-unsent",
      run: () => removeLocal(`lakko.channel-subscriptions.unsent.v1.${accountIdB64}`),
    },
    // ADR-0090 decision A: the "founded the DeviceGroup alone" marker.
    { name: "channels-founded-alone", run: () => removeLocal(`lakko.channels.founded-alone.v1.${accountIdB64}`) },
    // ADR-0090 walk follow-up: channel Welcomes held because they came before
    // this device applied its removal.
    { name: "channels-held-welcomes", run: () => removeLocal(`lakko.channels.held-welcomes.v1.${accountIdB64}`) },
    // ADR-0090 open question 4: admission seeds not yet sent to the
    // account's other devices.
    {
      name: "channel-admission-seeds-unsent",
      run: () => removeLocal(`lakko.channel-admission-seeds.unsent.v1.${accountIdB64}`),
    },
    // ADR-0092 section 3.6 — the own poll-choice register's unsent marker.
    { name: "poll-choice-unsent", run: () => removeLocal(`lakko.poll-choice.unsent.v1.${accountIdB64}`) },
    { name: "unseen-reactions", run: () => removeLocal(`lakko.unseen-reactions.v1:${accountIdB64}`) },
    // ADR-0080: the cards of the account's devices, this device's own card,
    // its city answer and its "Show my city" switch.
    { name: "device-cards", run: () => removeLocal(cardsStorageKey(accountIdB64)) },
    { name: "device-city", run: () => removeLocal(cityStorageKey(accountIdB64)) },
    { name: "device-city-shown", run: () => removeLocal(showCityStorageKey(accountIdB64)) },
    // ADR-0075: the "recovery-email register is not confirmed sent" marker
    // (the address itself is in the synced state, deleted with it), and the
    // "first password owed" marker of a guest that chose a username.
    {
      name: "recovery-email-unsent",
      run: () => removeLocal(`lakko.recovery-email.unsent.v1.${accountIdB64}`),
    },
    { name: "first-password-owed", run: () => clearFirstPasswordOwed(storage, accountIdB64) },
    // ADR-0076: the "own-username register is not confirmed sent" marker,
    // and an unfinished rename with the stamp of the last one.
    {
      name: "own-username-unsent",
      run: () => removeLocal(`lakko.own-username.unsent.v1.${accountIdB64}`),
    },
    { name: "username-change", run: () => clearRenameStores(storage, accountIdB64) },
    // Identity-recovery 17.5: the "device acknowledgement is not confirmed
    // sent" marker.
    {
      name: "device-ack-unsent",
      run: () => removeLocal(`lakko.device-ack.unsent.v1.${accountIdB64}`),
    },
    // ADR-0099 section 6: the "block register is not confirmed sent" marker.
    {
      name: "blocked-unsent",
      run: () => removeLocal(`lakko.blocked.unsent.v1.${accountIdB64}`),
    },
    // ADR-0079: the "link-record register is not confirmed sent" marker,
    // the claims of link requests (seen from the account's other devices and
    // this device's own, reworked 2026-10-17), and the ledger-row deletions
    // this device still owes.
    {
      name: "link-records-unsent",
      run: () => removeLocal(`lakko.link-records.unsent.v1.${accountIdB64}`),
    },
    { name: "link-claims", run: () => removeLocal(`lakko.link-claims.v1.${accountIdB64}`) },
    // ADR-0065 - blob deletes still owed, and when each conversation's
    // files were last kept.
    { name: "blob-deletes-owed", run: () => removeLocal(`lakko.blob-deletes-owed.v1.${accountIdB64}`) },
    { name: "blob-keeps", run: () => removeLocal(`lakko.blob-keeps.v1.${accountIdB64}`) },
    {
      name: "link-row-deletions-owed",
      run: () => removeLocal(`lakko.link-row-deletions-owed.v1.${accountIdB64}`),
    },
    // ADR-0073 R.5 / R.7: when a conversation moved, and that a re-send ran.
    { name: "reset-continuity-state", run: () => clearContinuityStateForAccount(accountIdB64) },
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
    // The conversations a restored backup named that this device has not
    // joined yet, and the "backup not consulted yet" flag (2026-10-02).
    { name: "restored-known-groups", run: () => clearRestoredKnownGroups(accountIdB64) },
    { name: "backup-restore-owed", run: () => clearBackupRestoreOwed(accountIdB64) },
    { name: "backup-restored-marker", run: () => clearRestoreMarker(accountIdB64) },
    { name: "knock-cursors", run: () => clearKnockCursorsForAccount(accountIdB64) },
    { name: "cursor-epochs", run: () => clearCursorEpochsForAccount(accountIdB64) },
    { name: "conversation-behind", run: () => conversationBehind.forgetAccount(accountIdB64) },
    { name: "reentry-rows", run: () => clearReentryRowsForAccount(accountIdB64) },
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
    // --- memory --------------------------------------------------------------
    // docs/ux/media-messages.md section 11: the composer trays (files staged
    // for the next messages, kept in memory while the app runs).
    { name: "composer-trays", run: () => dropTraysForAccount(accountIdB64) },
    // --- last: the plaintext restore marker (see the module doc) -----------
    { name: "restore-marker", run: () => clearPersistedAccountFor(accountIdB64) },
  ];
}

/**
 * What the STAGED wipe keeps (ADR-0074 section 1.1, UX 18.4): the recall
 * record, and the device root key it is sealed under. Everything else the
 * full wipe deletes, the staged wipe deletes.
 */
export const STAGED_WIPE_KEEPS: readonly string[] = ["device-key", "recall-record"];

/** The steps of the staged wipe: the full list minus {@link STAGED_WIPE_KEEPS},
 *  in the same order. */
export function stagedWipeSteps(accountIdB64: string): WipeStep[] {
  return accountWipeSteps(accountIdB64).filter((step) => !STAGED_WIPE_KEEPS.includes(step.name));
}

/**
 * Remove every piece of `accountIdB64`'s local data on this device. Never
 * throws; the report names the steps that failed (each is logged too).
 */
export function wipeAccountLocalData(accountIdB64: string): Promise<WipeReport> {
  return runWipe(accountIdB64, accountWipeSteps(accountIdB64));
}

/**
 * The staged wipe of a device whose account keys were reset elsewhere:
 * everything {@link wipeAccountLocalData} removes except the recall record
 * and its device key. The caller writes the record BEFORE this runs (the
 * account seed it is made from is among what this deletes). Never throws.
 */
export function wipeAccountStaged(accountIdB64: string): Promise<WipeReport> {
  return runWipe(accountIdB64, stagedWipeSteps(accountIdB64));
}

async function runWipe(accountIdB64: string, steps: readonly WipeStep[]): Promise<WipeReport> {
  const failed: string[] = [];
  // Seal FIRST: from here no store accepts a write of this account's data,
  // in this tab or another, so a write still suspended on an `await` cannot
  // put a row back behind a delete below. A write whose transaction already
  // exists is ordered ahead of the delete by IndexedDB itself. (The staged
  // wipe's caller wrote the recall record before this ran.)
  const ids = accountStoreIds(accountIdB64);
  sealAccountStoresForWipe(ids);
  try {
    await runWipeSteps(steps, failed);
  } finally {
    finishAccountWipe(ids);
    // The cross-tab marker names the account; it goes once every open tab
    // has heard of the wipe. A page that closes first leaves it to the next
    // page start (`installAccountStorageGate`).
    if (typeof setTimeout === "function") {
      setTimeout(() => pruneWipedMarker(), WIPED_MARKER_TTL_MS + 1_000);
    }
  }
  return { failed };
}

async function runWipeSteps(steps: readonly WipeStep[], failed: string[]): Promise<void> {
  for (const step of steps) {
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
}
