// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices

import android.content.Context
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.serialization.Serializable
import kotlinx.serialization.builtins.ListSerializer
import kotlinx.serialization.json.Json
import me.lakko.app.auth.ACCOUNT_KEY_REQUIRED
import me.lakko.app.auth.AuthClient
import me.lakko.app.auth.AuthHttpError
import me.lakko.app.auth.AuthNetworkError
import me.lakko.app.auth.AuthRepository
import me.lakko.app.auth.MarkerStorage
import me.lakko.app.auth.SharedPrefsMarkerStorage
import me.lakko.app.auth.base64UrlDecodeNoPad
import me.lakko.app.auth.base64UrlEncodeNoPad
import me.lakko.app.auth.isFreshAuthRequired
import me.lakko.app.discovery.DiscoveryClient
import uniffi.lakko_ffi_uniffi.Identity
import uniffi.lakko_ffi_uniffi.accountStatementHeaderName
import uniffi.lakko_ffi_uniffi.signDeviceRevokeStatement

/*
 * ADR-0074 section 2.3 — removing ANOTHER device, as one operation.
 *
 * The order, from one owed record that is retried at every landing until
 * each step has an answer:
 *
 *   1. a fresh-auth proof (the sheet's proof block);
 *   2. `POST /account/devices/revoke`: the device is signed out at auth for
 *      good. If this fails, NOTHING else has happened and the sheet says so;
 *   3. `DELETE /discovery/keypackage/device/{id}` with the same account
 *      statement: this is what closes the directory, so it comes before the
 *      groups (a peer that fetched the row meanwhile only produced a Welcome
 *      nobody will open);
 *   4. the DeviceGroup Remove commit (with the ADR-0015 attestation when the
 *      account's policy requires one);
 *   5. the removed device's leaf out of every chat.
 *
 * The record is written BEFORE the revoke is sent, so a crash between the
 * request and its answer never leaves a device revoked at auth and still in
 * the groups with nothing to retry it. The account statement is signed right
 * before it is sent and never stored (it is valid for two minutes).
 *
 * The decisions are pure and the steps go through [DeviceRemovalPorts], so a
 * host JVM test runs the whole machine without a server or the native core.
 */

/** A removal that still owes steps, per removed device. */
@Serializable
public class DeviceRemovalOwed(
    /** base64url (no padding) of the removed device's 32-byte id. */
    public val deviceIdB64Url: String,
    /** When the removal was started on this device (unix ms). */
    public val startedAtMs: Long,
    /** `POST /account/devices/revoke` answered `204`. */
    public val revoked: Boolean = false,
    /** The directory row was deleted (`204`). */
    public val directoryDeleted: Boolean = false,
    /** The DeviceGroup no longer holds the device's leaf. */
    public val groupRemoved: Boolean = false,
    /** Every chat this device is in answered the leaf removal. */
    public val chatsRetired: Boolean = false,
    /** The ADR-0015 removal attestation (a public signed artifact, one hour
     *  of life), base64url, when the account's policy needed one. */
    public val attestationB64Url: String? = null,
    /** When that attestation was obtained (unix ms). */
    public val attestationAtMs: Long? = null,
    /** The DeviceGroup step answered that the account's policy requires an
     *  attestation and none usable was held: only the person's passkey
     *  gesture can finish the removal ([removalWaitsOnPerson]). */
    public val needsAttestation: Boolean = false,
) {
    public fun with(
        revoked: Boolean = this.revoked,
        directoryDeleted: Boolean = this.directoryDeleted,
        groupRemoved: Boolean = this.groupRemoved,
        chatsRetired: Boolean = this.chatsRetired,
        attestationB64Url: String? = this.attestationB64Url,
        attestationAtMs: Long? = this.attestationAtMs,
        needsAttestation: Boolean = this.needsAttestation,
    ): DeviceRemovalOwed = DeviceRemovalOwed(
        deviceIdB64Url, startedAtMs, revoked, directoryDeleted, groupRemoved, chatsRetired,
        attestationB64Url, attestationAtMs, needsAttestation,
    )
}

/** The next thing a removal does. */
public enum class RemovalStep { Revoke, DirectoryDelete, DeviceGroupRemove, RetireChats, Done }

/** Auth first; then the directory; then the device list; then the chats. */
public fun nextRemovalStep(owed: DeviceRemovalOwed): RemovalStep = when {
    !owed.revoked -> RemovalStep.Revoke
    !owed.directoryDeleted -> RemovalStep.DirectoryDelete
    !owed.groupRemoved -> RemovalStep.DeviceGroupRemove
    !owed.chatsRetired -> RemovalStep.RetireChats
    else -> RemovalStep.Done
}

