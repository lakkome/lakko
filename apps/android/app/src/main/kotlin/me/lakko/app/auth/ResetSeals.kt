// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import android.content.Context
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json

/*
 * ADR-0101 section 10 - the reset seal on this account's own side: the seal
 * announcement the account publishes under its key, and the reset seal a
 * resetting device signs. A peer moves an account's conversations, history,
 * profile and channel subscriptions only for a reset signed by a seal key
 * the account announced BEFORE the reset; the server holds none of the seal
 * secrets (the password's export key, a passkey's PRF output, the phrase).
 *
 * The codec, the keys and the signatures are the core's (`reset_seal.rs` on
 * the UniFFI surface), reached through [SealCore] so the host suite, which
 * has no native library, runs every rule here against a fake.
 *
 * What "a phrase is saved" means since 2026-11-26 (ADR-0075, amended): the
 * account's current announcement lists a phrase seal. The server keeps no
 * flag.
 */

/** What a seal key comes from. The wire bytes are the core's. */
public enum class SealKindName { PASSWORD, PASSKEY, PHRASE }

/** One announced seal key. [tagHex] is the passkey's tag (32 hex
 *  characters), all zero for a password or phrase seal. */
@Serializable
public data class SealEntryView(val kind: SealKindName, val tagHex: String, val sealPkB64: String)

/** A decoded announcement, as far as this layer reads it. */
@Serializable
public data class SealAnnouncementView(
    val keyGeneration: Long,
    val seq: Long,
    val entries: List<SealEntryView>,
    /** When the announcing device signed it (its clock), unix milliseconds;
     *  0 when not known. */
    val issuedAtMs: Long = 0L,
) {
    val password: SealEntryView? get() = entries.firstOrNull { it.kind == SealKindName.PASSWORD }
    val phrase: SealEntryView? get() = entries.firstOrNull { it.kind == SealKindName.PHRASE }
    val passkeys: List<SealEntryView> get() = entries.filter { it.kind == SealKindName.PASSKEY }

    /** What would bring the chats back after a reset (UX 19.1c, 19.1d). */
    val keptBy: ChatsKeptBy get() = chatsKeptBy(phraseSeal = phrase != null, passkeySeal = passkeys.isNotEmpty())
}

/** The all-zero tag of a password or phrase entry. */
public const val SEAL_TAG_NONE_HEX: String = "00000000000000000000000000000000"

/** At most this many passkey entries in one announcement (the core's bound). */
public const val SEAL_MAX_PASSKEYS: Int = 8

/** What this device can put into the account's announcement now. */
public data class SealMaterial(
    /** The password seal key of the export key this device holds, or `null`
     *  when it holds none; [passwordSealToAssert] decides whether it is put
     *  forward, else the announced one is carried. */
    val passwordPkB64: String?,
    /** The phrase seal key, only when the account's phrase is saved AND this
     *  device holds the words. */
    val phrasePkB64: String?,
    /** Passkey seals made on this device just now (a passkey was added). */
    val newPasskeys: List<SealEntryView> = emptyList(),
    /** The tags of the account's passkeys now; `null` when the list could
     *  not be read (every announced passkey seal is then kept). */
    val livePasskeyTagsHex: Set<String>? = null,
    /** Whether a password ceremony produced the export key behind
     *  [passwordPkB64] in this run (never a cached key). */
    val passwordFromThisRun: Boolean = false,
)

/**
 * The entries the account's announcement should hold, from what it holds now
 * ([current], the CURRENT generation's, or `null` when there is none) and
 * what this device can contribute:
 *
 *   - exactly one password seal: this device's when it holds the export key
 *     (a password it just signed in with or changed), else the announced one.
 *     With neither, nothing can be announced yet (`null`): a device that
 *     never held the password waits for one that did;
 *   - the phrase seal once saved: this device's, else the announced one (a
 *     saved phrase is never un-saved);
 *   - the passkey seals already announced whose passkey still exists, plus
 *     the new ones (a new seal replaces an announced one of the same tag),
 *     at most [SEAL_MAX_PASSKEYS], the new ones first.
 */
