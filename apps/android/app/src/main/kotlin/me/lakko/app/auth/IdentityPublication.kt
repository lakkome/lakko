// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import kotlinx.coroutines.CancellationException

/**
 * When, and how, a session publishes the account's identity bundle
 * (`POST /discovery/identity`: the AccountIdentity signature + X25519 public
 * keys) to the discovery directory.
 *
 * ## The defect this closes
 *
 * On Android neither signup nor login published the bundle; only recover (and
 * the guest / lost-everything paths) did. A peer needs that bundle to verify
 * this account at all — the contact and group flows fetch it, and the
 * ADR-0017 fresh-device check (`provideRecoveryPhrase`) refuses outright when
 * it is missing. So an account created on Android could not be verified by
 * anyone, including its owner's second device.
 *
 * ## No publish-back (ADR-0073 section 4.2)
 *
 * On every landing that holds a cached seed — a sign-in, a recover, the
 * silent restore — the directory is read BEFORE anything is uploaded, and
 * the rule is one pure function, [landingVerdict]:
 *
 *   * the published key equals this device's: proceed (upload only where the
 *     landing uploaded before);
 *   * the published key differs and the published generation is above the
 *     one of this device's session pair: this device holds stale keys (the
 *     account was reset elsewhere). The signed-out screen and the automatic
 *     wipe; NEVER an upload;
 *   * the published key differs at the same (or a lower) generation: an
 *     anomaly the directory should make impossible; the blocking screen
 *     with the wipe on tap; never an upload;
 *   * nothing published (404): the device publishes, as its first entry;
 *   * the directory could not be read: nothing is uploaded, the next landing
 *     checks again.
 *
 * Nothing re-publishes a differing key. The dismissible "published identity
 * changed" banner and the heal upload that followed it are gone.
 *
 * ## Which identity
 *
 * The bundle is derived from the canonical AccountIdentity (the ADR-0017
 * phrase seed), never from the password export key. `Identity.fromSeed` is
 * deterministic, so the public keys published here are byte-identical to those
 * of the long-lived Identity `ChatRepository` builds from the same seed. That
 * is also why a short-lived handle is safe for the BUNDLE and not for a
 * KeyPackage: a bundle carries public keys only, while a KeyPackage's private
 * keys live in the provider of the handle that minted it. KeyPackages remain
 * `ChatRepository.republishKeyPackage`'s job.
 *
 * ## The policy
 *
 * One mode per landing, decided by [identityPublicationFor] and carried out by
 * [publishIdentity], mirroring the web `AuthContext` (`signup`,
 * `finishCanonicalSession`, `recover`). A reset's attested publish is not a
 * landing: it is `AuthRepository.mintFreshIdentity`'s own upload.
 */
internal enum class IdentityLanding {
    /** A new account: password signup, or ADR-0032 guest signup. */
    SIGNUP,

    /**
     * An explicit sign-in on a device that holds the phrase seed: password
     * login, passkey sign-in, or ACCESS-factor recovery (TOTP).
     */
    SIGN_IN,

    /** The silent cold-start restore of a persisted session. */
    SESSION_RESTORE,

    /** Recover-from-words: the password changes, the identity does not. */
    RECOVER,
}

/** How a landing publishes the bundle. */
internal enum class IdentityPublication {
    /**
     * A NEW key's first publish: carry the ADR-0019 genesis self-signature and
     * the ADR-0036 auth attestation so the transparency log accepts the
     * binding. FAIL LOUD — the caller fails rather than land an account no peer
     * can verify.
     */
    GENESIS,

    /**
     * Read the directory, apply [landingVerdict], then re-publish the same
     * key unconditionally. Best-effort: a failure never fails the sign-in.
     */
    REFRESH,

    /**
     * As [REFRESH], but publish only when the directory is missing the bundle
     * or serves a different X25519 key for the same signature key — a relaunch
     * must not pay an upload for a no-op.
     */
    REFRESH_IF_CHANGED,

    /**
     * After a recover: read the directory, apply [landingVerdict], re-publish.
     * The key is unchanged (ADR-0017), so a genesis self-signature is valid
     * when the directory holds nothing; it is attached then, so a recover
     * that is the directory's first sight of the account still gets a log
     * entry. Best-effort.
     */
    REPUBLISH,
}

