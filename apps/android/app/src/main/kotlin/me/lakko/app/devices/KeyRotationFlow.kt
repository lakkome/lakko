// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices

import android.content.Context
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import me.lakko.app.auth.ACCOUNT_KEY_REQUIRED
import me.lakko.app.auth.AuthHttpError
import me.lakko.app.auth.MarkerStorage
import me.lakko.app.auth.SharedPrefsMarkerStorage
import me.lakko.app.auth.base64UrlDecodeNoPad
import me.lakko.app.auth.base64UrlEncodeNoPad
import me.lakko.app.auth.isFreshAuthRequired
import me.lakko.app.auth.recallFinishAnswerLost

/*
 * ADR-0066 part 3, Stage A - "It was lost or stolen": remove a device AND
 * change the account keys, keeping the other devices
 * (`docs/ux/identity-recovery.md` 18.1; ADR sections 3.2 to 3.5).
 *
 * One owed operation, resumable from one record, in this order:
 *
 *   1. a fresh-auth proof; the record is written before anything is sent;
 *   2. remove the device with the ADR-0066 part 2 steps ([DeviceRemovalRunner])
 *      and WAIT until its DeviceGroup Remove commit has won the journal;
 *   3. generate the new phrase and seed; sign the supersession proof with
 *      the old key (14 days);
 *   4. announce: a `key-rotation` envelope into the DeviceGroup ONLY, sealed
 *      at the post-removal epoch ([removeThenAnnounce], the one seam);
 *   5. commit: `POST /account/keys/rotate/{start,finish}`. The point of no
 *      return;
 *   6. adopt locally (new seed and phrase; every chat, contact and message
 *      kept; the old key becomes this account's predecessor);
 *   7. publish: the directory with the proof AND the attestation, this
 *      device's KeyPackage, the DeviceGroup journal's root, the journals of
 *      owned groups, owned channels, the mailbox and reach keys, the profile
 *      re-sent to every conversation with the grants for the new mailbox
 *      (section 3.5a rule 3), the links and unanswered contact requests of
 *      the old key spent (rule 5), the backup;
 *   8. the new 12 words, withheld while the publish is owed.
 *
 * A failure before step 5 changes nothing on the server: the device WAS
 * removed and the keys were NOT changed (`devices.rotate.error.not_started`,
 * Try again / Not now). The envelope of step 4 then names a key that never
 * becomes the account's, and the kept devices drop it after 14 days. A
 * failure after step 5 resumes by itself from the record.
 *
 * The decisions are pure and every effect goes through [KeyRotationPorts],
 * so a host JVM test runs the whole machine.
 */

/** A rotation that still owes steps. No secret: the prepared entropy and
 *  proof live sealed under the device key ([me.lakko.app.auth.KeyRotationFiles]). */
@Serializable
public class KeyRotationOwed(
    /** base64url (no padding) of the removed device's 32-byte id. */
    public val deviceIdB64Url: String,
    public val startedAtMs: Long,
    /** The DeviceGroup epoch read BEFORE the removal started, when the
     *  device was still in the roster then (decimal; `null` when the removal
     *  had already happened). */
    public val epochBeforeRemoval: String? = null,
    /** The Remove commit won the journal: the device is out and the epoch
     *  advanced. */
    public val removalWon: Boolean = false,
    /** The new phrase, seed and proof exist (sealed on this device). */
    public val prepared: Boolean = false,
    /** The envelope was published into the DeviceGroup. */
    public val announced: Boolean = false,
    /** The epoch the envelope was sealed at (decimal). */
    public val announcedAtEpoch: String? = null,
    /** `rotate/finish` may have reached the server (set before it is sent). */
    public val commitSent: Boolean = false,
    /** The account IS at the new key. */
    public val committed: Boolean = false,
    /** Standard base64 of the 177-byte attestation; `null` when the commit's
     *  answer was lost and the commit was learned from the session upgrade
     *  (the publish then uses the proof alone). */
    public val attestationB64: String? = null,
    public val keyGeneration: Long? = null,
    /** The new seed and phrase replaced the old ones on this device. */
    public val adopted: Boolean = false,
    public val identityPublished: Boolean = false,
    /** Discovery refused the proof because the directory row no longer
     *  holds the key it was signed by (the removed device published a key
     *  change first): the upload is retried with the attestation alone. */
    public val publishWithoutProof: Boolean = false,
    public val keyPackagePublished: Boolean = false,
    public val deviceGroupRootMoved: Boolean = false,
    public val journalsMoved: Boolean = false,
    public val channelsMoved: Boolean = false,
    public val reachRekeyed: Boolean = false,
    public val backupSealed: Boolean = false,
    /** Section 3.5a rule 3: every conversation took the profile carrying
     *  the grants for the new mailbox. */
    public val profileResent: Boolean = false,
    /** Section 3.5a rule 5: the links the old key issued were revoked and
     *  the unanswered contact requests re-sent from the new key. */
    public val oldGrantsSpent: Boolean = false,
    /** ADR-0067 section 3: this rotation is "Save a recovery phrase". No
     *  device is removed ([deviceIdB64Url] is empty) and `rotate/finish`
     *  carries `phrase_saved: true`. */
    public val phraseSaved: Boolean = false,
) {
    /** Whether this rotation removes a device first. */
    public val removesDevice: Boolean get() = deviceIdB64Url.isNotEmpty()

    @Suppress("LongParameterList")
    public fun with(
        epochBeforeRemoval: String? = this.epochBeforeRemoval,
        removalWon: Boolean = this.removalWon,
        prepared: Boolean = this.prepared,
        announced: Boolean = this.announced,
        announcedAtEpoch: String? = this.announcedAtEpoch,
        commitSent: Boolean = this.commitSent,
        committed: Boolean = this.committed,
        attestationB64: String? = this.attestationB64,
        keyGeneration: Long? = this.keyGeneration,
        adopted: Boolean = this.adopted,
        identityPublished: Boolean = this.identityPublished,
        publishWithoutProof: Boolean = this.publishWithoutProof,
        keyPackagePublished: Boolean = this.keyPackagePublished,
        deviceGroupRootMoved: Boolean = this.deviceGroupRootMoved,
        journalsMoved: Boolean = this.journalsMoved,
        channelsMoved: Boolean = this.channelsMoved,
        reachRekeyed: Boolean = this.reachRekeyed,
        backupSealed: Boolean = this.backupSealed,
        profileResent: Boolean = this.profileResent,
        oldGrantsSpent: Boolean = this.oldGrantsSpent,
    ): KeyRotationOwed = KeyRotationOwed(
        deviceIdB64Url, startedAtMs, epochBeforeRemoval, removalWon, prepared, announced, announcedAtEpoch,
        commitSent, committed, attestationB64, keyGeneration, adopted, identityPublished, publishWithoutProof,
        keyPackagePublished, deviceGroupRootMoved, journalsMoved, channelsMoved, reachRekeyed, backupSealed,
        profileResent, oldGrantsSpent, phraseSaved,
    )
}