public fun desiredSealEntries(current: SealAnnouncementView?, material: SealMaterial): List<SealEntryView>? {
    val passwordPk = material.passwordPkB64 ?: current?.password?.sealPkB64 ?: return null
    val phrasePk = material.phrasePkB64 ?: current?.phrase?.sealPkB64
    val newTags = material.newPasskeys.map { it.tagHex }.toSet()
    val carried = current?.passkeys.orEmpty().filter { entry ->
        entry.tagHex !in newTags && (material.livePasskeyTagsHex == null || entry.tagHex in material.livePasskeyTagsHex)
    }
    val passkeys = (material.newPasskeys.distinctBy { it.tagHex } + carried).take(SEAL_MAX_PASSKEYS)
    return buildList {
        add(SealEntryView(SealKindName.PASSWORD, SEAL_TAG_NONE_HEX, passwordPk))
        addAll(passkeys.sortedBy { it.tagHex })
        if (phrasePk != null) add(SealEntryView(SealKindName.PHRASE, SEAL_TAG_NONE_HEX, phrasePk))
    }
}

/**
 * Which password seal this device puts forward (the web rule, aligned
 * 2026-11-27): its own export key's only when a password ceremony produced
 * that key in this run ([fromThisRun]: sign-up, a password sign-in, a
 * password change on this device), or when it is the announced one already.
 * A cached export key from an earlier run may be a previous password's (the
 * password may have been changed on another device since), so a plain
 * landing only carries the announced seal forward, and with none announced
 * it waits for a password sign-in.
 */
public fun passwordSealToAssert(
    current: SealAnnouncementView?,
    ownPasswordPkB64: String?,
    fromThisRun: Boolean,
): String? = when {
    ownPasswordPkB64 == null -> null
    fromThisRun -> ownPasswordPkB64
    current?.password?.sealPkB64 == ownPasswordPkB64 -> ownPasswordPkB64
    else -> null
}

/** Whether the account's announcement must be published again: there is
 *  none for [keyGeneration], or it does not list exactly [desired]. */
public fun announcementDue(current: SealAnnouncementView?, desired: List<SealEntryView>, keyGeneration: Long): Boolean =
    current == null || current.keyGeneration != keyGeneration || current.entries.toSet() != desired.toSet()

/** The next announcement number: above every one this device knows of
 *  (`seq` strictly increases per account, across generations). */
public fun nextSealSeq(vararg known: Long?): Long = (known.filterNotNull().maxOrNull() ?: 0L) + 1L

// ---------------------------------------------------------------------------
// Will this reset bring things back? (UX 15.1c)
// ---------------------------------------------------------------------------

/** A seal key the resetting device can sign with. */
public data class SealCandidate(val entry: SealEntryView)

/** What the device knows before the person types the word. */
public sealed interface SealPrediction {
    /** One of the device's seal keys is announced: the reset is sealed and
     *  the chats come back. */
    public data class Sealed(val entry: SealEntryView, val announcementSeq: Long) : SealPrediction

    /**
     * None is: the reset starts the account over. [passkeyCouldSeal] when
     * the account announced a passkey seal the device did not use: "Use a
     * passkey that keeps your chats to bring them back."
     */
    public data class Unsealed(val passkeyCouldSeal: Boolean) : SealPrediction
}

/**
 * Whether a reset made now would be sealed: the first of [candidates] (the
 * caller's order: password, passkey, phrase) that the account's current
 * announcement lists under its kind, tag and key. Asked of the directory's
 * copy of the announcement, never of the server ("will this be sealed?" is
 * not a question the server can answer).
 */
