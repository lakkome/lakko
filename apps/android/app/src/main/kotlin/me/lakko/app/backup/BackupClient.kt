// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.backup

import android.content.Context
import android.util.Log
import me.lakko.app.storage.StorageClient
import uniffi.lakko_ffi_uniffi.BackupContactRecord
import uniffi.lakko_ffi_uniffi.Identity
import uniffi.lakko_ffi_uniffi.BackupOwnPredecessor
import uniffi.lakko_ffi_uniffi.ParsedBackupPayload
import uniffi.lakko_ffi_uniffi.backupEnvelopeHash
import uniffi.lakko_ffi_uniffi.buildBackupPayload
import uniffi.lakko_ffi_uniffi.frameBackupBlob
import uniffi.lakko_ffi_uniffi.openBackup
import uniffi.lakko_ffi_uniffi.parseBackupPayload
import uniffi.lakko_ffi_uniffi.sealBackup

/**
 * `BackupClient` — orchestrates the encrypted-backup upload and
 * restore flows on Android (Phase 2a1c).
 *
 * Mirror of:
 *
 *   * `apps/web/src/backup/client.ts`
 *   * `apps/ios/Lakko/Backup/BackupClient.swift`
 *
 * Glues four layers:
 *
 *   1. [PhraseStore] — `EncryptedSharedPreferences`-backed
 *      recovery-phrase cache.
 *   2. UniFFI surface for `lakko-backup` — `buildBackupPayload`,
 *      `sealBackup`, `openBackup`, `parseBackupPayload`.
 *   3. [StorageClient] — the three `/backup` HTTP routes.
 *   4. Identity material from [`Identity.fromSeed`] — the
 *      Ed25519 signing seed that lands inside the payload.
 *
 * The backup carries NO MLS provider state (maintainer decision,
 * 2026-10-02; payload wire v6): contacts, the ADR-0058 synced state and
 * the ids of the account's conversations. A provider snapshot belongs to
 * the device that made it, so none is composed here and none is read back
 * (see `BackupRestore.kt`).
 */
