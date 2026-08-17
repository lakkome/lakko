// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.push

import android.app.ActivityManager
import android.content.Context
import android.os.Process
import me.lakko.app.auth.AccountIdentitySeedStore
import me.lakko.app.chat.IdentitySnapshotStore
import me.lakko.app.chat.KnownGroupsStore
import me.lakko.app.chat.MessageLogStore
import me.lakko.app.ds.DsClient
import uniffi.lakko_ffi_uniffi.Group
import uniffi.lakko_ffi_uniffi.Identity
import uniffi.lakko_ffi_uniffi.MessageBodyKind
import uniffi.lakko_ffi_uniffi.ProcessedMessage
import uniffi.lakko_ffi_uniffi.decodePushRoutingSecret
import uniffi.lakko_ffi_uniffi.loadGroup
import uniffi.lakko_ffi_uniffi.parseBodyEnvelope

/**
 * Production wiring for the FCM silent-push background decrypt — the
 * Android counterpart of the web `sw.js` `buildDecryptDeps` +
 * `loadSwWasm`. Everything here touches the Android framework
 * (Keystore-backed stores, `ActivityManager`) or the UniFFI native
 * `.so` (`Identity` / `Group` / `parseBodyEnvelope`), so it is NOT unit
 * tested; the pure, native-free decrypt core in `PushPreviewDecrypt.kt`
 * carries the tested logic.
 *
 * The one load-bearing invariant this file must uphold is the
 * READ-ONLY THROWAWAY discipline (see `PushPreviewDecrypt.kt`): the
 * deps object it builds has NO write path, and [RealThrowawayGroup.close]
 * frees the throwaway handles WITHOUT ever calling
 * `Identity.snapshotStorage()` / `IdentitySnapshotStore.putSnapshot`.
 */

/**
 * `true` when this process currently has a visible foreground
 * activity — in which case the live app runs its own poll loop and
 * shows its own notification, so the background decrypt is skipped
 * (the direct analog of the web SW's `clients.matchAll` open-visible-
 * tab defer).
 *
 * Uses `ActivityManager` process importance rather than a lifecycle
 * observer so it needs no new dependency and no Application subclass.
 * `IMPORTANCE_FOREGROUND` (the most-important band) means a visible,
 * focused activity exists; a process running ONLY the FMS sits at
 * `IMPORTANCE_FOREGROUND_SERVICE` (a less-important band), so this
 * cleanly distinguishes "user is looking at the app" from "we were
 * woken in the background". Any failure fails toward `false`
 * (background) so a probe error never suppresses the real preview.
 */
internal fun isAppForegrounded(context: Context): Boolean {
    return try {
        val am = context.getSystemService(Context.ACTIVITY_SERVICE) as? ActivityManager
            ?: return false
        val myPid = Process.myPid()
        am.runningAppProcesses?.any { info ->
            info.pid == myPid &&
                info.importance == ActivityManager.RunningAppProcessInfo.IMPORTANCE_FOREGROUND
        } ?: false
    } catch (_: Throwable) {
        false
    }
}

/**
 * Build the READ-ONLY [PushDecryptDeps] the pure core consumes. Every
 * capability is a store read, a pure compute, or a DS GET — there is
 * deliberately NO write path (no snapshot persist, no cursor advance).
 *
 * @param ds a [DsClient] owned by the caller (closed after the decrypt).
 */
internal fun buildBackgroundDecryptDeps(
    context: Context,
    ds: DsClient,
): PushDecryptDeps {
    val appContext = context.applicationContext
    val knownGroups = KnownGroupsStore(appContext)
    val pushRoutes = PushRouteStore(appContext)
    return object : PushDecryptDeps {
        override fun lookupRoute(pushGidHex: String): Pair<String, String>? =
            try {
                // ADR-0042 (Sprint 4): the plaintext reverse index
                // Sprint 3 populates — a READ, deliberately without
                // the device key (the FMS runs in the background).
                pushRoutes.lookup(pushGidHex)?.let { it.accountIdB64 to it.groupIdHex }
            } catch (_: Throwable) {
                null
            }

        override fun resolveAccount(groupHex: String): ResolvedPushAccount? {
            val accountIdB64 = knownGroups.findAccountForGroup(groupHex) ?: return null
            val groupIdBytes = hexToBytes(groupHex) ?: return null
            return ResolvedPushAccount(accountIdB64 = accountIdB64, groupIdBytes = groupIdBytes)
        }

        override fun loadIdentitySeed(accountIdB64: String): ByteArray? =
            try {
                AccountIdentitySeedStore.getAccountIdentitySeed(appContext, accountIdB64)
            } catch (_: Throwable) {
                null
            }

        override fun loadSnapshot(accountIdB64: String): ByteArray? =
            try {
                IdentitySnapshotStore.getSnapshot(appContext, accountIdB64)
            } catch (_: Throwable) {
                null
            }

        override fun openThrowawayGroup(
            seed: ByteArray,
            snapshot: ByteArray,
            groupIdBytes: ByteArray,
        ): ThrowawayGroup? {
            // A FRESH throwaway Identity — never the app's live handle.
            // Restoring into it lays down provider storage in this
            // temp Identity only; the live Identity is untouched.
            var identity: Identity? = null
            return try {
                identity = Identity.fromSeed(seed)
                identity.restoreStorage(snapshot)
                val group = loadGroup(identity, groupIdBytes)
                if (group == null) {
                    // Not a member of this group (or the group is not in
                    // the restored provider) — discard the temp Identity.
                    identity.close()
                    null
                } else {
                    RealThrowawayGroup(identity, group)
                }
            } catch (_: Throwable) {
                try {
                    identity?.close()
                } catch (_: Throwable) {
                    // best-effort
                }
                null
            }
        }

        override suspend fun dsFetch(groupIdBytes: ByteArray, afterId: Long): List<FetchedRow> {
            // wait=0 — a catch-up fetch, never a long-poll (the FMS
            // budget is short). Ascending id order from the DS.
            val res = ds.fetchGroup(groupIdBytes, after = afterId, waitSecs = 0)
            return res.messages.map { FetchedRow(id = it.id, payload = it.payload) }
        }

        override fun startCursor(accountIdB64: String, groupHex: String): Long? =
            try {
                MessageLogStore.getLog(appContext, accountIdB64)?.groups?.get(groupHex)?.cursor
            } catch (_: Throwable) {
                null
            }
    }
}