public fun predictSeal(current: SealAnnouncementView?, candidates: List<SealCandidate>): SealPrediction {
    val listed = current?.entries.orEmpty().toSet()
    val hit = candidates.firstOrNull { it.entry in listed }
    if (hit != null && current != null) return SealPrediction.Sealed(hit.entry, current.seq)
    val usedPasskeyTags = candidates.filter { it.entry.kind == SealKindName.PASSKEY }.map { it.entry.tagHex }.toSet()
    return SealPrediction.Unsealed(
        passkeyCouldSeal = current?.passkeys.orEmpty().any { it.tagHex !in usedPasskeyTags },
    )
}

/** The catalog key of the line between the confirmed factor and the word. */
public fun sealPredictionKey(prediction: SealPrediction): String = when (prediction) {
    is SealPrediction.Sealed -> "restore_identity.reset.sealed"
    is SealPrediction.Unsealed -> "restore_identity.reset.unsealed"
}

// ---------------------------------------------------------------------------
// The core, behind a port
// ---------------------------------------------------------------------------

/** The secret a seal key is made from. Never logged. */
public sealed interface SealSecret {
    public class Password(public val exportKey: ByteArray) : SealSecret
    public class Passkey(public val prfOutput: ByteArray, public val credentialId: ByteArray) : SealSecret
    public class Phrase(public val phrase: String) : SealSecret

    public fun wipe() {
        when (this) {
            is Password -> exportKey.fill(0)
            is Passkey -> prfOutput.fill(0)
            is Phrase -> Unit
        }
    }
}

/** The core's seal functions (`reset_seal.rs`). */
public interface SealCore {
    /** The announcement entry of [secret]'s seal key. */
    public fun entryOf(secret: SealSecret): SealEntryView

    /** A passkey's tag in the announcement (32 hex characters). */
    public fun passkeyTagHex(credentialId: ByteArray): String

    /** Decode [announcement] and verify it under [accountKey]; `null` when
     *  it does not decode or verify. */
    public fun open(announcement: ByteArray, accountKey: ByteArray): SealAnnouncementView?

    /** Decode [announcement] without checking its signature (for its
     *  number only); `null` when it does not decode. */
    public fun decodeUnverified(announcement: ByteArray): SealAnnouncementView?

    /** Sign the announcement with the account identity of [accountSeed]. */
    public fun signAnnouncement(
        accountSeed: ByteArray,
        accountId: ByteArray,
        keyGeneration: Long,
        seq: Long,
        issuedAtMs: Long,
        entries: List<SealEntryView>,
    ): ByteArray

    /** Sign the reset seal with [secret]'s key: the account leaves
     *  [fromGeneration] for the next with [newSignaturePubkey], under the
     *  announcement numbered [announcementSeq]. */
    public fun signResetSeal(
        secret: SealSecret,
        accountId: ByteArray,
        announcementSeq: Long,
        fromGeneration: Long,
        newSignaturePubkey: ByteArray,
    ): ByteArray
}

// ---------------------------------------------------------------------------
// Publishing the announcement
// ---------------------------------------------------------------------------

/** The account's own row at the directory, as far as the seals go. */
public class OwnSealBundle(
    public val signaturePubkey: ByteArray,
    public val keyGeneration: Long,
    public val resetSeals: ByteArray?,
    public val previousResetSeals: ByteArray?,
)

/** Discovery's refusal of an announcement numbered at or below one the
 *  account's row has held. */
public const val RESET_SEALS_STALE: String = "reset-seals-stale"

/** Its status. */
public const val SEAL_HTTP_CONFLICT: Int = 409

/** The directory refused an announcement number it already passed (`409
 *  reset-seals-stale`): read the row again and number above it. */
public class SealAnnouncementStale : Exception("the announcement number is not above the directory's")

/** What the announcer needs of the directory. */
public interface SealDirectoryPorts {
    /** The account's own row, `null` when it has none yet. Throws when the
     *  directory could not be asked. */
    public suspend fun fetchOwn(): OwnSealBundle?

    /** Upload [announcement] with the account's identity bundle. Throws
     *  [SealAnnouncementStale] on `409 reset-seals-stale`. */
    public suspend fun upload(announcement: ByteArray)
}