/** The next thing a rotation does. */
public enum class RotationStep {
    RemoveDevice,
    Prepare,
    Announce,
    Commit,
    Adopt,
    PublishIdentity,
    PublishKeyPackage,
    MoveDeviceGroupRoot,
    MoveJournals,
    MoveChannels,
    RekeyReach,
    ResendProfile,
    SpendOldGrants,
    SealBackup,
    Done,
}

/** The order of ADR-0066 section 3.2. */
public fun nextRotationStep(owed: KeyRotationOwed): RotationStep = when {
    !owed.removalWon -> RotationStep.RemoveDevice
    !owed.committed && !owed.prepared -> RotationStep.Prepare
    !owed.committed && !owed.announced -> RotationStep.Announce
    !owed.committed -> RotationStep.Commit
    !owed.adopted -> RotationStep.Adopt
    !owed.identityPublished -> RotationStep.PublishIdentity
    !owed.keyPackagePublished -> RotationStep.PublishKeyPackage
    !owed.deviceGroupRootMoved -> RotationStep.MoveDeviceGroupRoot
    !owed.journalsMoved -> RotationStep.MoveJournals
    !owed.channelsMoved -> RotationStep.MoveChannels
    !owed.reachRekeyed -> RotationStep.RekeyReach
    !owed.profileResent -> RotationStep.ResendProfile
    !owed.oldGrantsSpent -> RotationStep.SpendOldGrants
    !owed.backupSealed -> RotationStep.SealBackup
    else -> RotationStep.Done
}

/** The tail steps that hand out something for the NEW mailbox and so wait
 *  for the reach keys to be registered on it. */
public fun rotationStepNeedsReach(step: RotationStep): Boolean =
    step == RotationStep.ResendProfile || step == RotationStep.SpendOldGrants

/** The three lines of the sheet's step list (UX 18.1). */
public enum class RotationUiStep { RemovingDevice, ChangingKeys, UpdatingDevices }

public fun rotationUiStep(step: RotationStep): RotationUiStep = when (step) {
    RotationStep.RemoveDevice -> RotationUiStep.RemovingDevice
    RotationStep.Prepare, RotationStep.Announce, RotationStep.Commit, RotationStep.Adopt ->
        RotationUiStep.ChangingKeys
    else -> RotationUiStep.UpdatingDevices
}

/**
 * Whether the rotation may still be dropped ("Not now"): only while the
 * server cannot have changed the account's key. Once `rotate/finish` may
 * have been sent the record stays until the question is settled.
 */
public fun rotationCanBeAbandoned(owed: KeyRotationOwed): Boolean = !owed.commitSent && !owed.committed

/**
 * Whether the new recovery phrase is withheld (UX 18.1 "keys changed,
 * publishing still owed", the reset's rule): until the directory serves the
 * new key and the device list's journal is rooted under it, nobody can find
 * the new keys and the words must not be the only copy of anything.
 */
public fun rotationWithholdsPhrase(owed: KeyRotationOwed): Boolean =
    owed.committed && !(owed.adopted && owed.identityPublished && owed.deviceGroupRootMoved)