internal fun identityPublicationFor(landing: IdentityLanding): IdentityPublication =
    when (landing) {
        IdentityLanding.SIGNUP -> IdentityPublication.GENESIS
        IdentityLanding.SIGN_IN -> IdentityPublication.REFRESH
        IdentityLanding.SESSION_RESTORE -> IdentityPublication.REFRESH_IF_CHANGED
        IdentityLanding.RECOVER -> IdentityPublication.REPUBLISH
    }

/** The public keys the directory currently serves for the account, and the
 *  key generation they belong to (ADR-0073). */
internal class PublishedIdentityKeys(
    val signaturePubkey: ByteArray,
    val x25519Pubkey: ByteArray,
    val keyGeneration: Long,
)

/** What the directory's answer means for this device's keys. */
internal sealed interface LandingVerdict {
    /** Nothing published (`null`), or the published key is this device's. */
    class Proceed(val published: PublishedIdentityKeys?) : LandingVerdict

    /** Another key at a higher generation: the account was reset elsewhere
     *  and this device holds stale keys. Sign out and wipe. */
    data object StaleKeys : LandingVerdict

    /** Another key at the same or a lower generation: an anomaly. The
     *  blocking screen; the wipe on tap. */
    data object KeyMismatch : LandingVerdict
    /**
     * ADR-0074 section 3.3 - the directory still serves a key this device
     * holds as its OWN proven predecessor, at a LOWER generation than this
     * device's session: this device adopted a rotation whose directory
     * upload (the rotating device's, with the proof and the attestation)
     * has not landed yet. Nothing is uploaded and nothing is reported.
     */
    data object OwnPredecessorPublished : LandingVerdict
}

/** The pure rule of ADR-0073 section 4.2, over a readable directory answer. */
internal fun landingVerdict(
    published: PublishedIdentityKeys?,
    ownSignaturePubkey: ByteArray,
    ownKeyGeneration: Long,
    /** The keys of this account's own previous identities held on this
     *  device after a rotation it adopted (empty when it never rotated). */
    ownPredecessors: List<ByteArray> = emptyList(),
): LandingVerdict {
    if (published == null) return LandingVerdict.Proceed(null)
    if (published.signaturePubkey.contentEquals(ownSignaturePubkey)) return LandingVerdict.Proceed(published)
    if (published.keyGeneration < ownKeyGeneration &&
        ownPredecessors.any { it.contentEquals(published.signaturePubkey) }
    ) {
        return LandingVerdict.OwnPredecessorPublished
    }
    return if (published.keyGeneration > ownKeyGeneration) LandingVerdict.StaleKeys else LandingVerdict.KeyMismatch
}

/** What [publishIdentity] needs from the outside world. */
internal interface IdentityPublicationPorts {
    /** The canonical identity's Ed25519 public key. */
    val signaturePubkey: ByteArray

    /** The canonical identity's X25519 public key. */
    val x25519Pubkey: ByteArray

    /** The key generation of this device's session pair. */
    val ownKeyGeneration: Long

    /** The keys of this account's own previous identities on this device
     *  (ADR-0074 section 3.3); empty when it never rotated. */
    val ownPredecessors: List<ByteArray> get() = emptyList()

    /** `GET /discovery/identity/{account}`; `null` on 404, throws otherwise. */
    suspend fun fetchPublished(): PublishedIdentityKeys?

    /**
     * `POST /discovery/identity` with the canonical keys.
     *
     * @param withGenesisAuth attach the genesis self-signature and the
     *   best-effort auth attestation.
     */
    suspend fun upload(withGenesisAuth: Boolean)

    /**
     * `false` once the session this publish belongs to is gone (logout, or a
     * sign-in to another account while a background refresh was out). Read
     * only by the refresh modes, which run in the background.
     */
    fun isStillCurrent(): Boolean

    /** The directory serves other keys for the account: this device's keys
     *  are stale ([LandingVerdict.StaleKeys]) or an anomaly
     *  ([LandingVerdict.KeyMismatch]). Never followed by an upload. */
    fun onStaleKeys(verdict: LandingVerdict)
}

