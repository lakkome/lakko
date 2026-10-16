// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices

import android.content.Context
import android.util.Log
import me.lakko.app.auth.AccountIdentitySeedStore
import me.lakko.app.auth.OwnPredecessorStore
import uniffi.lakko_ffi_uniffi.DeviceGroup
import uniffi.lakko_ffi_uniffi.Identity
import uniffi.lakko_ffi_uniffi.deviceGroupIdFor
import uniffi.lakko_ffi_uniffi.loadDeviceGroup

/**
 * DeviceGroup lifecycle on Android (Phase 2a3b4, ADR-0007).
 *
 * Mirror of:
 *
 *   * `apps/web/src/devices/lifecycle.ts`
 *   * `apps/ios/Lakko/Devices/DeviceGroupLifecycle.swift`
 *
 * One entry point — [ensureDeviceGroup] — wraps the
 * "load existing or create-founder" decision plus the
 * post-mutation snapshot persistence so the per-screen call
 * sites see a single function.
 *
 * The [Identity] this module builds from the cached
 * `deviceIdentitySeed` carries its own in-memory OpenMLS
 * provider, **independent** of the AccountIdentity provider
 * that `ChatRepository` holds for conversation groups
 * (ADR-0007 §"two providers, two snapshots"). Snapshot /
 * restore goes through this DeviceIdentity provider's storage
 * only, persisted in [DeviceGroupSnapshotStore].
 *
 * ADR-0078 section 6 — the DeviceGroup id is derived from the account's
 * LINEAGE seed ([lineageSeed]: the earliest own predecessor after an
 * ADR-0074 rotation, else the current seed), never from the account id, so
 * a server holding the account id cannot compute the journal id, and a
 * rotation that keeps the devices keeps the journal.
 *
 * The returned [Identity] + [DeviceGroup] are FFI handles
 * backed by JNA pointers. Callers MUST close them when done —
 * idiomatic use is a `try { ... } finally { ensured.group.close();
 * ensured.identity.close() }` pair, mirroring the wasm
 * `.free()` discipline.
 */
public object DeviceGroupLifecycle {

    private const val LOG_TAG = "DeviceGroupLifecycle"

    /**
     * The store session each handle built by [ensureDeviceGroup] was loaded
     * in. Weak: an entry goes with its handle.
     */
    private val storeSessions: MutableMap<Identity, me.lakko.app.account.AccountStoreGate.Session> =
        java.util.Collections.synchronizedMap(java.util.WeakHashMap())

    /** Thrown by [ensureDeviceGroup] when the device holds no account seed. */
    public class NoAccountSeed : IllegalStateException("no account seed on this device")

    /**
     * ADR-0078 section 6 — the account seed the DeviceGroup id is derived
     * from: the EARLIEST own predecessor when the account rotated, else
     * [currentSeed], else the cached account seed. `null` when the device
     * holds none. Always a fresh copy.
     */
    public fun lineageSeed(context: Context, accountIdB64: String, currentSeed: ByteArray? = null): ByteArray? =
        try {
            OwnPredecessorStore.onDevice(context).conversationSeed(accountIdB64)
        } catch (cause: Throwable) {
            Log.w(LOG_TAG, "own predecessors unreadable; the current seed names the DeviceGroup", cause)
            null
        } ?: currentSeed?.copyOf()
            ?: AccountIdentitySeedStore.getAccountIdentitySeed(context, accountIdB64)

    /** The account's DeviceGroup journal id ([lineageSeed]); `null` when this
     *  device cannot derive it yet. */
    public fun groupId(context: Context, accountIdB64: String, currentSeed: ByteArray? = null): ByteArray? {
        val seed = lineageSeed(context, accountIdB64, currentSeed) ?: return null
        return try {
            Identity.fromSeed(seed).use { deviceGroupIdFor(it) }
        } catch (cause: Throwable) {
            Log.w(LOG_TAG, "DeviceGroup id not derived", cause)
            null
        } finally {
            seed.fill(0)
        }
    }

    /** Result of [ensureDeviceGroup]. */
    public data class Ensured(
        /** Identity built from the DeviceIdentity seed,
         *  populated with the (loaded or freshly-bootstrapped)
         *  MLS provider state. Caller owns the close()
         *  responsibility. */
        val identity: Identity,
        /** Live DeviceGroup handle. Caller owns close(). */
        val group: DeviceGroup,
        /** `true` when this call bootstrapped a fresh
         *  DeviceGroup (this device is now the founder). */
        val bootstrapped: Boolean,
    )

    /**
     * Resolve the local DeviceGroup state: either restore the
     * cached snapshot or bootstrap a fresh founder leaf.
     *
     * Idempotent on the snapshot side — calling twice with the
     * same `(accountIdB64, deviceIdentitySeed)` returns the same
     * group state (the bootstrap branch fires only when the
     * snapshot is absent).
     *
     * The caller owns the lifetime of the returned [Identity]
     * and [DeviceGroup] handles. Mirror the `try / finally
     * close()` pattern used in [PairingOrchestrator.runScanFlow].
     *
     * @throws Exception when the seed is the wrong shape, the
     *         FFI surface throws, or the snapshot is corrupt
     *         beyond recovery. The defensive "fall through and
     *         bootstrap" branch swallows snapshot-format errors
     *         (logged), so the typical user-visible failure
     *         mode is an FFI exception from `createFounder` /
     *         `loadDeviceGroup`.
     * @throws NoAccountSeed when the device holds no account seed to derive
     *         the DeviceGroup id from (ADR-0078 section 6). `accountSeed` is
     *         the current seed when the caller holds it (a key reset passes
     *         the NEW seed).
     */
    public fun ensureDeviceGroup(
        context: Context,
        accountIdBytes: ByteArray,
        accountIdB64: String,
        deviceIdentitySeed: ByteArray,
        accountSeed: ByteArray? = null,
    ): Ensured {
        val lineage = lineageSeed(context, accountIdB64, accountSeed) ?: throw NoAccountSeed()
        val lineageAccount = try {
            Identity.fromSeed(lineage)
        } finally {
            lineage.fill(0)
        }
        try {
            return ensureDeviceGroupWith(context, accountIdBytes, accountIdB64, deviceIdentitySeed, lineageAccount)
        } finally {
            lineageAccount.close()
        }
    }