public class BackupClient(
    private val storage: StorageClient = StorageClient(),
) : AutoCloseable {

    override fun close() {
        storage.close()
    }

    /** Bearer token for the storage `/backup` routes AND the
     *  `/auth/backup/head` mint (ADR-0037). Set via [setSessionToken]. */
    private var sessionToken: String? = null

    /** Bind / rebind the bearer token for the `/backup` routes AND the
     *  `/auth/backup/head` mint (ADR-0037). */
    public fun setSessionToken(token: String?) {
        this.sessionToken = token
        storage.setSessionToken(token)
    }

    // -----------------------------------------------------------------
    // Upload
    // -----------------------------------------------------------------

    /** Compose + seal + PUT in one step. Caller has the phrase
     *  on hand right now (signup just minted it, recover just
     *  consumed it). No phrase-cache hop. */
    public suspend fun uploadNow(
        accountId: ByteArray,
        ed25519Seed: ByteArray,
        phraseBytes: ByteArray,
        contacts: List<BackupContactRecord> = emptyList(),
        /** ADR-0037: Android context + `accountIdB64` for the version-floor
         *  store. When both are present, the auth-assigned `backup_version`
         *  floor is persisted after a successful head mint. Omitted → no floor
         *  persisted (the mint + framing still happen; a future restore falls
         *  back to the freshness window). */
        context: Context? = null,
        accountIdB64: String? = null,
        /** The MLS `group_id`s of the account's conversations, hex: the groups
         *  this device holds plus the ones a restored backup named that it has
         *  not joined yet ([BackupRestoreStore.backupGroupIds]). Names only -
         *  a restored device is admitted by the other members. */
        knownGroupIdsHex: List<String> = emptyList(),
        /** ADR-0058 section 5.2 — the folded synced state's bytes, or `null`. */
        syncedState: ByteArray? = null,
        /** ADR-0074 section 3.7 — the account's own predecessors, oldest
         *  first. The caller owns the seeds and zeroes them. */
        ownPredecessors: List<BackupOwnPredecessor> = emptyList(),
        /** The own name, photo and the visibility setting. */
        personal: BackupPersonal = BackupPersonal.NONE,
    ) {
        val createdAtMs = System.currentTimeMillis().toULong()
        val payload = buildBackupPayload(
            accountId = accountId,
            createdAtMs = createdAtMs,
            ed25519Seed = ed25519Seed,
            contacts = contacts,
            // A row that is not a group id is dropped rather than failing the
            // backup: one malformed local entry must not cost the whole
            // envelope.
            knownGroupIds = knownGroupIdsHex.mapNotNull(::groupIdBytesOrNull),
            syncedState = syncedState?.takeIf { it.isNotEmpty() },
            ownPredecessors = ownPredecessors.takeIf { it.isNotEmpty() },
            ownProfile = personal.ownProfile,
            visibility = personal.visibility,
            receivedProfiles = personal.receivedProfiles.takeIf { it.isNotEmpty() },
        )
        val envelope = sealBackup(phrase = phraseBytes, payloadBytes = payload)
        // ADR-0037: mint the auth-signed head, frame `TAG ‖ head ‖ envelope`,
        // and PUT the framed blob. Soft-fail — a mint failure writes the legacy
        // unframed envelope rather than failing the whole backup.
        val (blob, floor) = frameWithHead(envelope)
        storage.putBackup(blob)
        // Persist the floor ONLY on a successful mint (ADR-0037 §6) — this is
        // the monotonic anchor a future returning-client restore refuses to go
        // below. `putFloor` never lowers an existing floor.
        if (floor != null && context != null && accountIdB64 != null) {
            try {
                FloorStore.putFloor(context, accountIdB64, floor)
            } catch (cause: Throwable) {
                Log.w("lakko-backup", "floor persist failed", cause)
            }
        }
    }

    /**
     * Mint the auth-signed backup head for `envelope`, then frame
     * `TAG ‖ head ‖ envelope` (ADR-0037 §5). Soft-fail: a mint / framing failure
     * (network, 5xx, no session token) falls back to the UNFRAMED legacy
     * envelope so a backup never fails because the head could not be minted —
     * the mirror image of the restore-side fail-open on an absent head. A `401`
     * from auth ([BackupHeadSessionRefused]) is not such a failure: it stops
     * the upload before the PUT.
     */
    private suspend fun frameWithHead(envelope: ByteArray): Pair<ByteArray, BackupFloor?> {
        val headClient = BackupHeadClient()
        return try {
            val hash = backupEnvelopeHash(envelope)
            val minted = headClient.mint(sessionToken, hash)
            val blob = frameBackupBlob(minted.head, envelope)
            blob to BackupFloor(version = minted.version, envelopeHash = hash)
        } catch (cause: Throwable) {
            if (cause is kotlinx.coroutines.CancellationException) throw cause
            // Auth refused the session: nothing is written (a superseded or
            // removed device must not overwrite the account's backup).
            if (!headMintFailureAllowsLegacy(cause)) {
                Log.w("lakko-backup", "backup-head mint refused the session; backup not written")
                throw cause
            }
            Log.w(
                "lakko-backup",
                "backup-head mint failed, writing legacy envelope",
                cause,
            )
            envelope to null
        } finally {
            headClient.close()
        }
    }

    /** Read the phrase from the [PhraseStore] cache and run
     *  [uploadNow] with it. Returns a discriminated
     *  [UploadResult] so the orchestration layer can branch on
     *  `CacheMiss` (the user needs to run recovery to enable
     *  backups on this device). */
    public suspend fun uploadFromCache(
        context: Context,
        accountId: ByteArray,
        accountIdB64: String,
        ed25519Seed: ByteArray,
        contacts: List<BackupContactRecord> = emptyList(),
        knownGroupIdsHex: List<String> = emptyList(),
        syncedState: ByteArray? = null,
        ownPredecessors: List<BackupOwnPredecessor> = emptyList(),
        personal: BackupPersonal = BackupPersonal.NONE,
    ): UploadResult {
        val cached = PhraseStore.getPhrase(context, accountIdB64)
            ?: return UploadResult.CacheMiss
        try {
            uploadNow(
                accountId = accountId,
                ed25519Seed = ed25519Seed,
                phraseBytes = cached,
                contacts = contacts,
                // ADR-0037: mint the head + advance the monotonic version floor.
                context = context,
                accountIdB64 = accountIdB64,
                knownGroupIdsHex = knownGroupIdsHex,
                syncedState = syncedState,
                ownPredecessors = ownPredecessors,
                personal = personal,
            )
            return UploadResult.Uploaded
        } catch (cause: Throwable) {
            return UploadResult.TransportFailed(cause)
        } finally {
            // Best-effort scrub of the local copy. The
            // EncryptedSharedPreferences row stays intact —
            // future auto-backups can re-read.
            cached.fill(0)
        }
    }

    // -----------------------------------------------------------------
    // Restore
    // -----------------------------------------------------------------

    /**
     * Fetch + verify + decrypt + parse the server-stored backup with the
     * ADR-0037 §6 rollback gate armed (G-11). Mirror of the web
     * `BackupClient.restore(phrase, gate)`.
     *
     * Peels `TAG ‖ head` and applies the three-case policy BEFORE opening the
     * envelope:
     *   * [RestoreVerdict.HardFail] → [GatedRestoreOutcome.RollbackBlocked]
     *     (`hard = true`): refuse + alarm, no override (invariant #4).
     *   * [RestoreVerdict.StaleOverride] → ask the user via
     *     [RestoreGateContext.confirmStaleOverride]; a decline returns
     *     [GatedRestoreOutcome.RollbackBlocked] (`hard = false`), never a silent
     *     accept.
     *   * [RestoreVerdict.FailOpen] / [RestoreVerdict.Proceed] → open the
     *     envelope; a clean proceed advances the monotone floor.
     */
    public suspend fun restore(
        context: Context,
        phraseBytes: ByteArray,
        gate: RestoreGateContext,
    ): GatedRestoreOutcome {
        val blob = try {
            storage.getBackup()
        } catch (cause: Throwable) {
            return GatedRestoreOutcome.TransportFailed(cause)
        } ?: return GatedRestoreOutcome.NoBackup

        val evaluation = evaluateRestoreBlob(
            context = context,
            blob = blob,
            accountId = gate.accountId,
            accountIdB64 = gate.accountIdB64,
            nowMillis = gate.nowMillis,
            maxStalenessMillis = gate.maxStalenessMillis,
        )

        var cleanProceed: BackupFloor? = null
        val envelope: ByteArray = when (val verdict = evaluation.verdict) {
            is RestoreVerdict.HardFail ->
                // Refuse + alarm, no override (invariant #4 — no silent downgrade).
                return GatedRestoreOutcome.RollbackBlocked(hard = true, reason = verdict.reason)
            is RestoreVerdict.StaleOverride -> {
                // Never silent-accept a stale head — require an explicit user ack.
                val accepted = gate.confirmStaleOverride(verdict.reason)
                if (!accepted) {
                    return GatedRestoreOutcome.RollbackBlocked(
                        hard = false,
                        reason = verdict.reason,
                    )
                }
                // Accepted: proceed with the (older) envelope, but DO NOT advance
                // the floor down — `putFloor` is monotone, so a deliberate
                // downgrade never lowers this device's future protection.
                evaluation.envelope
            }
            is RestoreVerdict.FailOpen -> evaluation.envelope
            is RestoreVerdict.Proceed -> {
                val v = evaluation.verifiedVersion
                val h = evaluation.envelopeHash
                if (v != null && h != null) {
                    cleanProceed = BackupFloor(version = v, envelopeHash = h)
                }
                evaluation.envelope
            }
        }

        val parsed: ParsedBackupPayload = try {
            val payloadBytes = openBackup(phrase = phraseBytes, wire = envelope)
            parseBackupPayload(payloadBytes)
        } catch (cause: Throwable) {
            // Wrong phrase or tampered blob — folded into one type by the
            // UniFFI surface, mirroring the Rust-side `BackupError` fold.
            Log.w("lakko-backup", "openBackup failed", cause)
            return GatedRestoreOutcome.WrongPhraseOrTampered
        }

        // ADR-0037 §6: on a clean verified proceed, advance the monotone floor so
        // subsequent restores on this device are hard-protected (returning-client
        // path). Skipped on fail-open (no verified head) and stale-override
        // (never lower the floor).
        if (cleanProceed != null) {
            try {
                FloorStore.putFloor(context, gate.accountIdB64, cleanProceed)
            } catch (cause: Throwable) {
                Log.w("lakko-backup", "floor persist (restore) failed", cause)
            }
        }

        return GatedRestoreOutcome.Restored(parsed)
    }
}

