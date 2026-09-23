// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.push

import android.app.ActivityManager
import android.content.Context
import android.os.Process
import me.lakko.app.chat.IdentitySnapshotStore
import me.lakko.app.chat.KnownGroupsStore
import me.lakko.app.chat.MessageLogStore
import me.lakko.app.ds.DsClient
import uniffi.lakko_ffi_uniffi.FfiCallSignalingBody
import uniffi.lakko_ffi_uniffi.Group
import uniffi.lakko_ffi_uniffi.Identity
import uniffi.lakko_ffi_uniffi.MessageBodyKind
import uniffi.lakko_ffi_uniffi.ProcessedMessage
import uniffi.lakko_ffi_uniffi.committerBeaconEnvelopeLen
import uniffi.lakko_ffi_uniffi.decodeCommitterBeacon
import uniffi.lakko_ffi_uniffi.decodePushRoutingSecret
import uniffi.lakko_ffi_uniffi.loadGroup
import uniffi.lakko_ffi_uniffi.parseBodyEnvelope
import uniffi.lakko_ffi_uniffi.parseCallSignaling

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
    // ADR-0048 — READ-ONLY names for the notification title: loaded once from
    // what the app already stored. No write, no verification, no network
    // (C-59). A load failure just means the generic title.
    val names: me.lakko.app.profile.ProfileNameSnapshot? = try {
        me.lakko.app.profile.ProfileNameSnapshot.load(appContext)
    } catch (_: Throwable) {
        null
    }
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

        // ADR-0050 — the MLS snapshot is written under this device's
        // conversation identity (the per-install DEVICE seed, bound to the
        // account), not the account identity, so the throwaway identity is
        // rebuilt from the device seed. Read-only: nothing here mints or
        // persists a binding (decrypt needs only the leaf key).
        override fun loadIdentitySeed(accountIdB64: String): ByteArray? =
            try {
                me.lakko.app.auth.DeviceIdentityStore.getDeviceIdentitySeed(appContext, accountIdB64)
            } catch (_: Throwable) {
                null
            }

        override fun loadSnapshot(accountIdB64: String): ByteArray? =
            try {
                // A snapshot taken under another leaf identity would restore
                // groups this identity cannot open; treat it as absent.
                val recorded = IdentitySnapshotStore.getSnapshotLeafKey(appContext, accountIdB64)
                val ownLeaf = me.lakko.app.auth.DeviceIdentityStore
                    .getDeviceIdentitySeed(appContext, accountIdB64)
                    ?.let { seed ->
                        try {
                            Identity.fromSeed(seed).use { it.signaturePublicKey() }
                        } finally {
                            seed.fill(0)
                        }
                    }
                if (recorded == null || ownLeaf == null || !recorded.contentEquals(ownLeaf)) {
                    null
                } else {
                    IdentitySnapshotStore.getSnapshot(appContext, accountIdB64)
                }
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
                    RealThrowawayGroup(identity, group, names)
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

        // `docs/ux/calls.md` section 2.1 — the conversation's mute flag, from
        // the cross-device sync state the app already persists it in. A READ,
        // like everything else here (C-59), and the one store on this
        // interface that is NOT about decrypting: a wake decides whether to
        // interrupt, and a muted chat never rings.
        override fun isMuted(accountIdB64: String, groupHex: String): Boolean =
            me.lakko.app.devices.sync.ConversationMute.isMuted(
                context = appContext,
                accountIdB64 = accountIdB64,
                groupIdHex = groupHex,
            )
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
    /** ADR-0048 — read-only name lookup, or `null`. */
    private val names: me.lakko.app.profile.ProfileNameSnapshot?,
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
        // A SERVICE envelope (ADR-0042/0044 push-routing-secret, an
        // ADR-0045 committer beacon, a profile, a group name, a profile
        // request) is
        // machine-to-machine traffic with no user-visible body. It MUST
        // be rejected before `parseBodyEnvelope`, whose deliberate
        // legacy fallback would otherwise degrade the raw bytes to
        // `Chat` via a lossy UTF-8 decode and render the domain string
        // plus binary as a notification. Returning `null` here lets the
        // caller's loop fall back to the newest REAL chat message (and,
        // when there is none, to the generic "New message" banner). Same
        // filter the foreground `ChatClient` dispatch uses.
        //
        // Call signaling is the one kind that is classified rather than
        // dropped: it is still never a preview, but an offer rings the
        // phone (docs/ux/calls.md section 2.1).
        when (
            classifyBackgroundPayload(
                app.data,
                ::decodesAsRoutingSecret,
                ::decodesAsCommitterBeacon,
                committerBeaconEnvelopeLenCached,
            )
        ) {
            BackgroundPayloadKind.SERVICE_ENVELOPE -> return null
            BackgroundPayloadKind.CALL_SIGNALING ->
                return callSignalingRow(app.data, senderNameOf(app.senderLeaf))
            BackgroundPayloadKind.MESSAGE -> Unit
        }
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
            senderName = senderNameOf(app.senderLeaf),
        )
    }

    /**
     * ADR-0048 — the sender's already-known name. The leaf is resolved to a
     * signature key against the throwaway roster right after the apply (the
     * epoch the message was sealed at, C-68), then looked up in the read-only
     * snapshot. A fingerprint-only result is not a name worth a title.
     */
    private fun senderNameOf(senderLeaf: UInt?): String? {
        val lookup = names ?: return null
        val leaf = senderLeaf ?: return null
        return try {
            val member = group.members().firstOrNull { it.leafIndex == leaf } ?: return null
            val resolved = lookup.resolve(java.util.Base64.getEncoder().encodeToString(member.signaturePublicKey))
            resolved.primary.takeIf { !resolved.isPlaceholder }
        } catch (_: Throwable) {
            null
        }
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
 * Turn a call-signaling frame into the row the ring fold consumes, or `null`
 * when it is a frame nobody needs to be woken for.
 *
 * Only an offer and an end are kept — the two
 * [me.lakko.app.calls.shouldWakeForCallSignaling] sends a wake for. An answer,
 * an ICE candidate and the group-control bodies belong to a call whose peer is
 * already awake, so a wake carrying one of those means only that a batch was
 * fetched, and nothing should ring.
 *
 * The decode is the FFI's, so this lives in the production wiring rather than
 * in the pure core; a frame that will not parse is dropped (fail-closed,
 * invariant #4), never guessed at.
 */
private fun callSignalingRow(payload: ByteArray, senderName: String?): ProcessedRow? {
    val body = try {
        parseCallSignaling(payload)
    } catch (_: Throwable) {
        return null
    }
    val signal = when (body) {
        is FfiCallSignalingBody.Offer -> me.lakko.app.calls.CallRingSignal.Offer(
            callIdHex = me.lakko.app.calls.callIdHexOf(body.callId),
            video = body.media.video,
            // The offer's own stamp, so the ring window is measured from when
            // the caller started ringing, not from when this device woke up.
            offeredAtMs = body.createdAtMs.toLong(),
            callerName = senderName,
        )
        is FfiCallSignalingBody.End ->
            me.lakko.app.calls.CallRingSignal.End(me.lakko.app.calls.callIdHexOf(body.callId))
        else -> return null
    }
    return ProcessedRow(
        kind = ProcessedKind.CALL_SIGNALING,
        text = "",
        attachmentCount = 0,
        senderName = senderName,
        callSignal = signal,
    )
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

/**
 * The AUTHORITATIVE committer-beacon check ([isServiceEnvelope]'s third
 * injected step) — the native, fail-closed `decodeCommitterBeacon`
 * (ADR-0045). It returns nothing on success, because the beacon is
 * FIELD-LESS: a clean return IS the whole verdict, "this is a service
 * envelope, render nothing". A wrong domain, a bad length, or an unknown
 * schema version throws and yields `false`, so the payload falls through
 * to the normal chat parse.
 *
 * READ-ONLY: a pure decode. The background path never EMITS a beacon and
 * never folds a committer verdict — a beacon means "able to sweep", and
 * it structurally cannot commit (C-59), so a beacon from here would be a
 * lie that costs the group a takeover.
 */
private fun decodesAsCommitterBeacon(bytes: ByteArray): Boolean =
    try {
        decodeCommitterBeacon(bytes)
        true
    } catch (_: Throwable) {
        false
    }

/**
 * The ADR-0045 beacon envelope length, read ONCE from the FFI constant
 * rather than hand-copied as `27` into this client — the drift shape
 * ADR-0044 was written about. `by lazy` keeps it off the per-row hot
 * path while still sourcing it from the one definition.
 *
 * A `Throwable` here (no `.so`, a stale binding) falls back to `0`,
 * which no payload can match, so the beacon arm goes inert and beacons
 * merely render as before — never a swallowed real message.
 */
private val committerBeaconEnvelopeLenCached: Int by lazy {
    try {
        committerBeaconEnvelopeLen().toInt()
    } catch (_: Throwable) {
        0
    }
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
