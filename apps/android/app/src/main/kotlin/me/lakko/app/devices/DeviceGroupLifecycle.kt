// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices

import android.content.Context
import android.util.Log
import uniffi.lakko_ffi_uniffi.DeviceGroup
import uniffi.lakko_ffi_uniffi.Identity
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
 * The returned [Identity] + [DeviceGroup] are FFI handles
 * backed by JNA pointers. Callers MUST close them when done —
 * idiomatic use is a `try { ... } finally { ensured.group.close();
 * ensured.identity.close() }` pair, mirroring the wasm
 * `.free()` discipline.
 */
public object DeviceGroupLifecycle {

    private const val LOG_TAG = "DeviceGroupLifecycle"

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
     */
    public fun ensureDeviceGroup(
        context: Context,
        accountIdBytes: ByteArray,
        accountIdB64: String,
        deviceIdentitySeed: ByteArray,
    ): Ensured {
        // Build the per-device Identity from the cached seed.
        // This is the DeviceIdentity (ADR-0007), NOT the
        // AccountIdentity ChatRepository holds.
        val identity = Identity.fromSeed(deviceIdentitySeed)

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
                val group = loadDeviceGroup(identity, accountIdBytes)
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
        val group = DeviceGroup.createFounder(identity, accountIdBytes)
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
            DeviceGroupSnapshotStore.putSnapshot(context, accountIdB64, snapshot)
        } catch (cause: Throwable) {
            Log.w(LOG_TAG, "DeviceGroup snapshot persist failed", cause)
        }
    }
}