/** Whether a resumed rotation continues by itself (no proof, no person). */
public fun rotationResumesByItself(owed: KeyRotationOwed): Boolean = owed.commitSent || owed.committed

/**
 * The frozen 22-byte domain of the `key-rotation` envelope:
 * `lakko/key-rotation/v1` plus a trailing NUL (written as a number, never
 * as a raw NUL byte in this file).
 */
internal val KEY_ROTATION_ENVELOPE_DOMAIN: ByteArray =
    "lakko/key-rotation/v1".toByteArray(Charsets.US_ASCII) + byteArrayOf(0)

/**
 * Whether [payload] opens with the `key-rotation` domain. The background
 * filters and the conversation receive paths use it to DROP the envelope
 * (it is meaningful only in the account's own DeviceGroup, where the sync
 * runtime dispatches it through the core instead). Pure.
 */
internal fun looksLikeKeyRotationEnvelope(payload: ByteArray): Boolean {
    val domain = KEY_ROTATION_ENVELOPE_DOMAIN
    if (payload.size < domain.size) return false
    for (i in domain.indices) {
        if (payload[i] != domain[i]) return false
    }
    return true
}

/** Where the removed device stands in the account's DeviceGroup. */
public class RemovalStanding(
    /** Whether the live roster still holds the device; `null` when the
     *  roster could not be read. */
    public val inRoster: Boolean?,
    /** This device's DeviceGroup epoch. */
    public val localEpoch: ULong?,
    /** The newest epoch registered on the DS for the journal, when read. */
    public val journalEpoch: ULong?,
)

/**
 * "The stolen device is out first": the Remove commit WON when the live
 * roster no longer holds the device, this device's epoch is past the one the
 * removal started from, and the journal's head is that same epoch (so the
 * commit was not lost to a concurrent one). An unreadable roster or epoch is
 * "not yet".
 */
public fun removalHasWon(standing: RemovalStanding, epochBeforeRemoval: ULong?): Boolean {
    if (standing.inRoster != false) return false
    val local = standing.localEpoch ?: return false
    if (epochBeforeRemoval != null && local <= epochBeforeRemoval) return false
    val head = standing.journalEpoch ?: return true
    return head == local
}

/** The envelope would be readable by the removed device: nothing is sent. */
public class RotationOrderViolation(message: String) : IllegalStateException(message)

/** What the one seam needs from the DeviceGroup. */
public interface RotationAnnouncePort {
    /** Run the removal; returns once it has an answer. */
    public suspend fun removeDevice(): RemovalResult

    /** Where the removed device stands now. */
    public suspend fun standing(): RemovalStanding

    /**
     * Seal [envelope] as an application message of the DeviceGroup at its
     * CURRENT epoch and return `(wire, epoch)` WITHOUT publishing it.
     */
    public suspend fun seal(envelope: ByteArray): Pair<ByteArray, ULong>

    /** Publish a sealed message into the DeviceGroup journal, and nowhere
     *  else. */
    public suspend fun publish(wire: ByteArray)
}

/**
 * THE seam (ADR-0066 section 3.2, "T5 as built" item 5): the removal, then
 * the announcement, as one function so the two cannot be reordered
 * independently. The new seed is sealed at an epoch the removed device never
 * held; that ordering is the sender's obligation and nothing else - no
 * crate, no server - can check it.
 *
 * [removal] `null` means the removal already won on an earlier run. The
 * envelope is sealed only once [removalHasWon], and is PUBLISHED only when
 * the sealed message's own epoch is past [epochBeforeRemoval]; otherwise
 * [RotationOrderViolation] and nothing leaves the device.
 *
 * Returns the epoch the published envelope was sealed at.
 */
public suspend fun removeThenAnnounce(
    port: RotationAnnouncePort,
    epochBeforeRemoval: ULong?,
    envelope: () -> ByteArray,
    removal: (suspend () -> Unit)? = null,
): ULong {
    removal?.invoke()
    val standing = port.standing()
    if (!removalHasWon(standing, epochBeforeRemoval)) {
        throw RotationOrderViolation("the removed device is not out of the device list yet")
    }
    val (wire, sealedAt) = port.seal(envelope())
    if (epochBeforeRemoval != null && sealedAt <= epochBeforeRemoval) {
        throw RotationOrderViolation("the envelope was sealed at an epoch the removed device held")
    }
    // The roster is read once more against the epoch the message carries: a
    // handle that was reopened between the two reads must not have gone back.
    val after = port.standing()
    if (after.inRoster != false || after.localEpoch != sealedAt) {
        throw RotationOrderViolation("the device list moved while the envelope was sealed")
    }
    port.publish(wire)
    return sealedAt
}

/**
 * The announcement of a rotation that removes NO device (ADR-0067 section 3,
 * "Save a recovery phrase"): every device in the list is kept, so there is
 * no epoch the envelope must be past. Sealed at the current epoch and
 * published; returns that epoch.
 */
public suspend fun announceToKeptDevices(port: RotationAnnouncePort, envelope: () -> ByteArray): ULong {
    val (wire, sealedAt) = port.seal(envelope())
    port.publish(wire)
    return sealedAt
}

