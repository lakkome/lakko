// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import android.content.Context
import java.io.File
import java.nio.ByteBuffer
import me.lakko.app.l10n.UiText

/*
 * ADR-0066 part 1 - "Was it you?" and taking the account back
 * (`docs/ux/identity-recovery.md` section 18.4).
 *
 * A device that learns the account's keys were reset elsewhere deletes
 * everything the account kept on it AT ONCE, except one small recall record.
 * With that record and the password the account had before the reset, the
 * person can take the account back for as long as the server's window is
 * open. The record goes, and the rest of the wipe runs, when the person says
 * "Yes, it was me", when a recall succeeds, when the server says the account
 * can no longer be taken back from this device, or when the window has
 * passed by this device's clock.
 *
 * Everything in this file is a pure decision or a store; the network and the
 * key-reset path live in [AuthClient] and [AuthRepository]. A host JVM test
 * runs all of it without a server or the native core.
 */

// ---------------------------------------------------------------------------
// The record
// ---------------------------------------------------------------------------

/**
 * What an old device keeps after the staged wipe. Nothing else of the
 * account survives: no phrase, no 64-byte phrase seed, no session, no chats.
 */
public class RecallRecord(
    /** The 16-byte account id. */
    public val accountId: ByteArray,
    /** The 32-byte Ed25519 seed of the OLD account identity: what signs the
     *  recall statements. Not the 64-byte phrase seed, not the phrase. */
    public val oldAccountSeed: ByteArray,
    /** The account's username for the screen; empty for a guest account. */
    public val username: String,
    /** When this device learned of the reset (unix ms). */
    public val learnedAtMs: Long,
) {
    public val accountIdB64: String get() = base64Encode(accountId)

    /** Overwrite the seed this instance holds. */
    public fun zero() {
        oldAccountSeed.fill(0)
    }

    override fun toString(): String = "RecallRecord(<redacted>)"
}

/** The record's layout: `version(1) | account_id(16) | seed(32) |
 *  learned_at_ms(8 BE) | username_len(2 BE) | username (UTF-8)`. */
public object RecallRecordCodec {
    public const val VERSION: Byte = 1
    public const val ACCOUNT_ID_LEN: Int = 16
    public const val SEED_LEN: Int = 32

    /** A username is at most 32 characters of `[a-z0-9_]`; the bound here is
     *  only what keeps a damaged record from allocating. */
    public const val MAX_USERNAME_BYTES: Int = 255
    private const val FIXED_LEN: Int = 1 + ACCOUNT_ID_LEN + SEED_LEN + 8 + 2

    public fun encode(record: RecallRecord): ByteArray {
        require(record.accountId.size == ACCOUNT_ID_LEN) { "account id must be $ACCOUNT_ID_LEN bytes" }
        require(record.oldAccountSeed.size == SEED_LEN) { "account seed must be $SEED_LEN bytes" }
        val name = record.username.toByteArray(Charsets.UTF_8)
        require(name.size <= MAX_USERNAME_BYTES) { "username too long" }
        return ByteBuffer.allocate(FIXED_LEN + name.size)
            .put(VERSION)
            .put(record.accountId)
            .put(record.oldAccountSeed)
            .putLong(record.learnedAtMs)
            .putShort(name.size.toShort())
            .put(name)
            .array()
    }

    /** `null` for anything that is not exactly one record. */
    public fun decode(bytes: ByteArray): RecallRecord? {
        if (bytes.size < FIXED_LEN || bytes[0] != VERSION) return null
        val buf = ByteBuffer.wrap(bytes, 1, bytes.size - 1)
        val accountId = ByteArray(ACCOUNT_ID_LEN).also { buf.get(it) }
        val seed = ByteArray(SEED_LEN).also { buf.get(it) }
        val learnedAtMs = buf.long
        val nameLen = buf.short.toInt() and 0xFFFF
        if (nameLen > MAX_USERNAME_BYTES || buf.remaining() != nameLen) {
            seed.fill(0)
            return null
        }
        val name = ByteArray(nameLen).also { buf.get(it) }
        return RecallRecord(accountId, seed, String(name, Charsets.UTF_8), learnedAtMs)
    }
}

/** Where the sealed record lives; the device implementation seals it under
 *  the hardware-backed device key. */
public interface RecallBlobStorage {
    /** The account ids (lower-case hex) that have a record. */
    public fun list(): List<String>

