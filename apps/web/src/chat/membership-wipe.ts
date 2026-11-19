// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The one list of what a conversation's group leaves on this device
 * (ADR-0087 section 9, step 4).
 *
 * Every per-group store, by name, the way `auth/wipe-account.ts` lists every
 * per-account one: a store added later goes here, and the test enumerates the
 * names. Both ways a group goes from this device run it: the end of a
 * membership (`MembershipEndContext.tsx`) and `Delete this chat`
 * (`delete-chat.ts`).
 *
 * Not on this list, and why:
 *
 * - the profile rows: they are about people, not about a group;
 * - the synced registers (Saved, folders, flags, read cursor, draft, order):
 *   they travel between the account's devices, so they are cleared through
 *   their own tombstones ({@link syncedClearEdits} and the context);
 * - cached media: the web client keeps none (a decrypted file is an object
 *   URL in memory, gone with the view);
 * - the conversation vault: ADR-0064 is not built on this client;
 * - the keep schedule of `storage/keep-scheduler.ts`: a keep is computed from
 *   the pins and Saved that remain, so it stops by itself;
 * - the MLS group, the known-groups row and the registry entry: they go LAST,
 *   and only the caller knows when that is.
 *
 * Each step is best effort and runs whatever the others did, so one store
 * that fails costs a retry of the list and not the rest of the wipe. Every
 * step is idempotent. Called under the group's MLS lock, so a poll or a name
 * still in flight lands before the list runs and nothing refills a store
 * behind it.
 */

import { getDeviceKey } from "../auth/key-store";
import { dropRestoredKnownGroup } from "../backup/restored-known-groups";
import { clearGroupCallsForGroup } from "../calls/group-call-store";
import { dropCarriedCopies } from "../contacts/carried-copies";
import { clearAppointmentRecord } from "../ds/appointments";
import { clearRegistrarList } from "../ds/registrar-list";
import { registrarSeeds } from "../ds/registrar-seeds";
import { clearHandoverOwed, pendingRootSeeds } from "../ds/root-handover";
import { clearRootHeld } from "../ds/root-held";
import { withoutConversation } from "../feed/feed-marks";
import { updateFeedMarks } from "../feed/feed-marks-store";
import { clearMemberBaselineForGroup } from "../profile/membership-baseline";
import { clearProfileRequestForGroup } from "../profile/profile-request";
import { deleteGroupRouting } from "../push/group-routing-store";
import { dropScope } from "../reach/received-grants";
import { reachSession } from "../reach/reach-service";
import { deleteOutboxForGroup } from "./commit-outbox";
import { clearBeaconObservationsForGroup } from "./committer-beacon";
import { clearContinuityStateForGroup } from "./continuity-store";
import { clearRemovedDevicesForGroup } from "./device-readmission";
import { heldSends, persistHeldSends } from "./held-send-queue";
import { clearKnockCursorForGroup } from "./knock-cursor";
import { clearCursorEpochForGroup } from "./cursor-epoch";
import { conversationBehind } from "./conversation-behind";
import { clearReentryRowsForGroup } from "./reentry-store";
import { clearLeaveRequested } from "./membership-ended";
import { clearArmedRetries, clearOwnRequestsForGroup, retryScopeKey } from "./membership-retry";
import { deleteGroupFromMessageLog } from "./message-log-store";
import { clearLastObservedOwnerForGroup } from "./ownership-transfer";
import { forgetOwnPublished } from "./own-published";
import { pruneObservations } from "./pending-requests";
import { clearUnappliedForGroup } from "./unapplied-requests";

/** One group of a conversation. */
export interface GroupWipeTarget {
  readonly accountIdB64: string;
  /** Standard base64 of the group id, the key of every per-group store. */
  readonly groupIdB64: string;
  /** Hex of the same id: the message log's key, and a reach scope. */
  readonly groupIdHex: string;
  /** Hex of the conversation id, the key of what is about the conversation
   *  (held sends, carried copies, a feed's hidden mark). */
  readonly conversationHex: string;
}

/** The stores a context owns. */
export interface GroupWipeContexts {
  forgetPins(groupIdB64: string): Promise<void>;
  forgetGroupName(groupIdB64: string): Promise<void>;
  forgetGroupClosed(groupIdB64: string): Promise<void>;
  forgetReadWatermarks(groupIdB64: string): void;
  forgetSharedChannels(groupIdB64: string): Promise<void>;
  forgetCallLog(groupIdB64: string): void;
  /** ADR-0082 section 5: the translations and transcripts this device made
   *  of the conversation's messages. */
  forgetTranslations(groupIdB64: string): Promise<void>;
}

/** One store of the list. */
export interface GroupWipeStep {
  /** Plain ASCII, logged on failure. */
  readonly name: string;
  readonly run: () => void | Promise<void>;
}