/** Why the keys were not changed (the device WAS removed). */
public sealed interface NotStartedReason {
    /** No live proof: the sheet shows the block; the same button retries. */
    public data object ProofRequired : NotStartedReason

    /** The device is signed out, but its removal from the device list has
     *  not finished (or could not be confirmed) yet. */
    public data object RemovalUnfinished : NotStartedReason

    /** `403 account-key-required`: auth holds no identity key for the
     *  account, or it is not the one this device holds. */
    public data object AccountKeyRequired : NotStartedReason

    /** `429`. */
    public class RateLimited(public val retryAfterSecs: Long?) : NotStartedReason

    /** Offline, or any other failure before the commit. */
    public class Failed(public val cause: Throwable) : NotStartedReason
}

/** The outcome of one run. */
public sealed interface RotationResult {
    /** Every step answered. */
    public data object Done : RotationResult

    /** The device was NOT signed out; nothing else was attempted. */
    public class NotRemoved(public val failure: RevokeFailure) : RotationResult

    /** The device was removed; the keys were not changed. */
    public class NotStarted(public val reason: NotStartedReason) : RotationResult

    /** The keys WERE changed; [step] is still owed and is retried by itself. */
    public class Pending(public val step: RotationStep) : RotationResult
}

/** What `rotate/{start,finish}` answered. */
public sealed interface CommitAnswer {
    /** The account is at the new key. */
    public class Committed(public val attestationB64: String, public val keyGeneration: Long) : CommitAnswer
}

/** How a failed commit is read. */
public sealed interface CommitFailure {
    /** A definite refusal: nothing changed on the server. */
    public class Refused(public val reason: NotStartedReason) : CommitFailure

    /** No answer was read: `finish` may have landed. */
    public class AnswerLost(public val cause: Throwable) : CommitFailure
}

/** Classify a failed `rotate/{start,finish}` in the server's order. */
public fun commitFailureOf(err: Throwable): CommitFailure = when {
    isFreshAuthRequired(err) -> CommitFailure.Refused(NotStartedReason.ProofRequired)
    err is AuthHttpError && err.status == ROTATE_HTTP_FORBIDDEN && err.code == ACCOUNT_KEY_REQUIRED ->
        CommitFailure.Refused(NotStartedReason.AccountKeyRequired)
    err is AuthHttpError && err.status == ROTATE_HTTP_TOO_MANY ->
        CommitFailure.Refused(NotStartedReason.RateLimited(err.retryAfterSecs))
    recallFinishAnswerLost(err) -> CommitFailure.AnswerLost(err)
    else -> CommitFailure.Refused(NotStartedReason.Failed(err))
}

/** What became of a commit whose answer was lost. */
public sealed interface LostCommit {
    /** The session upgrade signed by the NEW key was accepted at a later
     *  generation: the account is at the new key (no attestation in hand). */
    public class Committed(public val keyGeneration: Long) : LostCommit

    /** The server does not know the new key: nothing was committed. */
    public data object NotCommitted : LostCommit

    /** Still no answer. */
    public data object Unknown : LostCommit
}

/** What the directory upload did. */
public sealed interface IdentityPublish {
    public data object Landed : IdentityPublish

    /** `409 key-generation-not-newer`: already landed. */
    public data object AlreadyLanded : IdentityPublish

    /** `400 supersession-proof-invalid`: the row no longer holds the key
     *  the proof was signed by. With an attestation in hand the upload is
     *  retried WITHOUT the proof (peers then follow it as a reset). */
    public data object ProofRefused : IdentityPublish

    public class Retry(public val cause: Throwable) : IdentityPublish
}

/** The directory's codes for the rotation's upload. */
public const val DISCOVERY_SUPERSESSION_PROOF_INVALID: String = "supersession-proof-invalid"

/** Classify `POST /discovery/identity` of a rotation; `null` is a 2xx. */
public fun rotationIdentityPublishOf(error: Throwable?): IdentityPublish = when {
    error == null -> IdentityPublish.Landed
    error is me.lakko.app.discovery.DiscoveryHttpError && error.status == ROTATE_HTTP_CONFLICT &&
        error.code == me.lakko.app.auth.DISCOVERY_KEY_GENERATION_NOT_NEWER -> IdentityPublish.AlreadyLanded
    error is me.lakko.app.discovery.DiscoveryHttpError && error.status == ROTATE_HTTP_BAD_REQUEST &&
        error.code == DISCOVERY_SUPERSESSION_PROOF_INVALID -> IdentityPublish.ProofRefused
    else -> IdentityPublish.Retry(error)
}

/** What the machine needs from the device and the servers. */
public interface KeyRotationPorts : RotationAnnouncePort {
    /** The live fresh-auth grant, or `null`. */
    public fun freshGrant(): String?

    /** The DeviceGroup epoch now, and whether the device is in the roster
     *  (read before the removal starts). */
    public suspend fun epochIfInRoster(): ULong?

    /** Generate the new phrase and seed, sign the proof with the old key,
     *  and keep them sealed on this device. Idempotent while they exist. */
    public fun prepare(nowMs: Long)