    /** The record's plaintext bytes, or `null` when absent or unreadable. */
    public fun read(accountIdHex: String): ByteArray?

    /** Store [plaintext]; throws when it could not be sealed or written. */
    public fun write(accountIdHex: String, plaintext: ByteArray)

    public fun delete(accountIdHex: String)
}

/**
 * The recall record of each account on this device (in practice at most
 * one: the "Was it you?" screen replaces the whole app until it is
 * answered).
 */
public class RecallRecordStore(private val storage: RecallBlobStorage) {

    /** Persist [record]; `false` when it could not be stored, in which case
     *  the caller runs the FULL wipe (nothing half-kept). */
    public fun save(record: RecallRecord): Boolean {
        val plain = try {
            RecallRecordCodec.encode(record)
        } catch (_: IllegalArgumentException) {
            return false
        }
        return try {
            storage.write(hexOf(record.accountId), plain)
            // Read back: a record that cannot be opened again is no record.
            val again = storage.read(hexOf(record.accountId))
            val ok = again != null && again.contentEquals(plain)
            again?.fill(0)
            if (!ok) storage.delete(hexOf(record.accountId))
            ok
        } catch (_: Throwable) {
            false
        } finally {
            plain.fill(0)
        }
    }

    /** The record of the account [accountIdB64] (standard base64). */
    public fun load(accountIdB64: String): RecallRecord? {
        val id = try {
            base64Decode(accountIdB64)
        } catch (_: IllegalArgumentException) {
            return null
        }
        return loadHex(hexOf(id))
    }

    /**
     * The one record this device holds, for the launch: a readable one, or
     * `null`. A row that cannot be opened (a wrap key the OS invalidated, a
     * damaged file) is deleted - it could never be used again.
     */
    public fun loadAny(): RecallRecord? {
        val ids = try {
            storage.list()
        } catch (_: Throwable) {
            emptyList()
        }
        for (hex in ids) {
            val record = loadHex(hex)
            if (record != null) return record
            try {
                storage.delete(hex)
            } catch (_: Throwable) {
                // Nothing to drop.
            }
        }
        return null
    }

    public fun delete(accountIdB64: String) {
        val id = try {
            base64Decode(accountIdB64)
        } catch (_: IllegalArgumentException) {
            return
        }
        try {
            storage.delete(hexOf(id))
        } catch (_: Throwable) {
            // Nothing to drop.
        }
    }

    private fun loadHex(hex: String): RecallRecord? {
        val plain = try {
            storage.read(hex)
        } catch (_: Throwable) {
            null
        } ?: return null
        try {
            val record = RecallRecordCodec.decode(plain) ?: return null
            // The file's name must be the account the record names.
            if (hexOf(record.accountId) != hex) {
                record.zero()
                return null
            }
            return record
        } finally {
            plain.fill(0)
        }
    }

    public companion object {
        /** Subdirectory of the no-backup files dir. The file is named by the
         *  account id in HEX on purpose: the wipe's backstop sweep deletes
         *  files named by the base64 spellings, and the staged wipe must
         *  leave this one. */
        public const val DIR: String = "lakko-recall"
        public const val FILE_SUFFIX: String = ".rec"

        public fun hexOf(bytes: ByteArray): String = bytes.joinToString("") { "%02x".format(it) }

        public fun onDevice(context: Context): RecallRecordStore =
            RecallRecordStore(DeviceSealedRecallStorage(context.applicationContext))

        /** Delete the record file of [accountIdB64] without opening it (the
         *  full wipe's step; needs no Keystore). */
        public fun deleteFile(context: Context, accountIdB64: String) {
            val id = try {
                base64Decode(accountIdB64)
            } catch (_: IllegalArgumentException) {
                return
            }
            File(File(context.noBackupFilesDir, DIR), hexOf(id) + FILE_SUFFIX).delete()
        }
    }
}

/**
 * The record on the device: sealed with the Keystore-resident wrap key
 * ([DeviceKeyStore], hardware-backed, never extractable - the protection the
 * device root key has) and written under `noBackupFilesDir`, so it is in no
 * backup and never travels to another device.
 */
internal class DeviceSealedRecallStorage(private val context: Context) : RecallBlobStorage {
    private fun dir(): File = File(context.noBackupFilesDir, RecallRecordStore.DIR)

    private fun file(accountIdHex: String): File = File(dir(), accountIdHex + RecallRecordStore.FILE_SUFFIX)