/** Who the announcement is for and signed by. */
public class SealAccount(
    public val accountId: ByteArray,
    /** The account identity's Ed25519 public key this device holds. */
    public val accountKey: ByteArray,
    /** The seed the account identity is derived from. */
    public val accountSeed: ByteArray,
    /** The generation of this device's session pair. */
    public val keyGeneration: Long,
)

/** How one announce attempt ended. */
public sealed interface AnnounceOutcome {
    /** The directory already holds exactly this: nothing was sent. */
    public data class Current(val view: SealAnnouncementView) : AnnounceOutcome

    /** A new announcement landed. */
    public data class Published(val view: SealAnnouncementView) : AnnounceOutcome

    /** This device holds no password seal key and none is announced: it
     *  waits for a password sign-in. */
    public data object NoPasswordSeal : AnnounceOutcome

    /** The directory's key or generation is not this device's (a reset or a
     *  rotation it has not caught up with, or no row yet): the landing rule
     *  and the publish come first. Nothing was sent. */
    public data object NotThisKey : AnnounceOutcome
}

/**
 * Publish the account's announcement when it is due
 * ([desiredSealEntries], [announcementDue]). Read, decide, sign, upload; a
 * `409 reset-seals-stale` is answered by reading again once. Throws what the
 * directory threw otherwise (the caller retries at the next landing).
 *
 * [seqFloor] is the highest number this device has seen for the account;
 * the previous generation's announcement the directory still serves counts
 * too, so the first announcement after a reset or a rotation continues the
 * numbering.
 */
public suspend fun announceSeals(
    account: SealAccount,
    material: SealMaterial,
    core: SealCore,
    directory: SealDirectoryPorts,
    seqFloor: Long,
    nowMs: Long,
): AnnounceOutcome {
    var floor = seqFloor
    repeat(2) { attempt ->
        val own = directory.fetchOwn() ?: return AnnounceOutcome.NotThisKey
        if (!own.signaturePubkey.contentEquals(account.accountKey) || own.keyGeneration != account.keyGeneration) {
            return AnnounceOutcome.NotThisKey
        }
        val current = own.resetSeals?.let { core.open(it, account.accountKey) }
            ?.takeIf { it.keyGeneration == account.keyGeneration }
        // The previous generation's announcement was signed by the PREVIOUS
        // account key, which a device that reset no longer holds: its number
        // is read without its signature. It only ever raises the number this
        // device signs with, so a forged one costs a higher number, no more.
        val previousSeq = own.previousResetSeals?.let { core.decodeUnverified(it) }?.seq
        val desired = desiredSealEntries(
            current,
            material.copy(
                passwordPkB64 = passwordSealToAssert(current, material.passwordPkB64, material.passwordFromThisRun),
            ),
        ) ?: return AnnounceOutcome.NoPasswordSeal
        if (current != null && !announcementDue(current, desired, account.keyGeneration)) {
            return AnnounceOutcome.Current(current)
        }
        val seq = nextSealSeq(current?.seq, previousSeq, floor)
        val signed = core.signAnnouncement(
            accountSeed = account.accountSeed,
            accountId = account.accountId,
            keyGeneration = account.keyGeneration,
            seq = seq,
            issuedAtMs = nowMs,
            entries = desired,
        )
        try {
            directory.upload(signed)
        } catch (stale: SealAnnouncementStale) {
            if (attempt == 1) throw stale
            floor = seq
            return@repeat
        }
        return AnnounceOutcome.Published(SealAnnouncementView(account.keyGeneration, seq, desired, nowMs))
    }
    throw SealAnnouncementStale()
}

// ---------------------------------------------------------------------------
// What this device remembers
// ---------------------------------------------------------------------------

/**
 * The account's announcement as this device last read or published it, and
 * the highest number it has seen: public facts (the announcement is served
 * to anyone), kept so Settings can say what keeps the chats without a
 * network call and so a number is never reused.
 */