/**
 * Whether the Devices row of an owed removal shows `Removing...` with no
 * Remove control (UX 18.1 "signed out, removal still finishing"): only once
 * the device IS signed out at auth. Before that the row keeps its Remove
 * button, which is also the retry.
 */
public fun removalShowsPending(owed: DeviceRemovalOwed): Boolean = owed.revoked

/**
 * Whether the pending row also shows `Finish removing`: the device is signed
 * out at auth, its leaf is still in the DeviceGroup, and the step that takes
 * it out is waiting on the PERSON - the account's ADR-0015 policy requires a
 * removal attestation (a passkey gesture) and none usable is held, which is
 * the case after a relaunch or once the attestation aged out. Every other
 * owed step finishes by itself and shows no control.
 */
public fun removalWaitsOnPerson(owed: DeviceRemovalOwed, nowMs: Long): Boolean =
    owed.revoked && !owed.groupRemoved && owed.needsAttestation && usableAttestation(owed, nowMs) == null

/** Why a removal may not start now. */
public enum class RemovalGate {
    Allowed,

    /**
     * A key reset's publish is still owed on this device: auth already
     * holds the NEW identity key and the directory the OLD one, so the one
     * statement cannot satisfy both. The reset deleted every directory row
     * anyway. Blocked with the existing "publish owed" copy.
     */
    KeyResetPublishOwed,

    /** This device holds no account keys: it cannot sign the statement, so
     *  it cannot remove devices (UX 18.9). It can reset the keys instead. */
    NoAccountKeys,
}

/** The pure gate in front of the sheet's main button. */
public fun removalGate(holdsAccountKeys: Boolean, keyResetPublishOwed: Boolean): RemovalGate = when {
    !holdsAccountKeys -> RemovalGate.NoAccountKeys
    keyResetPublishOwed -> RemovalGate.KeyResetPublishOwed
    else -> RemovalGate.Allowed
}

/** Why the sign-out at auth did not happen. */
public sealed interface RevokeFailure {
    /** `403 fresh-auth-required`: the proof ran out; the block appears. */
    public data object ProofRequired : RevokeFailure

    /** `403 account-key-required`: auth has no recorded identity key for
     *  the account yet, or refused the statement. */
    public data object AccountKeyRequired : RevokeFailure

    /** `429`: wait, then the same button. */
    public class RateLimited(public val retryAfterSecs: Long?) : RevokeFailure

    /** No answer reached this device: the request MAY have landed. */
    public class Unconfirmed(public val cause: Throwable) : RevokeFailure

    /** Any other refusal: nothing was removed. */
    public class Refused(public val cause: Throwable) : RevokeFailure
}

/** Classify a failed `POST /account/devices/revoke`. */
public fun revokeFailureOf(err: Throwable): RevokeFailure = when {
    isFreshAuthRequired(err) -> RevokeFailure.ProofRequired
    err is AuthHttpError && err.status == HTTP_FORBIDDEN && err.code == ACCOUNT_KEY_REQUIRED ->
        RevokeFailure.AccountKeyRequired
    err is AuthHttpError && err.status == HTTP_TOO_MANY -> RevokeFailure.RateLimited(err.retryAfterSecs)
    err is AuthNetworkError -> RevokeFailure.Unconfirmed(err)
    else -> RevokeFailure.Refused(err)
}

/**
 * Whether the owed record survives a failed revoke. A definite refusal
 * means nothing happened: the record goes. A request with no answer may
 * have landed, so the record stays (not yet "pending" on screen: the row
 * keeps its Remove button, and the next attempt with a proof settles it).
 */
public fun keepOwedAfterRevokeFailure(failure: RevokeFailure): Boolean = failure is RevokeFailure.Unconfirmed

/** What the DeviceGroup step did. */
public enum class GroupRemoval {
    /** The Remove commit was built, registered and published. */
    Removed,

    /** No leaf holds the device's key: it is already out. */
    NotInRoster,

    /** The account's policy requires an ADR-0015 attestation (a passkey
     *  gesture) and none usable was supplied: the step waits for the next
     *  run that the person starts. Committing without one would be refused
     *  by every peer and fork this device. */
    NeedsAttestation,
}

/** How long a stored ADR-0015 attestation is used after it was obtained
 *  (its server lifetime is one hour; chosen with a margin). */
public const val REMOVAL_ATTESTATION_USABLE_MS: Long = 50L * 60L * 1000L