/**
 * A THROWAWAY MLS group backed by a temporary [Identity] + [Group]
 * restored from the last-persisted snapshot.
 *
 * ## READ-ONLY THROWAWAY — load-bearing
 *
 * [process] runs the audited [Group.processIncomingVerifyingRoles]
 * receive path (advancing THIS temp provider's ratchet in memory) but
 * NEVER persists it, and [close] frees the handles WITHOUT ever calling
 * `identity.snapshotStorage()` / `IdentitySnapshotStore.putSnapshot`.
 * Persisting the advanced ratchet would desync the foreground app,
 * which restores from the SAME last-persisted snapshot and does the
 * real durable processing from its OWN cursor (the G-34
 * nonce-reuse-on-crash class). There is intentionally no persist call
 * anywhere in this class.
 */
private class RealThrowawayGroup(
    private val identity: Identity,
    private val group: Group,
) : ThrowawayGroup {

    override fun process(payload: ByteArray): ProcessedRow? {
        // ADR-0022 — the same role-verifying receive path the live poll
        // loop uses. A bad frame / wrong epoch / own echo / role-refused
        // commit throws and is dropped by the pure core (fail-closed).
        val processed = try {
            group.processIncomingVerifyingRoles(identity, payload)
        } catch (_: Throwable) {
            return null
        }
        // Only Application messages carry a user-visible preview. A
        // Commit / Proposal / ExternalJoinProposal advances state (in
        // the THROWAWAY provider only) but has no body to show.
        val app = processed as? ProcessedMessage.Application ?: return null
        // A SERVICE envelope (ADR-0042/0044 push-routing-secret, or a
        // call-signaling proto) is machine-to-machine traffic with no
        // user-visible body. It MUST be rejected before
        // `parseBodyEnvelope`, whose deliberate legacy fallback would
        // otherwise degrade the raw bytes to `Chat` via a lossy UTF-8
        // decode and render the domain string plus binary as a
        // notification. Returning `null` here lets the caller's loop
        // fall back to the newest REAL chat message (and, when there is
        // none, to the generic "New message" banner). Same filter the
        // foreground `ChatClient` dispatch uses.
        if (isServiceEnvelope(app.data, ::decodesAsRoutingSecret)) return null
        val body = try {
            parseBodyEnvelope(app.data)
        } catch (_: Throwable) {
            return null
        }
        val kind = when (body.kind) {
            MessageBodyKind.CHAT -> ProcessedKind.CHAT
            MessageBodyKind.EDIT -> ProcessedKind.EDIT
            MessageBodyKind.DELETE -> ProcessedKind.DELETE
            MessageBodyKind.REACTION -> ProcessedKind.REACTION
            MessageBodyKind.TYPING -> ProcessedKind.TYPING
            MessageBodyKind.READ_RECEIPT -> ProcessedKind.READ_RECEIPT
        }
        return ProcessedRow(
            kind = kind,
            text = body.text,
            attachmentCount = body.attachments.size,
        )
    }

    /**
     * Free the throwaway [Group] + [Identity]. LOAD-BEARING: this does
     * NOT persist — no `snapshotStorage()` / `putSnapshot` — so the
     * background decrypt leaves zero durable MLS state behind.
     */
    override fun close() {
        try {
            group.close()
        } catch (_: Throwable) {
            // best-effort
        }
        try {
            identity.close()
        } catch (_: Throwable) {
            // best-effort
        }
    }
}

/**
 * The AUTHORITATIVE push-routing-secret check ([isServiceEnvelope]'s
 * injected second step) — the native, fail-closed
 * `decodePushRoutingSecret`. `true` only when the bytes really are a
 * current-version envelope; a wrong domain, a bad length, or a retired
 * v1 envelope throws and yields `false`, so the payload falls through to
 * the normal chat parse. Lives here (not in the pure filter) because it
 * is the one part that needs the UniFFI `.so`.
 */
private fun decodesAsRoutingSecret(bytes: ByteArray): Boolean =
    try {
        decodePushRoutingSecret(bytes)
        true
    } catch (_: Throwable) {
        false
    }

/** Decode a lowercase hex string into bytes. Returns `null` on any
 *  malformed input (odd length, non-hex chars). */
private fun hexToBytes(hex: String): ByteArray? {
    if (hex.length % 2 != 0) return null
    val out = ByteArray(hex.length / 2)
    for (i in out.indices) {
        val hi = Character.digit(hex[i * 2], 16)
        val lo = Character.digit(hex[i * 2 + 1], 16)
        if (hi < 0 || lo < 0) return null
        out[i] = ((hi shl 4) or lo).toByte()
    }
    return out
}