    private fun ensureDeviceGroupWith(
        context: Context,
        accountIdBytes: ByteArray,
        accountIdB64: String,
        deviceIdentitySeed: ByteArray,
        lineageAccount: Identity,
    ): Ensured {
        // Build the per-device Identity from the cached seed.
        // This is the DeviceIdentity (ADR-0007), NOT the
        // AccountIdentity ChatRepository holds.
        val identity = Identity.fromSeed(deviceIdentitySeed)
        // The provider state this handle is about to load is the state of
        // the account's session as it is open NOW; [persistSnapshot] writes
        // the handle back only into that same session.
        storeSessions[identity] = me.lakko.app.account.AccountStoreGate.session(accountIdB64)

        // Try to restore prior MLS state.
        val snapshot = try {
            DeviceGroupSnapshotStore.getSnapshot(context, accountIdB64)
        } catch (cause: Throwable) {
            Log.w(LOG_TAG, "DeviceGroup snapshot read failed; bootstrapping", cause)
            null
        }
        if (snapshot != null) {
            try {
                identity.restoreStorage(snapshot)
                val group = loadDeviceGroup(identity, accountIdBytes, lineageAccount)
                if (group != null) {
                    return Ensured(
                        identity = identity,
                        group = group,
                        bootstrapped = false,
                    )
                }
                // Snapshot existed but didn't carry the
                // DeviceGroup row — fall through to bootstrap.
                // Defensive: shouldn't happen in normal flows
                // because every snapshot we write was captured
                // immediately after a DeviceGroup mutation.
                Log.w(
                    LOG_TAG,
                    "snapshot restored but no DeviceGroup row " +
                        "for account; bootstrapping",
                )
            } catch (cause: Throwable) {
                Log.w(LOG_TAG, "DeviceGroup snapshot restore failed; bootstrapping", cause)
                // Fall through to founder bootstrap.
            }
        }

        // Bootstrap. Local device becomes the founding leaf.
        val group = DeviceGroup.createFounder(identity, accountIdBytes, lineageAccount)
        // ADR-0057 second security pass, finding 3 — the founding is not
        // claimed yet: record that BEFORE it is persisted, so no path adds a
        // device (and sends a Welcome) until a claim lands. The marker is
        // cleared by the first claim that answers 204.
        DeviceGroupJournalAccess.markFoundingUnconfirmed(context, accountIdB64)
        val fresh = identity.snapshotStorage()
        try {
            DeviceGroupSnapshotStore.putSnapshot(context, accountIdB64, fresh)
        } catch (cause: Throwable) {
            Log.w(LOG_TAG, "DeviceGroup snapshot persist failed (bootstrap)", cause)
            // Best-effort. The in-memory state is still valid;
            // the next app launch will re-bootstrap.
        }
        return Ensured(identity = identity, group = group, bootstrapped = true)
    }

    /**
     * ADR-0073 section 5 — the DeviceGroup this device holds was founded under
     * the account's OLD seed, whose root the attested reset is replacing.
     * Drop its MLS snapshot (its own store, apart from the conversation
     * groups) and the stale founding marker, so the next [ensureDeviceGroup]
     * founds a fresh DeviceGroup under the id the NEW seed derives (ADR-0078
     * section 6; the reset cleared the own predecessors). The folded
     * sync state and the device identity seed stay: they belong to the
     * account and to this device, not to the group's root.
     */
    public fun discardForKeyReset(context: Context, accountIdB64: String) {
        try {
            DeviceGroupSnapshotStore.deleteSnapshot(context, accountIdB64)
        } catch (cause: Throwable) {
            Log.w(LOG_TAG, "DeviceGroup snapshot delete (key reset) failed", cause)
        }
        DeviceGroupJournalAccess.clearFoundingUnconfirmed(context, accountIdB64)
    }

    /**
     * Re-capture the MLS provider snapshot after a DeviceGroup
     * mutation (add-device, remove-device, application-message
     * send). Best-effort — a snapshot persist failure logs
     * and continues.
     */
    public fun persistSnapshot(
        context: Context,
        identity: Identity,
        accountIdB64: String,
    ) {
        try {
            val snapshot = identity.snapshotStorage()
            val session = storeSessions[identity]
            if (session == null) {
                DeviceGroupSnapshotStore.putSnapshot(context, accountIdB64, snapshot)
            } else {
                // A handle loaded in an EARLIER session of this account (it
                // signed out and in again meanwhile) holds that session's
                // DeviceGroup state: an older epoch, an older ratchet. The
                // gate is open for the account again, so only the session
                // the handle was loaded in stops it from rolling the
                // snapshot back.
                me.lakko.app.account.AccountStoreGate.writeOrEnd(session) {
                    DeviceGroupSnapshotStore.putSnapshot(context, accountIdB64, snapshot)
                }
            }
        } catch (cause: Throwable) {
            Log.w(LOG_TAG, "DeviceGroup snapshot persist failed", cause)
        }
    }
}
