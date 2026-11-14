// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

/**
 * ADR-0090 - the pure decisions of a channel publisher pass on a device of an
 * account that publishes, kept apart from the network and the native core so
 * the host suite runs them.
 */

/** What one account's devices in a PublisherGroup need (section 2.1 / 2.2):
 *  devices of its DeviceGroup with no leaf, and leaves whose device left. */
public data class DeviceReconcilePlan(
    /** Signature keys of devices to admit (in the roster, no leaf). */
    val admit: List<ByteArray>,
    /** Leaf keys to retire (a leaf of the account, its device gone). */
    val retire: List<ByteArray>,
) {
    public val isEmpty: Boolean get() = admit.isEmpty() && retire.isEmpty()
}

/**
 * Section 2.1 - compare one account's leaves in the PublisherGroup
 * ([accountLeaves], their signature keys) with the account's DeviceGroup
 * [roster] (the authority on which devices belong to it). A roster device
 * with no leaf is admitted; a leaf whose device is not in the roster is
 * retired. [self] (this device) is never retired here: OpenMLS cannot remove
 * its own leaf, another device of the account does.
 */
public fun planDeviceReconcile(
    accountLeaves: List<ByteArray>,
    roster: List<ByteArray>,
    self: ByteArray,
): DeviceReconcilePlan {
    val admit = roster.filter { r -> accountLeaves.none { it.contentEquals(r) } }
        .distinctBy { it.toList() }
    val retire = accountLeaves.filter { l -> !l.contentEquals(self) && roster.none { it.contentEquals(l) } }
        .distinctBy { it.toList() }
    return DeviceReconcilePlan(admit, retire)
}

/**
 * Section 2.1 - the KeyPackage to admit for the roster device [deviceKey], of
 * the [published] ones (device id, KeyPackage): only one the directory lists
 * under that device id, whose leaf key IS that device key and whose binding
 * names [accountKey]. A device is never added on the strength of a KeyPackage
 * alone; [leafKeyOf] and [accountOf] are the core's readers.
 */
public fun keyPackageToAdmit(
    deviceKey: ByteArray,
    accountKey: ByteArray,
    published: List<Pair<ByteArray, ByteArray>>,
    leafKeyOf: (ByteArray) -> ByteArray?,
    accountOf: (ByteArray) -> ByteArray?,
): ByteArray? {
    val kp = published.firstOrNull { it.first.contentEquals(deviceKey) }?.second ?: return null
    val leaf = runCatching { leafKeyOf(kp) }.getOrNull() ?: return null
    if (!leaf.contentEquals(deviceKey)) return null
    val account = runCatching { accountOf(kp) }.getOrNull() ?: return null
    return if (account.contentEquals(accountKey)) kp else null
}

/** What an owner device does with a publisher's device request (2.2). */
public enum class DeviceRequestVerdict { ADMIT, RETIRE, IGNORE }

/**
 * Section 2.2 / decision B - whether an owner device acts on a device request
 * it read on [channelId]'s PublisherGroup: only for this channel; only from a
 * current publisher account ([sender] resolved at receipt, in
 * [publisherAccounts]); an admit only from a co-publisher (the owner's own
 * devices come from the DeviceGroup roster), for a KeyPackage bound to the
 * sender ([kpAccount]); a retire only of a leaf of the sender's account
 * ([leafAccount], `null` when no such leaf).
 */
public fun deviceRequestVerdict(
    isOwnerDevice: Boolean,
    requestChannelId: ByteArray,
    channelId: ByteArray,
    sender: ByteArray?,
    ownerAccountKey: ByteArray,
    publisherAccounts: List<ByteArray>,
    admit: Boolean,
    kpAccount: ByteArray?,
    leafAccount: ByteArray?,
): DeviceRequestVerdict {
    if (!isOwnerDevice || sender == null || !requestChannelId.contentEquals(channelId)) return DeviceRequestVerdict.IGNORE
    if (publisherAccounts.none { it.contentEquals(sender) }) return DeviceRequestVerdict.IGNORE
    return if (admit) {
        if (sender.contentEquals(ownerAccountKey)) return DeviceRequestVerdict.IGNORE
        if (kpAccount != null && kpAccount.contentEquals(sender)) DeviceRequestVerdict.ADMIT else DeviceRequestVerdict.IGNORE
    } else {
        if (leafAccount != null && leafAccount.contentEquals(sender)) DeviceRequestVerdict.RETIRE else DeviceRequestVerdict.IGNORE
    }
}

/** What a received (or still pending) admission mark changes (section 6). */
public data class AdmissionMarkVerdict(
    /** The mark this device moves to now, or `null`. */
    val apply: Long?,
    /** The part above the rows this device has read, kept until it has read
     *  that far, or `null` (nothing pending). */
    val pending: Long?,
)

/**
 * Section 6, security pass - an admission mark read from the PublisherGroup
 * counts only from a device of the channel OWNER's account ([ownerKey]; the
 * sender resolved at receipt), never from a co-publisher; it is kept as the
 * maximum, but moves this device's mark only up to the newest subscriber row
 * this device has itself read from the channels service ([seenThrough]). The
 * rest stays pending ([pendingHeld], the part held from earlier) until this
 * device has read that far.
 */