/**
 * What a backup carries about the person that no other part of it does: the own
 * profile envelope and the visibility envelope, both with their original stamps
 * ([ownProfile] and [visibility] `null` when never set). Opaque here; a restore
 * decodes them and fails closed.
 */
public data class BackupPersonal(
    val ownProfile: ByteArray?,
    val visibility: ByteArray?,
    /** ADR-0048 / `lakko-backup` v8 — the names this device learned of
     *  other people ([receivedProfilesOnDevice]), so a restore with no other
     *  device alive names them at once. */
    val receivedProfiles: List<uniffi.lakko_ffi_uniffi.BackupReceivedProfile> = emptyList(),
) {
    /** A short value that changes with any envelope. */
    public fun changeKey(): String {
        val digest = java.security.MessageDigest.getInstance("SHA-256")
        ownProfile?.let(digest::update)
        digest.update(0)
        visibility?.let(digest::update)
        for (r in receivedProfiles) {
            digest.update(1)
            digest.update(r.personKey)
            digest.update(r.envelope)
        }
        return digest.digest().joinToString("") { "%02x".format(it) }
    }

    override fun equals(other: Any?): Boolean =
        other is BackupPersonal &&
            ownProfile.contentEqualsOrNull(other.ownProfile) &&
            visibility.contentEqualsOrNull(other.visibility) &&
            receivedProfiles.size == other.receivedProfiles.size &&
            receivedProfiles.indices.all { i ->
                receivedProfiles[i].personKey.contentEquals(other.receivedProfiles[i].personKey) &&
                    receivedProfiles[i].envelope.contentEquals(other.receivedProfiles[i].envelope)
            }

    override fun hashCode(): Int =
        ((ownProfile?.contentHashCode() ?: 0) * 31 + (visibility?.contentHashCode() ?: 0)) * 31 +
            receivedProfiles.sumOf { it.personKey.contentHashCode() }

    public companion object {
        public val NONE: BackupPersonal = BackupPersonal(null, null)

        /** Read from this device's stores. A store that cannot be read gives
         *  `null`, and that field is left out of this backup. */
        public fun of(context: Context, accountIdB64: String): BackupPersonal {
            val ownProfile = runCatching {
                val accountId = me.lakko.app.auth.base64Decode(accountIdB64)
                me.lakko.app.profile.ownProfileBackupEnvelope(
                    me.lakko.app.profile.OwnProfileStore.load(context, accountId),
                    accountId,
                ) { me.lakko.app.profile.ProfileSigner.compose(it, accountId) }
            }.getOrNull()
            val visibility = runCatching {
                me.lakko.app.reach.ReachManager.get(context).visibilityEnvelopeFor(accountIdB64)
            }.getOrNull()
            val received = runCatching { receivedProfilesOnDevice(context, accountIdB64) }.getOrDefault(emptyList())
            return BackupPersonal(ownProfile, visibility, received)
        }

        private fun ByteArray?.contentEqualsOrNull(other: ByteArray?): Boolean =
            if (this == null) other == null else other != null && contentEquals(other)
    }
}