    override fun list(): List<String> =
        dir().listFiles().orEmpty()
            .filter { it.isFile && it.name.endsWith(RecallRecordStore.FILE_SUFFIX) }
            .map { it.name.removeSuffix(RecallRecordStore.FILE_SUFFIX) }
            .filter { name -> name.length == 2 * RecallRecordCodec.ACCOUNT_ID_LEN && name.all { it in HEX } }

    override fun read(accountIdHex: String): ByteArray? {
        val f = file(accountIdHex)
        if (!f.exists()) return null
        return DeviceKeyStore.openWithDeviceKey(f.readBytes())
    }

    override fun write(accountIdHex: String, plaintext: ByteArray) {
        val d = dir()
        if (!d.exists()) d.mkdirs()
        file(accountIdHex).writeBytes(DeviceKeyStore.sealWithDeviceKey(plaintext))
    }

    override fun delete(accountIdHex: String) {
        file(accountIdHex).delete()
    }

    private companion object {
        const val HEX: String = "0123456789abcdef"
    }
}

// ---------------------------------------------------------------------------
// Which wipe a refused session leads to
// ---------------------------------------------------------------------------

/** What a device deletes when its session is refused for good. */
public enum class SupersededWipe {
    /** Everything except the recall record; then "Was it you?". */
    StagedKeepingRecall,

    /** Everything; then the signed-out / removed screen with `Sign in`. */
    Full,
}

/**
 * The verdict (ADR-0066 section 1.1 and 2.3; UX 18.7 "three answers, three
 * screens, three deletions"):
 *
 *   * the keys were reset elsewhere AND this device holds the account's
 *     keys -> the staged wipe;
 *   * the keys were reset elsewhere and this device never held them -> the
 *     full wipe: it has nothing to prove itself with;
 *   * this device was REMOVED (`device-revoked`) -> the full wipe, never a
 *     recall record: a removal changes no keys, so there is nothing to take
 *     back;
 *   * the same-generation mismatch -> the full wipe on the person's tap.
 */
public fun supersededWipeFor(kind: KeyResetSignOut.Kind, holdsAccountKeys: Boolean): SupersededWipe = when (kind) {
    KeyResetSignOut.Kind.Reset ->
        if (holdsAccountKeys) SupersededWipe.StagedKeepingRecall else SupersededWipe.Full
    KeyResetSignOut.Kind.Removed, KeyResetSignOut.Kind.Mismatch -> SupersededWipe.Full
}

/** A record is used for at most this long after the device learned of the
 *  reset (the server's window is 30 days; the server is the authority, this
 *  is the bound for a device that never reaches it). */
public const val RECALL_RECORD_MAX_AGE_MS: Long = 30L * 24 * 60 * 60 * 1000

/**
 * Whether the window has passed by this device's clock: after the date the
 * server gave when it gave one, else 30 days after the device learned.
 */
public fun recallExpired(learnedAtMs: Long, serverExpiresAtMs: Long?, nowMs: Long): Boolean =
    if (serverExpiresAtMs != null) nowMs >= serverExpiresAtMs else nowMs - learnedAtMs >= RECALL_RECORD_MAX_AGE_MS

// ---------------------------------------------------------------------------
// The server's answers
// ---------------------------------------------------------------------------

/** `401` on the three recall routes for everything that is not a wrong
 *  password: one identical body. */
public const val RECALL_REFUSED: String = "recall-refused"

/** `401` on `finish` only. */
public const val RECALL_WRONG_PASSWORD: String = "wrong-password"

/** `409` on `DELETE /account` while a window is open. */
public const val RECALL_WINDOW_OPEN: String = "recall-window-open"

/** `POST /account/recall/status` 200. */
public data class RecallStatus(
    val anchorSeq: Long,
    /** Until when the account can be taken back (unix ms). */
    val expiresAtMs: Long,
    /** When the keys were reset (unix ms). */
    val changedAtMs: Long,
    /** Password attempts left before the server destroys the anchor. */
    val attemptsRemaining: Int,
    /** ADR-0067: the account the anchor belongs to (the answer names it for
     *  a device that asked by login handle). */
    val accountId: ByteArray? = null,
    /** ADR-0067 section 8: the account can be taken back with the recovery
     *  phrase it had before the reset (it had a saved phrase then). */
    val phraseArm: Boolean = false,
)