public fun admissionMarkVerdict(
    held: Long,
    received: Long,
    sender: ByteArray?,
    ownerKey: ByteArray,
    seenThrough: Long,
    pendingHeld: Long,
): AdmissionMarkVerdict {
    if (sender == null || !sender.contentEquals(ownerKey)) return AdmissionMarkVerdict(null, null)
    return pendingAdmissionMark(held, maxOf(received, pendingHeld), seenThrough)
}

/** A mark [target] (received or pending) against what this device has read:
 *  applied up to [seenThrough], the rest pending. */
public fun pendingAdmissionMark(held: Long, target: Long, seenThrough: Long): AdmissionMarkVerdict {
    val reachable = minOf(target, seenThrough)
    return AdmissionMarkVerdict(
        apply = reachable.takeIf { it > held },
        pending = target.takeIf { it > seenThrough && it > held },
    )
}

/** Section 4 - the random wait (0 to 60 s, chosen, not measured) before an
 *  owner device stages a due rotation. */
public const val ROTATION_JITTER_MAX_MS: Long = 60_000L

/** Section 4 - how long a group epoch whose seed the channel service has not
 *  taken waits before an owner device reseals it itself (chosen). */
public const val UNFINISHED_RESEAL_WAIT_MS: Long = 5L * 60L * 1_000L

/**
 * Section 4 - the unfinished-reseal takeover: the channel service's epoch
 * ([serviceEpoch]) trails the group's ([groupEpoch]) and has done so since
 * [trailingSinceMs] for at least [UNFINISHED_RESEAL_WAIT_MS], and this device
 * has no recorded commit of its own still running for the channel.
 */
public fun resealTakeoverDue(
    groupEpoch: Long,
    serviceEpoch: Long,
    trailingSinceMs: Long?,
    nowMs: Long,
    ownCommitPending: Boolean,
): Boolean =
    !ownCommitPending && serviceEpoch < groupEpoch && trailingSinceMs != null &&
        nowMs - trailingSinceMs >= UNFINISHED_RESEAL_WAIT_MS

/** Section 2.5 - consecutive refused signed reads of a PublisherGroup journal
 *  after which this device reads its journal as reset under it (a re-founding
 *  on another device). Chosen, not measured; one refusal can be a lagging
 *  registration. */
public const val RESET_REFUSALS: Int = 3

/** Section 2.5 - whether the journal was reset under this device: enough
 *  refused reads in a row, or the DS names a newest epoch below this
 *  device's own (a chain it does not hold). */
public fun journalResetUnder(consecutiveRefusals: Int, latestEpoch: ULong?, ownEpoch: ULong): Boolean =
    consecutiveRefusals >= RESET_REFUSALS || (latestEpoch != null && latestEpoch < ownEpoch)

/** Section 2.2 - how often a co-publisher's device sends the same device
 *  request again while not acted on (a delegation lives 24 h). Chosen. */
public const val DEVICE_REQUEST_RESEND_MS: Long = 30L * 60L * 1_000L

/** Section 4 - the post count of the rotation policy: the posts listed under
 *  the current channel epoch ([epochs], one per post), at least the count
 *  this device kept itself. */
public fun postsUnderEpoch(epochs: List<UInt?>, current: UInt, kept: UInt): UInt =
    maxOf(kept, epochs.count { it == current }.toUInt())

/** Decision A - a device founds the account's owned channels again only
 *  while the account's DeviceGroup roster is exactly this device. */
public fun refoundAllowed(roster: List<ByteArray>, self: ByteArray): Boolean =
    roster.size == 1 && roster[0].contentEquals(self)

/**
 * ADR-0090 section 2.3 - the owner account key a co-publisher's device takes
 * a PublisherGroup Welcome under: the one the account's register records
 * ([registerKey], written by the device that joined from the `/g/`
 * capability's verified issuer), else the one this device recorded itself
 * ([recordedKey]); `null` (do not join) when neither holds 32 bytes. Never a
 * key read off the Welcome.
 */
public fun coPublisherWelcomeOwnerKey(registerKey: ByteArray?, recordedKey: ByteArray?): ByteArray? =
    registerKey?.takeIf { it.size == 32 } ?: recordedKey?.takeIf { it.size == 32 }

/** Decision A - whom a re-founding device reseals the new seed to. */
public sealed class RefoundReseal {
    /** Every row: a channel that was never private. */
    public object EveryRow : RefoundReseal()

    /** Exactly these subscriber keys (lower-case hex). */
    public data class Keys(val keys: List<String>) : RefoundReseal()
}

/**
 * Decision A, security pass - the reseal of a re-founded channel: the rows a
 * ticket this device can open lets in ([admitted], from
 * `ChannelSubscriberRows.admissible`) plus the account's own reader key
 * ([ownKeys]); with no admission seed here ([admitted] `null`) a private
 * channel reseals only to the account's own reader key (the admission pass
 * lets the others in later) and only a public one, never private, reseals to
 * every row.
 */
public fun refoundResealPlan(admitted: List<String>?, public: Boolean, ownKeys: Set<String>): RefoundReseal = when {
    admitted != null -> RefoundReseal.Keys((admitted + ownKeys).map { it.lowercase() }.distinct())
    public -> RefoundReseal.EveryRow
    else -> RefoundReseal.Keys(ownKeys.map { it.lowercase() }.distinct())
}