public class SealStateStore(private val storage: MarkerStorage) {
    private val json = Json { ignoreUnknownKeys = true }

    public fun view(accountIdB64: String): SealAnnouncementView? = try {
        storage.get(VIEW_PREFIX + accountIdB64)?.let { json.decodeFromString(SealAnnouncementView.serializer(), it) }
    } catch (_: Throwable) {
        null
    }

    public fun seqFloor(accountIdB64: String): Long = try {
        storage.get(FLOOR_PREFIX + accountIdB64)?.toLongOrNull() ?: 0L
    } catch (_: Throwable) {
        0L
    }

    /** Keep [view] and raise the floor to its number. */
    public fun keep(accountIdB64: String, view: SealAnnouncementView) {
        try {
            me.lakko.app.account.AccountStoreGate.write(accountIdB64) {
                storage.set(VIEW_PREFIX + accountIdB64, json.encodeToString(SealAnnouncementView.serializer(), view))
                if (view.seq > seqFloor(accountIdB64)) storage.set(FLOOR_PREFIX + accountIdB64, view.seq.toString())
            }
        } catch (_: Throwable) {
            // Read again at the next landing.
        }
    }

    /** A number seen elsewhere (the previous generation's, a reset's). */
    public fun raiseFloor(accountIdB64: String, seq: Long) {
        if (seq <= seqFloor(accountIdB64)) return
        try {
            me.lakko.app.account.AccountStoreGate.write(accountIdB64) {
                storage.set(FLOOR_PREFIX + accountIdB64, seq.toString())
            }
        } catch (_: Throwable) {
            // The directory refuses a reused number; the announcer reads again.
        }
    }

    /** The announcement of the keys before a reset is not this account's
     *  any more; the floor stays. */
    public fun dropView(accountIdB64: String) {
        try {
            storage.remove(VIEW_PREFIX + accountIdB64)
        } catch (_: Throwable) {
            // Replaced by the next read.
        }
    }

    /** Passkey seal entries made on this device and not announced yet
     *  (public: an entry is a public key). */
    public fun pendingPasskeys(accountIdB64: String): List<SealEntryView> = try {
        storage.get(PENDING_PREFIX + accountIdB64)?.let {
            json.decodeFromString(kotlinx.serialization.builtins.ListSerializer(SealEntryView.serializer()), it)
        }.orEmpty().filter { it.kind == SealKindName.PASSKEY }
    } catch (_: Throwable) {
        emptyList()
    }

    /** Replace the not-yet-announced passkey seal entries. */
    public fun keepPendingPasskeys(accountIdB64: String, entries: List<SealEntryView>) {
        try {
            me.lakko.app.account.AccountStoreGate.write(accountIdB64) {
                if (entries.isEmpty()) {
                    storage.remove(PENDING_PREFIX + accountIdB64)
                } else {
                    storage.set(
                        PENDING_PREFIX + accountIdB64,
                        json.encodeToString(kotlinx.serialization.builtins.ListSerializer(SealEntryView.serializer()), entries),
                    )
                }
            }
        } catch (_: Throwable) {
            // Memory still holds this run's; the next announce writes again.
        }
    }

    /** Everything kept for [accountIdB64] (Forget device, a wipe). */
    public fun clearAccount(accountIdB64: String) {
        try {
            storage.remove(VIEW_PREFIX + accountIdB64)
            storage.remove(FLOOR_PREFIX + accountIdB64)
            storage.remove(PENDING_PREFIX + accountIdB64)
        } catch (_: Throwable) {
            // Nothing to drop.
        }
    }

    public companion object {
        public const val PREFS_NAME: String = "lakko-reset-seals"
        private const val VIEW_PREFIX: String = "announcement:"
        private const val FLOOR_PREFIX: String = "seq-floor:"
        private const val PENDING_PREFIX: String = "pending-passkeys:"

        public fun onDevice(context: Context): SealStateStore =
            SealStateStore(SharedPrefsMarkerStorage(context, PREFS_NAME))
    }
}