/** The old password did not open the anchor: noticed on this device (the
 *  OPAQUE login could not finish) or by the server (`401 wrong-password`). */
public class RecallWrongPassword : RuntimeException("the password before the reset did not match")

/** What a failed recall request means. */
public sealed interface RecallFailure {
    /** `401 recall-refused`: the account can no longer be taken back from
     *  this device. The record is destroyed and the wipe finishes. */
    public data object Refused : RecallFailure

    /** The old password is wrong. */
    public data object WrongPassword : RecallFailure

    /** `429`: wait [retryAfterSecs], then the same button. */
    public data class RateLimited(val retryAfterSecs: Long?) : RecallFailure

    /** No answer reached this device. */
    public data object Offline : RecallFailure

    /** Anything else (`400`, `5xx`, a malformed body). Never a reason to
     *  destroy the record. */
    public data object Other : RecallFailure
}

/** Classify a failed recall request. Only the exact `recall-refused` code
 *  ends the recall; an unlabelled `401` does not. */
public fun recallFailureOf(err: Throwable): RecallFailure = when {
    err is RecallWrongPassword -> RecallFailure.WrongPassword
    // A `finish` that was sent and not answered: it is asked again with the
    // same bytes; the record stays.
    err is RecallFinishUnanswered -> RecallFailure.Offline
    err is AuthNetworkError -> RecallFailure.Offline
    err is AuthHttpError && err.status == HTTP_UNAUTHORIZED && err.code == RECALL_REFUSED -> RecallFailure.Refused
    err is AuthHttpError && err.status == HTTP_UNAUTHORIZED && err.code == RECALL_WRONG_PASSWORD ->
        RecallFailure.WrongPassword
    err is AuthHttpError && err.status == HTTP_TOO_MANY -> RecallFailure.RateLimited(err.retryAfterSecs)
    else -> RecallFailure.Other
}

/** What one `status` request told this device. */
public sealed interface RecallStatusOutcome {
    public data class Open(val status: RecallStatus) : RecallStatusOutcome

    /** `recall-refused`. */
    public data object Refused : RecallStatusOutcome

    public data object Offline : RecallStatusOutcome

    /** A limit or a server fault: asked again later. */
    public data class Later(val retryAfterSecs: Long?) : RecallStatusOutcome
}

public fun recallStatusOutcomeOf(err: Throwable): RecallStatusOutcome = when (val failure = recallFailureOf(err)) {
    RecallFailure.Refused -> RecallStatusOutcome.Refused
    RecallFailure.Offline -> RecallStatusOutcome.Offline
    is RecallFailure.RateLimited -> RecallStatusOutcome.Later(failure.retryAfterSecs)
    RecallFailure.WrongPassword, RecallFailure.Other -> RecallStatusOutcome.Later(null)
}

/**
 * `DELETE /account` refused because the account can still be taken back:
 * the date until which it is blocked (unix ms), or `null` for any other
 * answer. [body] is the response body.
 */
public fun deleteBlockedUntilMs(status: Int, body: String?): Long? {
    if (status != HTTP_CONFLICT) return null
    if (me.lakko.app.anon.errorCodeOf(body) != RECALL_WINDOW_OPEN) return null
    return me.lakko.app.anon.longFieldOf(body, "until_ms")
}

// ---------------------------------------------------------------------------
// The screen's state machine
// ---------------------------------------------------------------------------

/** Whether this device has confirmed with the server that the account can
 *  be taken back. */
public sealed interface RecallAvailability {
    /** Asking (the first time, or again after a limit). */
    public data object Checking : RecallAvailability

    /** The server could not be reached; asked again when it can. */
    public data object Offline : RecallAvailability

    public data class Open(val status: RecallStatus) : RecallAvailability
}

public enum class RecallStage {
    /** "Was it you?" with the two buttons. */
    Question,

    /** The confirmation of "Yes, it was me", inside the same screen. */
    ConfirmYes,

    /** "Take the account back". */
    Form,
}

/** The one line under the form. */
public sealed interface RecallLine {
    /** [triesLeft] is set only when the server said fewer than five. */
    public data class WrongPassword(val triesLeft: Int?) : RecallLine

    public data class RateLimited(val untilMs: Long) : RecallLine

    public data object Offline : RecallLine

    public data object Failed : RecallLine
}