    /** The `key-rotation` envelope of the prepared material. */
    public fun envelope(): ByteArray

    /** `rotate/start` then `rotate/finish`; persists the returned session
     *  pair before it returns. Throws on anything but success. */
    public suspend fun commit(
        freshGrant: String,
        /** `null` for a rotation that removes no device. */
        revokeDeviceId: ByteArray?,
        /** ADR-0067 section 3: "Save a recovery phrase". */
        phraseSaved: Boolean,
    ): CommitAnswer.Committed

    /** Ask whether a commit whose answer was lost landed. */
    public suspend fun resolveLostCommit(): LostCommit

    /** The new seed and phrase replace the old ones; the old identity is
     *  kept as this account's predecessor; nothing else local changes. */
    public suspend fun adoptLocally(attestationB64: String?, keyGeneration: Long?)

    public suspend fun publishIdentity(attestationB64: String?, withProof: Boolean): IdentityPublish

    public suspend fun publishKeyPackage(): Boolean

    public suspend fun moveDeviceGroupRoot(attestationB64: String?, keyGeneration: Long?): Boolean

    public suspend fun moveOwnedJournals(): Boolean

    public suspend fun moveOwnedChannels(): Boolean

    public suspend fun rekeyReach(): Boolean

    /**
     * Section 3.5a rule 3: re-send the profile, with the grants for the new
     * mailbox, to every one-to-one and group. `true` only once every
     * conversation took it; `false` or a throw keeps the step owed.
     */
    public suspend fun resendProfile(): Boolean

    /**
     * Section 3.5a rule 5: revoke the `/i/` and `/c/` links the old key
     * issued and re-send every unanswered outgoing contact request from the
     * new key. `true` only once nothing is left; `false` or a throw keeps
     * the step owed.
     */
    public suspend fun spendOldGrants(): Boolean

    public suspend fun sealBackup(): Boolean

    /** The publish landed far enough for the words to be shown. */
    public fun onPhraseReleased()

    /** Drop the prepared material (an abandoned or finished rotation). */
    public fun discardPrepared()

    public suspend fun pause(ms: Long)
}

/** The owed rotation of each account on this device (at most one). */
public class KeyRotationOwedStore(private val storage: MarkerStorage) {
    public fun load(accountIdB64: String): KeyRotationOwed? {
        val raw = try {
            storage.get(KEY_PREFIX + accountIdB64)
        } catch (_: Throwable) {
            null
        } ?: return null
        return try {
            json.decodeFromString(KeyRotationOwed.serializer(), raw)
        } catch (_: Throwable) {
            null
        }
    }

    public fun save(accountIdB64: String, owed: KeyRotationOwed) {
        try {
            storage.set(KEY_PREFIX + accountIdB64, json.encodeToString(KeyRotationOwed.serializer(), owed))
        } catch (_: Throwable) {
            // The debt then lives for this process only.
        }
    }

    public fun clear(accountIdB64: String) {
        try {
            storage.remove(KEY_PREFIX + accountIdB64)
        } catch (_: Throwable) {
            // Nothing to drop.
        }
    }

    public companion object {
        public const val PREFS_NAME: String = "lakko-key-rotation-owed"
        private const val KEY_PREFIX: String = "key-rotation-v1:"
        private val json = Json { ignoreUnknownKeys = true }

        public fun onDevice(context: Context): KeyRotationOwedStore =
            KeyRotationOwedStore(SharedPrefsMarkerStorage(context, PREFS_NAME))
    }
}

/**
 * The rotation machine for one account. [rotate] is the sheet's button and
 * its `Try again`; [resume] runs at every landing; [abandon] is `Not now`.
 */