/** The stored attestation, when it may still ride with the commit. */
public fun usableAttestation(owed: DeviceRemovalOwed, nowMs: Long): ByteArray? {
    val b64 = owed.attestationB64Url ?: return null
    val at = owed.attestationAtMs ?: return null
    if (nowMs < at || nowMs - at > REMOVAL_ATTESTATION_USABLE_MS) return null
    return try {
        base64UrlDecodeNoPad(b64)
    } catch (_: IllegalArgumentException) {
        null
    }
}

/** How long one signed statement is reused for the next request of the same
 *  run (the server accepts it for 120 s; chosen with a margin). */
public const val REMOVAL_STATEMENT_REUSE_MS: Long = 60_000L

/** The outcome of one run of a removal. */
public sealed interface RemovalResult {
    /** Every step answered: the record is gone and so is the row. */
    public data object Done : RemovalResult

    /** The device was NOT signed out; nothing else was attempted. */
    public class NotRevoked(public val failure: RevokeFailure) : RemovalResult

    /** The device is signed out; [step] is still owed and is retried by
     *  itself the next time this device is online. Not an error. */
    public class Pending(public val step: RemovalStep) : RemovalResult
}

/** What the machine needs from the device and the servers. */
public interface DeviceRemovalPorts {
    /** A fresh `device-revoke` account statement for [deviceId] (138 bytes). */
    public fun signStatement(deviceId: ByteArray): ByteArray

    /** The live fresh-auth grant, or `null`. */
    public fun freshGrant(): String?

    /** `POST /account/devices/revoke`; throws on anything but `204`. */
    public suspend fun revoke(deviceId: ByteArray, statement: ByteArray, freshGrant: String)

    /** `DELETE /discovery/keypackage/device/{id}` with the statement. */
    public suspend fun deleteDirectoryRow(deviceId: ByteArray, statement: ByteArray)

    /** The DeviceGroup Remove commit. */
    public suspend fun removeFromDeviceGroup(deviceId: ByteArray, attestation: ByteArray?): GroupRemoval

    /** The leaf out of every chat; `true` when every chat answered. */
    public suspend fun retireFromChats(deviceId: ByteArray): Boolean
}

/** The owed removals of each account on this device. Plain storage: device
 *  ids are public, the attestation is a public signed artifact. */
public class DeviceRemovalOwedStore(private val storage: MarkerStorage) {

    public fun load(accountIdB64: String): List<DeviceRemovalOwed> {
        val raw = try {
            storage.get(KEY_PREFIX + accountIdB64)
        } catch (_: Throwable) {
            null
        } ?: return emptyList()
        return try {
            json.decodeFromString(LIST, raw)
        } catch (_: Throwable) {
            emptyList()
        }
    }

    public fun find(accountIdB64: String, deviceIdB64Url: String): DeviceRemovalOwed? =
        load(accountIdB64).firstOrNull { it.deviceIdB64Url == deviceIdB64Url }

    public fun save(accountIdB64: String, owed: DeviceRemovalOwed) {
        write(accountIdB64, load(accountIdB64).filterNot { it.deviceIdB64Url == owed.deviceIdB64Url } + owed)
    }

    public fun clear(accountIdB64: String, deviceIdB64Url: String) {
        write(accountIdB64, load(accountIdB64).filterNot { it.deviceIdB64Url == deviceIdB64Url })
    }

    public fun clearAccount(accountIdB64: String) {
        try {
            storage.remove(KEY_PREFIX + accountIdB64)
        } catch (_: Throwable) {
            // Nothing to drop.
        }
    }

    private fun write(accountIdB64: String, rows: List<DeviceRemovalOwed>) {
        try {
            if (rows.isEmpty()) {
                storage.remove(KEY_PREFIX + accountIdB64)
            } else {
                me.lakko.app.account.AccountStoreGate.write(accountIdB64) { storage.set(KEY_PREFIX + accountIdB64, json.encodeToString(LIST, rows)) }
            }
        } catch (_: Throwable) {
            // The debt then lives for this process only.
        }
    }

    public companion object {
        public const val PREFS_NAME: String = "lakko-device-removal-owed"
        private const val KEY_PREFIX: String = "device-removal-v1:"
        private val json = Json { ignoreUnknownKeys = true }
        private val LIST = ListSerializer(DeviceRemovalOwed.serializer())

        public fun onDevice(context: Context): DeviceRemovalOwedStore =
            DeviceRemovalOwedStore(SharedPrefsMarkerStorage(context, PREFS_NAME))
    }
}

/**
 * The removal machine for one account. [remove] is the sheet's button;
 * [resume] is run at every landing. One run at a time per process (the two
 * could otherwise both build a Remove commit for the same leaf).
 */