/** Discriminated outcome of [BackupClient.uploadFromCache]. */
public sealed interface UploadResult {
    public data object Uploaded : UploadResult
    public data object CacheMiss : UploadResult
    public data class TransportFailed(val cause: Throwable) : UploadResult
}

/**
 * Context the restore seam threads into the gated [BackupClient.restore] to arm
 * the ADR-0037 §6 rollback gate (G-11). Mirror of the web `RestoreGateContext`.
 */
public class RestoreGateContext(
    /** The 16-byte account_id the client OPAQUE-authenticated as (head binding). */
    val accountId: ByteArray,
    /** Base64 account id — the floor-store key. */
    val accountIdB64: String,
    /** Injectable clock in ms (default now). */
    val nowMillis: Long = System.currentTimeMillis(),
    /** Injectable staleness window in ms (default from the build-config knob). */
    val maxStalenessMillis: Long = backupHeadStalenessMillis(),
    /** Blocking confirmation for a stale head — MUST NOT auto-accept. Returns
     *  `true` only when the user explicitly acknowledges the deliberate
     *  downgrade; a decline / no-UI path returns `false` (fail-closed — never a
     *  silent downgrade). */
    val confirmStaleOverride: suspend (reason: RestoreReason) -> Boolean,
)

/** Discriminated outcome of the gated [BackupClient.restore] (ADR-0037 §6). */
public sealed interface GatedRestoreOutcome {
    /** Verified (or fail-open / accepted-override) — the parsed payload. */
    public data class Restored(val payload: ParsedBackupPayload) : GatedRestoreOutcome