public class KeyRotationRunner(
    private val accountIdB64: String,
    private val store: KeyRotationOwedStore,
    private val ports: KeyRotationPorts,
    private val clock: () -> Long = { System.currentTimeMillis() },
    private val log: (String) -> Unit = {},
    /** Told the line of the step list that is active (UX 18.1). */
    private val onStep: (RotationUiStep) -> Unit = {},
) {
    /** The rotation this device still owes, or `null`. */
    public fun owed(): KeyRotationOwed? = store.load(accountIdB64)

    /**
     * Remove [deviceId] and change the account keys. Also `Try again` for a
     * rotation that did not start: the run continues at the first step
     * still owed.
     */
    public suspend fun rotate(deviceId: ByteArray): RotationResult = lock.withLock {
        val id = base64UrlEncodeNoPad(deviceId)
        var owed = store.load(accountIdB64)?.takeIf { it.deviceIdB64Url == id }
        if (owed == null) {
            // A rotation owed for ANOTHER device that passed its commit must
            // finish first; one that did not is replaced.
            store.load(accountIdB64)?.let { other ->
                if (!rotationCanBeAbandoned(other)) return@withLock advance(other)
                ports.discardPrepared()
            }
            if (ports.freshGrant() == null) {
                return@withLock RotationResult.NotStarted(NotStartedReason.ProofRequired)
            }
            val before = try {
                ports.epochIfInRoster()
            } catch (cancel: CancellationException) {
                throw cancel
            } catch (_: Throwable) {
                null
            }
            owed = KeyRotationOwed(
                deviceIdB64Url = id,
                startedAtMs = clock(),
                epochBeforeRemoval = before?.toString(),
            )
            // Before anything is sent.
            store.save(accountIdB64, owed)
        }
        advance(owed)
    }

    /**
     * ADR-0067 section 3 / UX 19.5 - "Save a recovery phrase": change the
     * account keys, removing no device, and mark the account
     * phrase-protected in the same server step. Also its `Try again`. A
     * rotation already owed for a device that passed its commit finishes
     * first; one that did not is replaced.
     */
    public suspend fun savePhrase(): RotationResult = lock.withLock {
        var owed = store.load(accountIdB64)?.takeIf { it.phraseSaved && !it.removesDevice }
        if (owed == null) {
            store.load(accountIdB64)?.let { other ->
                if (!rotationCanBeAbandoned(other)) return@withLock advance(other)
                ports.discardPrepared()
            }
            if (ports.freshGrant() == null) {
                return@withLock RotationResult.NotStarted(NotStartedReason.ProofRequired)
            }
            owed = KeyRotationOwed(
                deviceIdB64Url = "",
                startedAtMs = clock(),
                removalWon = true,
                phraseSaved = true,
            )
            // Before anything is sent.
            store.save(accountIdB64, owed)
        }
        advance(owed)
    }

    /** Continue an owed rotation that passed its commit. One that did not
     *  waits for the person (it needs a proof). */
    public suspend fun resume(): RotationResult? = lock.withLock {
        val owed = store.load(accountIdB64) ?: return@withLock null
        if (!rotationResumesByItself(owed)) return@withLock null
        advance(owed)
    }

    /** `Not now`: drop a rotation that did not start. `false` when the
     *  commit may have been sent (it then finishes by itself). */
    public suspend fun abandon(): Boolean = lock.withLock {
        val owed = store.load(accountIdB64) ?: return@withLock true
        if (!rotationCanBeAbandoned(owed)) return@withLock false
        store.clear(accountIdB64)
        ports.discardPrepared()
        true
    }

    @Suppress("CyclomaticComplexMethod", "LongMethod", "ReturnCount")
    private suspend fun advance(start: KeyRotationOwed): RotationResult {
        var owed = start
        val deviceId: ByteArray? = try {
            if (owed.removesDevice) base64UrlDecodeNoPad(owed.deviceIdB64Url) else null
        } catch (_: IllegalArgumentException) {
            store.clear(accountIdB64)
            return RotationResult.NotStarted(NotStartedReason.Failed(IllegalStateException("bad device id")))
        }
        fun save(next: KeyRotationOwed): KeyRotationOwed {
            store.save(accountIdB64, next)
            return next
        }
        val epochBefore = owed.epochBeforeRemoval?.toULongOrNull()
        while (true) {
            val step = nextRotationStep(owed)
            onStep(rotationUiStep(step))
            when (step) {
                RotationStep.RemoveDevice -> {
                    val removal = try {
                        ports.removeDevice()
                    } catch (cancel: CancellationException) {
                        throw cancel
                    } catch (cause: Throwable) {
                        return RotationResult.NotStarted(NotStartedReason.Failed(cause))
                    }
                    when (removal) {
                        is RemovalResult.NotRevoked -> {
                            // Nothing was removed and nothing is owed.
                            store.clear(accountIdB64)
                            return RotationResult.NotRemoved(removal.failure)
                        }
                        is RemovalResult.Pending ->
                            if (removal.step != RemovalStep.RetireChats) {
                                return RotationResult.NotStarted(NotStartedReason.RemovalUnfinished)
                            }
                        RemovalResult.Done -> Unit
                    }
                    if (!awaitRemovalWon(epochBefore)) {
                        return RotationResult.NotStarted(NotStartedReason.RemovalUnfinished)
                    }
                    owed = save(owed.with(removalWon = true))
                }
                RotationStep.Prepare -> {
                    try {
                        ports.prepare(clock())
                    } catch (cause: Throwable) {
                        return RotationResult.NotStarted(NotStartedReason.Failed(cause))
                    }
                    owed = save(owed.with(prepared = true))
                }
                RotationStep.Announce -> {
                    val sealedAt = try {
                        if (owed.removesDevice) {
                            removeThenAnnounce(ports, epochBefore, envelope = { ports.envelope() })
                        } else {
                            announceToKeptDevices(ports, envelope = { ports.envelope() })
                        }
                    } catch (cancel: CancellationException) {
                        throw cancel
                    } catch (cause: RotationOrderViolation) {
                        log("key rotation: the envelope was not sent (${cause.message})")
                        return RotationResult.NotStarted(NotStartedReason.RemovalUnfinished)
                    } catch (cause: Throwable) {
                        return RotationResult.NotStarted(NotStartedReason.Failed(cause))
                    }
                    owed = save(owed.with(announced = true, announcedAtEpoch = sealedAt.toString()))
                }
                RotationStep.Commit -> {
                    if (owed.commitSent) {
                        // An earlier `finish` had no answer: settle it first.
                        when (val lost = resolveLost()) {
                            is LostCommit.Committed ->
                                owed = save(owed.with(committed = true, keyGeneration = lost.keyGeneration))
                            LostCommit.NotCommitted -> owed = save(owed.with(commitSent = false))
                            LostCommit.Unknown -> return RotationResult.Pending(step)
                        }
                        continue
                    }
                    val grant = ports.freshGrant()
                        ?: return RotationResult.NotStarted(NotStartedReason.ProofRequired)
                    // Before `finish` can reach the server.
                    owed = save(owed.with(commitSent = true))
                    val answer = try {
                        ports.commit(grant, deviceId, owed.phraseSaved)
                    } catch (cancel: CancellationException) {
                        throw cancel
                    } catch (cause: Throwable) {
                        when (val failure = commitFailureOf(cause)) {
                            is CommitFailure.Refused -> {
                                owed = save(owed.with(commitSent = false))
                                return RotationResult.NotStarted(failure.reason)
                            }
                            is CommitFailure.AnswerLost -> {
                                log("key rotation: the commit had no answer; asking whether it landed")
                                continue
                            }
                        }
                    }
                    owed = save(
                        owed.with(
                            committed = true,
                            attestationB64 = answer.attestationB64,
                            keyGeneration = answer.keyGeneration,
                        ),
                    )
                }
                RotationStep.Adopt -> {
                    try {
                        ports.adoptLocally(owed.attestationB64, owed.keyGeneration)
                    } catch (cancel: CancellationException) {
                        throw cancel
                    } catch (cause: Throwable) {
                        log("key rotation: the new keys were not stored yet (${cause.javaClass.simpleName})")
                        return RotationResult.Pending(step)
                    }
                    owed = save(owed.with(adopted = true))
                }
                RotationStep.PublishIdentity -> {
                    val withProof = !owed.publishWithoutProof
                    val outcome = try {
                        ports.publishIdentity(owed.attestationB64, withProof)
                    } catch (cancel: CancellationException) {
                        throw cancel
                    } catch (cause: Throwable) {
                        IdentityPublish.Retry(cause)
                    }
                    when (outcome) {
                        IdentityPublish.Landed, IdentityPublish.AlreadyLanded ->
                            owed = save(owed.with(identityPublished = true))
                        IdentityPublish.ProofRefused -> {
                            // The fallback needs the attestation: without it
                            // (a lost commit answer) nothing can be uploaded.
                            if (!withProof || owed.attestationB64 == null) return RotationResult.Pending(step)
                            owed = save(owed.with(publishWithoutProof = true))
                        }
                        is IdentityPublish.Retry -> {
                            log("key rotation: the directory upload did not land (${outcome.cause.javaClass.simpleName})")
                            return RotationResult.Pending(step)
                        }
                    }
                }
                // From here on the steps do not depend on each other: one
                // that fails is retried later and the rest still run.
                else -> return runTail(owed)
            }
        }
    }

    /** The publishing steps after the directory upload, each attempted once
     *  per run; only a step that answered is recorded. */
    private suspend fun runTail(start: KeyRotationOwed): RotationResult {
        var owed = start
        var firstPending: RotationStep? = null
        for (step in TAIL) {
            val done = when (step) {
                RotationStep.PublishKeyPackage -> owed.keyPackagePublished
                RotationStep.MoveDeviceGroupRoot -> owed.deviceGroupRootMoved
                RotationStep.MoveJournals -> owed.journalsMoved
                RotationStep.MoveChannels -> owed.channelsMoved
                RotationStep.RekeyReach -> owed.reachRekeyed
                RotationStep.ResendProfile -> owed.profileResent
                RotationStep.SpendOldGrants -> owed.oldGrantsSpent
                else -> owed.backupSealed
            }
            if (done) continue
            // The one dependency in the tail: the profile carries grants for
            // the new mailbox, and a re-sent contact request a reply grant
            // for it; both exist only once the reach keys moved.
            if (rotationStepNeedsReach(step) && !owed.reachRekeyed) {
                if (firstPending == null) firstPending = step
                continue
            }
            onStep(rotationUiStep(step))
            val attestation = owed.attestationB64
            val generation = owed.keyGeneration
            val ok = attempt(step) {
                when (step) {
                    RotationStep.PublishKeyPackage -> ports.publishKeyPackage()
                    RotationStep.MoveDeviceGroupRoot -> ports.moveDeviceGroupRoot(attestation, generation)
                    RotationStep.MoveJournals -> ports.moveOwnedJournals()
                    RotationStep.MoveChannels -> ports.moveOwnedChannels()
                    RotationStep.RekeyReach -> ports.rekeyReach()
                    RotationStep.ResendProfile -> ports.resendProfile()
                    RotationStep.SpendOldGrants -> ports.spendOldGrants()
                    else -> ports.sealBackup()
                }
            }
            if (!ok) {
                if (firstPending == null) firstPending = step
                continue
            }
            owed = when (step) {
                RotationStep.PublishKeyPackage -> owed.with(keyPackagePublished = true)
                RotationStep.MoveDeviceGroupRoot -> owed.with(deviceGroupRootMoved = true)
                RotationStep.MoveJournals -> owed.with(journalsMoved = true)
                RotationStep.MoveChannels -> owed.with(channelsMoved = true)
                RotationStep.RekeyReach -> owed.with(reachRekeyed = true)
                RotationStep.ResendProfile -> owed.with(profileResent = true)
                RotationStep.SpendOldGrants -> owed.with(oldGrantsSpent = true)
                else -> owed.with(backupSealed = true)
            }
            store.save(accountIdB64, owed)
            if (step == RotationStep.MoveDeviceGroupRoot && !rotationWithholdsPhrase(owed)) ports.onPhraseReleased()
        }
        if (!rotationWithholdsPhrase(owed)) ports.onPhraseReleased()
        firstPending?.let { return RotationResult.Pending(it) }
        store.clear(accountIdB64)
        ports.discardPrepared()
        return RotationResult.Done
    }

    private suspend fun attempt(step: RotationStep, run: suspend () -> Boolean): Boolean =
        try {
            run()
        } catch (cancel: CancellationException) {
            throw cancel
        } catch (cause: Throwable) {
            log("key rotation: $step not done yet (${cause.javaClass.simpleName})")
            false
        }

    private suspend fun resolveLost(): LostCommit =
        try {
            ports.resolveLostCommit()
        } catch (cancel: CancellationException) {
            throw cancel
        } catch (_: Throwable) {
            LostCommit.Unknown
        }

    /** Poll the standing a few times: the removal's publish has answered,
     *  but the live handle is reopened asynchronously. */
    private suspend fun awaitRemovalWon(epochBefore: ULong?): Boolean {
        repeat(REMOVAL_WAIT_TRIES) { attemptNo ->
            val standing = try {
                ports.standing()
            } catch (cancel: CancellationException) {
                throw cancel
            } catch (_: Throwable) {
                null
            }
            if (standing != null && removalHasWon(standing, epochBefore)) return true
            if (attemptNo < REMOVAL_WAIT_TRIES - 1) ports.pause(REMOVAL_WAIT_STEP_MS)
        }
        return false
    }

    public companion object {
        /** One rotation run at a time in this process. */
        private val lock = Mutex()

        /** How long a run waits for the Remove commit to be seen as won
         *  (chosen, not measured). */
        public const val REMOVAL_WAIT_TRIES: Int = 10
        public const val REMOVAL_WAIT_STEP_MS: Long = 1_000L

        private val TAIL: List<RotationStep> = listOf(
            RotationStep.PublishKeyPackage,
            RotationStep.MoveDeviceGroupRoot,
            RotationStep.MoveJournals,
            RotationStep.MoveChannels,
            RotationStep.RekeyReach,
            RotationStep.ResendProfile,
            RotationStep.SpendOldGrants,
            RotationStep.SealBackup,
        )
    }
}