/** What a publish did. Diagnostic, and what the tests assert on. */
internal sealed interface IdentityPublicationResult {
    data class Uploaded(val withGenesisAuth: Boolean) : IdentityPublicationResult

    /** The directory already serves exactly our keys ([IdentityPublication.REFRESH_IF_CHANGED]). */
    data object UpToDate : IdentityPublicationResult

    /** The directory could not be read, so nothing was uploaded. */
    data object Unverified : IdentityPublicationResult

    /** The session ended while the read was out; nothing was reported or published. */
    data object Superseded : IdentityPublicationResult

    /** A best-effort upload failed (logged). */
    data object UploadFailed : IdentityPublicationResult

    /** The directory serves other keys at a higher generation: the account
     *  was reset elsewhere. Reported, nothing uploaded. */
    data object StaleKeys : IdentityPublicationResult

    /** The directory serves other keys at the same generation. Reported,
     *  nothing uploaded. */
    data object KeyMismatch : IdentityPublicationResult

    /** The directory still serves this account's own previous key after a
     *  rotation this device adopted. Nothing uploaded, nothing reported. */
    data object AwaitingRotationPublish : IdentityPublicationResult
}

/**
 * Carry out [publication] through [ports].
 *
 * Every mode but [IdentityPublication.GENESIS] reads BEFORE it writes and
 * applies [landingVerdict]; a directory that could not be read gets no
 * upload at all (the next landing checks again), so a device can never put
 * its keys over the account's without having looked.
 *
 * Only [IdentityPublication.GENESIS] throws; the other modes log and report
 * [IdentityPublicationResult.UploadFailed]. Cancellation always propagates.
 */
internal suspend fun publishIdentity(
    publication: IdentityPublication,
    ports: IdentityPublicationPorts,
    log: (String, Throwable?) -> Unit,
): IdentityPublicationResult {
    if (publication == IdentityPublication.GENESIS) {
        ports.upload(withGenesisAuth = true)
        return IdentityPublicationResult.Uploaded(withGenesisAuth = true)
    }

    val published: PublishedIdentityKeys? = try {
        ports.fetchPublished()
    } catch (cancel: CancellationException) {
        throw cancel
    } catch (cause: Throwable) {
        log("identity bundle self-check could not read the directory; nothing uploaded", cause)
        return IdentityPublicationResult.Unverified
    }
    if (!ports.isStillCurrent()) return IdentityPublicationResult.Superseded

    val verdict = landingVerdict(published, ports.signaturePubkey, ports.ownKeyGeneration, ports.ownPredecessors)
    when (verdict) {
        LandingVerdict.OwnPredecessorPublished -> {
            log("the directory still serves this account's previous key; the rotation's upload has not landed", null)
            return IdentityPublicationResult.AwaitingRotationPublish
        }
        LandingVerdict.StaleKeys -> {
            log("the directory serves other keys at a higher generation: this device's keys are stale", null)
            ports.onStaleKeys(verdict)
            return IdentityPublicationResult.StaleKeys
        }
        LandingVerdict.KeyMismatch -> {
            log("the directory serves other keys at the same generation: never uploading over them", null)
            ports.onStaleKeys(verdict)
            return IdentityPublicationResult.KeyMismatch
        }
        is LandingVerdict.Proceed -> Unit
    }

    if (publication == IdentityPublication.REFRESH_IF_CHANGED &&
        published != null && published.x25519Pubkey.contentEquals(ports.x25519Pubkey)
    ) {
        return IdentityPublicationResult.UpToDate
    }

    // Genesis authorisation only when the directory PROVABLY has nothing: that
    // publish is the account's first log entry (an account signed up before
    // signup published). A re-publish of a logged key needs none.
    return uploadBestEffort(ports, withGenesisAuth = published == null, log = log)
}

private suspend fun uploadBestEffort(
    ports: IdentityPublicationPorts,
    withGenesisAuth: Boolean,
    log: (String, Throwable?) -> Unit,
): IdentityPublicationResult =
    try {
        ports.upload(withGenesisAuth)
        IdentityPublicationResult.Uploaded(withGenesisAuth)
    } catch (cancel: CancellationException) {
        throw cancel
    } catch (cause: Throwable) {
        log("identity bundle upload failed", cause)
        IdentityPublicationResult.UploadFailed
    }