    /** No blob has been uploaded yet. */
    public data object NoBackup : GatedRestoreOutcome

    /** Wrong recovery phrase or a tampered envelope (AEAD-open failed). */
    public data object WrongPhraseOrTampered : GatedRestoreOutcome

    /** The rollback gate refused the served blob. `hard = true` = an
     *  unambiguous tamper / corruption (no override); `hard = false` = a stale
     *  head the user was offered and DECLINED. Either way no restore happened —
     *  the caller alarms (hard) or notes (declined). */
    public data class RollbackBlocked(val hard: Boolean, val reason: RestoreReason) : GatedRestoreOutcome

    /** Transport failure fetching the blob. */
    public data class TransportFailed(val cause: Throwable) : GatedRestoreOutcome
}

/** Derive the AccountIdentity Ed25519 signing seed from the
 *  identity seed (ADR-0017: the BIP39 phrase seed, NOT the password
 *  exportKey — so the backup is signed with the stable account
 *  identity that survives a password change, matching web/iOS).
 *  Uses the same `Identity.fromSeed` deterministic flow every
 *  identity-bearing operation uses. Helper so the auth-side bootstrap
 *  can skip threading the seed through the public surface. */
public fun ed25519SeedFromIdentitySeed(identitySeed: ByteArray): ByteArray {
    return me.lakko.app.auth.SessionSecretWipe.identity(identitySeed).use { it.ed25519Seed() }
}

/**
 * Decode one hex group id, or `null` when it is not one.
 *
 * Lakko mints 32-byte ids; anything else names no conversation, so carrying it
 * would only put a name nobody can act on into the backup.
 */
/**
 * ADR-0074 section 3.7: the account's own predecessors as a backup carries
 * them, oldest first, each with the time the account adopted its successor.
 * Fresh copies of the seeds: the caller zeroes them and [rows] alike.
 */
internal fun ownPredecessorsForBackup(rows: List<me.lakko.app.auth.OwnPredecessor>): List<BackupOwnPredecessor> =
    rows.map { BackupOwnPredecessor(seed = it.phraseSeed.copyOf(), adoptedAtMs = it.supersededAtMs.toULong()) }

internal fun groupIdBytesOrNull(hex: String): ByteArray? {
    if (hex.length != 64) return null
    val out = ByteArray(32)
    for (i in 0 until 32) {
        val byte = hex.substring(i * 2, i * 2 + 2).toIntOrNull(16) ?: return null
        out[i] = byte.toByte()
    }
    return out
}