private const val ROTATE_HTTP_BAD_REQUEST: Int = 400
private const val ROTATE_HTTP_CONFLICT: Int = 409
private const val ROTATE_HTTP_FORBIDDEN: Int = 403
private const val ROTATE_HTTP_TOO_MANY: Int = 429

/** What the "Save a recovery phrase" screen does after one run (UX 19.5). */
public sealed interface SavePhraseOutcome {
    /** The key change went through: show the NEW words; [withheld] while
     *  the publish of the new keys is still owed. */
    public data class ShowWords(val withheld: Boolean) : SavePhraseOutcome

    /** The proof ran out: the block appears again, no error. */
    public data object ShowProofBlock : SavePhraseOutcome

    /** "We could not finish. Nothing changed." with `Try again`;
     *  [retryAfterSecs] when the server stated a wait. */
    public data class Failed(val retryAfterSecs: Long?) : SavePhraseOutcome

    /** `rotate/finish` was sent and its answer is not known yet: nothing is
     *  shown (the words on this device may still be the old ones); the run
     *  settles by itself and `Try again` asks again. */
    public data object Unsettled : SavePhraseOutcome
}

/**
 * The pure mapping of a save-phrase run. The new words are shown ONLY once
 * the server step succeeded ([owed] is committed, or the run is done); a run
 * that stopped before that changed nothing.
 */
public fun savePhraseOutcome(result: RotationResult, owed: KeyRotationOwed?): SavePhraseOutcome = when (result) {
    RotationResult.Done -> SavePhraseOutcome.ShowWords(withheld = false)
    is RotationResult.Pending -> when {
        owed == null -> SavePhraseOutcome.ShowWords(withheld = false)
        owed.committed -> SavePhraseOutcome.ShowWords(withheld = rotationWithholdsPhrase(owed))
        else -> SavePhraseOutcome.Unsettled
    }
    is RotationResult.NotRemoved -> SavePhraseOutcome.Failed(null)
    is RotationResult.NotStarted -> when (val reason = result.reason) {
        NotStartedReason.ProofRequired -> SavePhraseOutcome.ShowProofBlock
        is NotStartedReason.RateLimited -> SavePhraseOutcome.Failed(reason.retryAfterSecs)
        else -> SavePhraseOutcome.Failed(null)
    }
}