/** The screen. Driven by the recall record, so it survives a restart. */
public data class RecallUi(
    val accountIdB64: String,
    /** Empty for a guest account. */
    val username: String,
    val learnedAtMs: Long,
    val availability: RecallAvailability = RecallAvailability.Checking,
    val stage: RecallStage = RecallStage.Question,
    /** The recall is being sent. */
    val working: Boolean = false,
    val line: RecallLine? = null,
) {
    /** The date the body names: the server's when it said, else when this
     *  device learned. */
    val resetAtMs: Long
        get() = (availability as? RecallAvailability.Open)?.status?.changedAtMs ?: learnedAtMs

    /** The until-date, drawn only once the server confirmed. */
    val untilMs: Long?
        get() = (availability as? RecallAvailability.Open)?.status?.expiresAtMs
}

/** Why the recall ended without the account being taken back. */
public enum class RecallEnd {
    /** The person said "Yes, it was me". */
    ConfirmedOwn,

    /** Not possible (any more), learned on the question screen: the screen
     *  becomes the signed-out one and nothing says why. */
    NotPossible,

    /** Not possible any more, learned while the form was open. */
    Closed,

    /** No password attempts are left. */
    Exhausted,
}

/** The line the signed-out screen carries after [end], or `null`. */
public fun recallEndLine(end: RecallEnd): UiText? = when (end) {
    RecallEnd.ConfirmedOwn, RecallEnd.NotPossible -> null
    RecallEnd.Closed -> UiText("devices.recall.error.closed")
    RecallEnd.Exhausted -> UiText("devices.recall.error.exhausted")
}

/** One step of the machine: the screen stays (changed), or the recall ends. */
public sealed interface RecallStep {
    public data class Stay(val ui: RecallUi) : RecallStep

    public data class End(val reason: RecallEnd) : RecallStep
}

/** "No, take the account back" is enabled only once the server confirmed. */
public fun recallNoEnabled(ui: RecallUi): Boolean =
    ui.availability is RecallAvailability.Open && !ui.working

/** The tries-left line is drawn only below five (and above zero). */
public fun triesLeftToShow(attemptsRemaining: Int): Int? =
    attemptsRemaining.takeIf { it in 1 until TRIES_LEFT_SHOWN_BELOW }

public const val TRIES_LEFT_SHOWN_BELOW: Int = 5

/** The end a refusal leads to, by what the person was looking at. */
private fun endFor(stage: RecallStage, exhausted: Boolean): RecallEnd = when {
    stage != RecallStage.Form -> RecallEnd.NotPossible
    exhausted -> RecallEnd.Exhausted
    else -> RecallEnd.Closed
}

/**
 * Apply what a `status` request told this device.
 *
 *   * open with attempts left -> the until-date is drawn, "No" is enabled;
 *   * open with no attempt left, or refused -> the recall ends;
 *   * past the window by this device's clock -> the recall ends;
 *   * offline or a limit -> the screen stays; an availability the server
 *     already confirmed is kept (a failed re-check does not close the form).
 */
public fun recallAfterStatus(ui: RecallUi, outcome: RecallStatusOutcome, nowMs: Long): RecallStep {
    return when (outcome) {
        is RecallStatusOutcome.Open -> when {
            outcome.status.attemptsRemaining <= 0 -> RecallStep.End(endFor(ui.stage, exhausted = true))
            recallExpired(ui.learnedAtMs, outcome.status.expiresAtMs, nowMs) ->
                RecallStep.End(endFor(ui.stage, exhausted = false))
            else -> RecallStep.Stay(ui.copy(availability = RecallAvailability.Open(outcome.status)))
        }
        RecallStatusOutcome.Refused -> RecallStep.End(endFor(ui.stage, exhausted = false))
        RecallStatusOutcome.Offline, is RecallStatusOutcome.Later -> {
            val known = (ui.availability as? RecallAvailability.Open)?.status
            when {
                recallExpired(ui.learnedAtMs, known?.expiresAtMs, nowMs) ->
                    RecallStep.End(endFor(ui.stage, exhausted = false))
                known != null -> RecallStep.Stay(ui)
                outcome is RecallStatusOutcome.Offline ->
                    RecallStep.Stay(ui.copy(availability = RecallAvailability.Offline))
                else -> RecallStep.Stay(ui.copy(availability = RecallAvailability.Checking))
            }
        }
    }
}

/**
 * Apply a failed submit. [statusAfter] is the fresh `status` read after a
 * wrong password (`null` when it was not read): it carries the tries left.
 */