/** Every step, in order. */
export function groupWipeSteps(target: GroupWipeTarget, contexts: GroupWipeContexts): GroupWipeStep[] {
  const { accountIdB64, groupIdB64, groupIdHex, conversationHex } = target;
  return [
    {
      name: "message-log",
      run: async () => {
        const deviceKey = await getDeviceKey(accountIdB64);
        if (deviceKey !== null) await deleteGroupFromMessageLog(accountIdB64, deviceKey, groupIdHex);
      },
    },
    {
      name: "held-sends",
      run: async () => {
        if (heldSends.take(conversationHex).length > 0) await persistHeldSends(accountIdB64);
      },
    },
    { name: "carried-copies", run: () => dropCarriedCopies(accountIdB64, conversationHex).then(() => undefined) },
    { name: "pins", run: () => contexts.forgetPins(groupIdB64) },
    { name: "group-name", run: () => contexts.forgetGroupName(groupIdB64) },
    { name: "group-closed", run: () => contexts.forgetGroupClosed(groupIdB64) },
    { name: "read-watermarks", run: () => contexts.forgetReadWatermarks(groupIdB64) },
    { name: "shared-channels", run: () => contexts.forgetSharedChannels(groupIdB64) },
    { name: "call-log", run: () => contexts.forgetCallLog(groupIdB64) },
    { name: "translations", run: () => contexts.forgetTranslations(groupIdB64) },
    { name: "group-calls", run: () => clearGroupCallsForGroup(accountIdB64, groupIdB64) },
    { name: "own-published", run: () => forgetOwnPublished(groupIdB64) },
    { name: "commit-outbox", run: () => deleteOutboxForGroup(accountIdB64, groupIdB64) },
    { name: "push-routing", run: () => deleteGroupRouting(accountIdB64, groupIdB64) },
    { name: "pending-requests", run: () => pruneObservations(accountIdB64, groupIdB64, []) },
    { name: "unapplied-requests", run: () => clearUnappliedForGroup(accountIdB64, groupIdB64) },
    { name: "own-requests", run: () => clearOwnRequestsForGroup(accountIdB64, groupIdB64) },
    { name: "armed-retries", run: () => clearArmedRetries(retryScopeKey(accountIdB64, groupIdB64)) },
    { name: "leave-requested", run: () => clearLeaveRequested(accountIdB64, groupIdB64) },
    { name: "committer-beacons", run: () => clearBeaconObservationsForGroup(accountIdB64, groupIdB64) },
    { name: "last-observed-owner", run: () => clearLastObservedOwnerForGroup(accountIdB64, groupIdB64) },
    { name: "readmission-removed", run: () => clearRemovedDevicesForGroup(accountIdB64, groupIdB64) },
    { name: "knock-cursor", run: () => clearKnockCursorForGroup(groupIdB64, accountIdB64) },
    { name: "cursor-epoch", run: () => clearCursorEpochForGroup(groupIdB64, accountIdB64) },
    // ADR-0091 phase 2 - the behind state, the re-entry record, the gap
    // lines and the knock key held for the group.
    { name: "conversation-behind", run: () => conversationBehind.forgetGroup(groupIdB64, accountIdB64) },
    { name: "reentry-rows", run: () => clearReentryRowsForGroup(groupIdB64, accountIdB64) },
    { name: "continuity", run: () => clearContinuityStateForGroup(accountIdB64, groupIdB64) },
    { name: "member-baseline", run: () => clearMemberBaselineForGroup(accountIdB64, groupIdB64) },
    { name: "profile-request", run: () => clearProfileRequestForGroup(accountIdB64, groupIdB64) },
    // The registrar seed, the root held and a handover owed exist so this
    // device can keep doing the group's journal work, which is over.
    { name: "registrar-seed", run: () => registrarSeeds.delete(groupIdB64) },
    { name: "pending-root-seed", run: () => pendingRootSeeds.delete(groupIdB64) },
    { name: "ds-appointments", run: () => clearAppointmentRecord(accountIdB64, groupIdB64) },
    { name: "ds-registrar-list", run: () => clearRegistrarList(accountIdB64, groupIdB64) },
    { name: "ds-root-handover", run: () => clearHandoverOwed(accountIdB64, groupIdB64) },
    { name: "ds-root-held", run: () => clearRootHeld(accountIdB64, groupIdB64) },
    {
      name: "reach-grants",
      run: () => reachSession()?.updateReceivedGrants((g) => dropScope(g, groupIdHex)),
    },
    {
      name: "feed-marks",
      run: () =>
        updateFeedMarks(accountIdB64, (m) => withoutConversation(m, conversationHex)).then(() => undefined),
    },
    { name: "restored-known-groups", run: () => dropRestoredKnownGroup(accountIdB64, groupIdB64) },
  ];
}

/** Run a list; the names of the steps that failed. Never rejects. */
export async function runWipeSteps(steps: readonly GroupWipeStep[]): Promise<string[]> {
  const failed: string[] = [];
  for (const step of steps) {
    try {
      await step.run();
    } catch (err) {
      failed.push(step.name);
      // eslint-disable-next-line no-console
      console.warn(`[membership-end] wipe step failed: ${step.name}`, err instanceof Error ? err.name : "");
    }
  }
  return failed;
}