public class DeviceRemovalRunner(
    private val accountIdB64: String,
    private val store: DeviceRemovalOwedStore,
    private val ports: DeviceRemovalPorts,
    private val clock: () -> Long = { System.currentTimeMillis() },
    private val log: (String) -> Unit = {},
) {
    /** The devices whose row shows `Removing...` (base64url ids). */
    public fun pendingDeviceIds(): Set<String> =
        store.load(accountIdB64).filter(::removalShowsPending).map { it.deviceIdB64Url }.toSet()

    /** The pending devices whose row carries `Finish removing` (base64url ids). */
    public fun waitingOnPersonDeviceIds(): Set<String> {
        val now = clock()
        return store.load(accountIdB64).filter { removalWaitsOnPerson(it, now) }.map { it.deviceIdB64Url }.toSet()
    }

    /**
     * Remove [deviceId]: the sheet's main button, after its proof block.
     * [attestation] is the ADR-0015 removal attestation the passkey gesture
     * yielded, when the account has a passkey.
     *
     * Also the row's `Finish removing`: for a device already signed out at
     * auth the run starts at the first step still owed, needs no proof, and
     * the fresh [attestation] is what lets the DeviceGroup step go through.
     */
    public suspend fun remove(deviceId: ByteArray, attestation: ByteArray?): RemovalResult = lock.withLock {
        val id = base64UrlEncodeNoPad(deviceId)
        val now = clock()
        val earlier = store.find(accountIdB64, id)
        var owed = earlier ?: DeviceRemovalOwed(deviceIdB64Url = id, startedAtMs = now)
        if (attestation != null) {
            owed = owed.with(
                attestationB64Url = base64UrlEncodeNoPad(attestation),
                attestationAtMs = now,
                needsAttestation = false,
            )
        }
        // Before the revoke goes out: a crash between the request and its
        // answer must leave something to retry.
        store.save(accountIdB64, owed)
        advance(owed, deviceId, interactive = true, fromEarlierRun = earlier != null)
    }

    /** Retry everything still owed. Results by device id (base64url). */
    public suspend fun resume(): Map<String, RemovalResult> = lock.withLock {
        val results = LinkedHashMap<String, RemovalResult>()
        for (owed in store.load(accountIdB64)) {
            val deviceId = try {
                base64UrlDecodeNoPad(owed.deviceIdB64Url)
            } catch (_: IllegalArgumentException) {
                store.clear(accountIdB64, owed.deviceIdB64Url)
                continue
            }
            results[owed.deviceIdB64Url] = advance(owed, deviceId, interactive = false, fromEarlierRun = true)
        }
        results
    }

    private suspend fun advance(
        start: DeviceRemovalOwed,
        deviceId: ByteArray,
        interactive: Boolean,
        /** The record existed before this run (an earlier revoke whose
         *  answer never arrived): it is not dropped for want of a proof. */
        fromEarlierRun: Boolean,
    ): RemovalResult {
        var owed = start
        var statement: ByteArray? = null
        var statementAtMs = 0L
        fun statementNow(): ByteArray {
            val now = clock()
            val held = statement
            if (held != null && now >= statementAtMs && now - statementAtMs <= REMOVAL_STATEMENT_REUSE_MS) return held
            return ports.signStatement(deviceId).also {
                statement = it
                statementAtMs = now
            }
        }
        while (true) {
            when (val step = nextRemovalStep(owed)) {
                RemovalStep.Revoke -> {
                    val grant = ports.freshGrant()
                    if (grant == null) {
                        // Nothing was sent by THIS run. The sheet shows the
                        // proof block; a record left by an earlier run whose
                        // answer was lost stays for the next one with a proof.
                        if (!fromEarlierRun) store.clear(accountIdB64, owed.deviceIdB64Url)
                        return if (interactive) {
                            RemovalResult.NotRevoked(RevokeFailure.ProofRequired)
                        } else {
                            RemovalResult.Pending(step)
                        }
                    }
                    try {
                        ports.revoke(deviceId, statementNow(), grant)
                    } catch (cancel: CancellationException) {
                        throw cancel
                    } catch (cause: Throwable) {
                        val failure = revokeFailureOf(cause)
                        log("device removal: sign-out at auth not done (${failure.javaClass.simpleName})")
                        if (!keepOwedAfterRevokeFailure(failure)) store.clear(accountIdB64, owed.deviceIdB64Url)
                        return RemovalResult.NotRevoked(failure)
                    }
                    owed = owed.with(revoked = true).also { store.save(accountIdB64, it) }
                }
                RemovalStep.DirectoryDelete -> {
                    try {
                        ports.deleteDirectoryRow(deviceId, statementNow())
                    } catch (cancel: CancellationException) {
                        throw cancel
                    } catch (cause: Throwable) {
                        log("device removal: directory row not deleted yet (${cause.javaClass.simpleName})")
                        return RemovalResult.Pending(step)
                    }
                    owed = owed.with(directoryDeleted = true).also { store.save(accountIdB64, it) }
                }
                RemovalStep.DeviceGroupRemove -> {
                    val outcome = try {
                        ports.removeFromDeviceGroup(deviceId, usableAttestation(owed, clock()))
                    } catch (cancel: CancellationException) {
                        throw cancel
                    } catch (cause: Throwable) {
                        log("device removal: device list not updated yet (${cause.javaClass.simpleName})")
                        return RemovalResult.Pending(step)
                    }
                    if (outcome == GroupRemoval.NeedsAttestation) {
                        // Recorded, so the row offers `Finish removing`: a
                        // device revoked at auth is never left in the
                        // DeviceGroup with no way to finish.
                        log("device removal: the device list needs a passkey gesture; waiting on the person")
                        owed.with(needsAttestation = true).also { store.save(accountIdB64, it) }
                        return RemovalResult.Pending(step)
                    }
                    owed = owed.with(groupRemoved = true, needsAttestation = false)
                        .also { store.save(accountIdB64, it) }
                }
                RemovalStep.RetireChats -> {
                    val all = try {
                        ports.retireFromChats(deviceId)
                    } catch (cancel: CancellationException) {
                        throw cancel
                    } catch (cause: Throwable) {
                        log("device removal: chats not updated yet (${cause.javaClass.simpleName})")
                        false
                    }
                    if (!all) return RemovalResult.Pending(step)
                    owed = owed.with(chatsRetired = true).also { store.save(accountIdB64, it) }
                }
                RemovalStep.Done -> {
                    store.clear(accountIdB64, owed.deviceIdB64Url)
                    return RemovalResult.Done
                }
            }
        }
    }

    public companion object {
        /** One removal run at a time in this process. */
        private val lock = Mutex()

        /**
         * The runner over the device: auth, discovery, the DeviceGroup and
         * the chats. `null` when no session with the account's keys is live.
         */
        public fun onDevice(
            context: Context,
            authRepository: AuthRepository,
            chatRepository: me.lakko.app.chat.ChatRepository?,
        ): DeviceRemovalRunner? {
            val secrets = authRepository.identitySecrets.value ?: return null
            val app = context.applicationContext
            val accountIdBytes = secrets.accountId.copyOf()
            val accountIdB64 = me.lakko.app.auth.base64Encode(accountIdBytes)
            val ports = object : DeviceRemovalPorts {
                private fun token(): String =
                    authRepository.identitySecrets.value?.sessionToken ?: secrets.sessionToken

                override fun signStatement(deviceId: ByteArray): ByteArray {
                    val live = authRepository.identitySecrets.value
                        ?: throw IllegalStateException("no session to sign the statement with")
                    val seed = Identity.fromSeed(live.accountIdentitySeed).use { it.ed25519Seed() }
                    try {
                        return signDeviceRevokeStatement(seed, accountIdBytes, deviceId)
                    } finally {
                        seed.fill(0)
                    }
                }

                override fun freshGrant(): String? = authRepository.freshGrant()

                override suspend fun revoke(deviceId: ByteArray, statement: ByteArray, freshGrant: String) {
                    AuthClient().use { it.revokeDevice(token(), freshGrant, deviceId, statement) }
                }

                override suspend fun deleteDirectoryRow(deviceId: ByteArray, statement: ByteArray) {
                    DiscoveryClient().use { client ->
                        client.setSessionToken(token())
                        client.deleteKeyPackage(deviceId, statement, accountStatementHeaderName())
                    }
                }

                override suspend fun removeFromDeviceGroup(deviceId: ByteArray, attestation: ByteArray?): GroupRemoval =
                    DeviceRemoval.removeDeviceByKey(
                        context = app,
                        accountIdBytes = accountIdBytes,
                        accountIdB64 = accountIdB64,
                        deviceKey = deviceId,
                        attestation = attestation,
                    )

                override suspend fun retireFromChats(deviceId: ByteArray): Boolean {
                    val chats = chatRepository ?: return false
                    return chats.retireDevice(deviceId).failed == 0
                }
            }
            return DeviceRemovalRunner(
                accountIdB64 = accountIdB64,
                store = DeviceRemovalOwedStore.onDevice(app),
                ports = ports,
                log = { message -> android.util.Log.w("DeviceRemoval", message) },
            )
        }
    }
}

private const val HTTP_FORBIDDEN: Int = 403
private const val HTTP_TOO_MANY: Int = 429