public fun recallAfterFailure(
    ui: RecallUi,
    failure: RecallFailure,
    statusAfter: RecallStatusOutcome?,
    nowMs: Long,
): RecallStep {
    val idle = ui.copy(working = false)
    return when (failure) {
        RecallFailure.Refused -> RecallStep.End(RecallEnd.Closed)
        RecallFailure.WrongPassword -> when (statusAfter) {
            is RecallStatusOutcome.Open -> when {
                statusAfter.status.attemptsRemaining <= 0 -> RecallStep.End(RecallEnd.Exhausted)
                else -> RecallStep.Stay(
                    idle.copy(
                        availability = RecallAvailability.Open(statusAfter.status),
                        line = RecallLine.WrongPassword(triesLeftToShow(statusAfter.status.attemptsRemaining)),
                    ),
                )
            }
            RecallStatusOutcome.Refused -> RecallStep.End(RecallEnd.Closed)
            RecallStatusOutcome.Offline, is RecallStatusOutcome.Later, null ->
                RecallStep.Stay(idle.copy(line = RecallLine.WrongPassword(null)))
        }
        is RecallFailure.RateLimited -> RecallStep.Stay(
            idle.copy(line = RecallLine.RateLimited(nowMs + (failure.retryAfterSecs ?: DEFAULT_RETRY_SECS) * 1000L)),
        )
        RecallFailure.Offline -> RecallStep.Stay(idle.copy(line = RecallLine.Offline))
        RecallFailure.Other -> RecallStep.Stay(idle.copy(line = RecallLine.Failed))
    }
}

/** The shortest password the sign-up form accepts. */
public const val RECALL_MIN_PASSWORD_LEN: Int = 8

/** What the form says before anything is sent, or `null` when it can be
 *  submitted: the sign-up password rule, and the two new fields match. */
public fun recallFormProblem(newPassword: String, repeat: String): UiText? = when {
    newPassword != repeat -> UiText("auth.password.error.mismatch")
    newPassword.length < RECALL_MIN_PASSWORD_LEN -> UiText(
        "auth.password.error.too_short",
        counts = mapOf("count" to RECALL_MIN_PASSWORD_LEN.toULong()),
    )
    else -> null
}

/** The main button: the three fields filled, the rule passed, the two new
 *  passwords equal, the server confirmed, nothing in flight, no wait. */
public fun recallSubmitEnabled(
    ui: RecallUi,
    oldPassword: String,
    newPassword: String,
    repeat: String,
    nowMs: Long,
): Boolean {
    if (!recallNoEnabled(ui)) return false
    if (oldPassword.isEmpty() || newPassword.isEmpty() || repeat.isEmpty()) return false
    if (recallFormProblem(newPassword, repeat) != null) return false
    val wait = ui.line as? RecallLine.RateLimited
    return wait == null || nowMs >= wait.untilMs
}

/** How long to wait before asking `status` again. */
public fun recallRecheckDelayMs(outcome: RecallStatusOutcome): Long = when (outcome) {
    is RecallStatusOutcome.Open -> RECHECK_OPEN_MS
    RecallStatusOutcome.Offline -> RECHECK_OFFLINE_MS
    is RecallStatusOutcome.Later -> (outcome.retryAfterSecs ?: DEFAULT_RETRY_SECS) * 1000L
    RecallStatusOutcome.Refused -> Long.MAX_VALUE
}

/** While a submit is in flight the periodic check stands aside. */
public const val RECALL_BUSY_RECHECK_MS: Long = 5_000L

/** Chosen, not measured. */
private const val RECHECK_OPEN_MS: Long = 10L * 60 * 1000
private const val RECHECK_OFFLINE_MS: Long = 10_000L
private const val DEFAULT_RETRY_SECS: Long = 60L

private const val HTTP_UNAUTHORIZED: Int = 401
private const val HTTP_CONFLICT: Int = 409
private const val HTTP_TOO_MANY: Int = 429

/**
 * ADR-0067 section 8 / UX 19.7 - whether the 18.4 form offers `Use my
 * recovery phrase instead`: only once the server said the phrase arm is
 * open (the account had a saved phrase before the reset).
 */
public fun recallOffersPhrase(ui: RecallUi): Boolean =
    (ui.availability as? RecallAvailability.Open)?.status?.phraseArm == true
