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
 * `finishCanonicalSession`, `recover`).
 */
internal enum class IdentityLanding {
    /** A new account: password signup, or ADR-0032 guest signup. */
    SIGNUP,

    /** ADR-0017 lost-everything: a freshly minted identity replaces the old one. */
    FRESH_IDENTITY,

    /**
     * An explicit sign-in on a device that holds the phrase seed: password
     * login, passkey sign-in, or ACCESS-factor recovery (TOTP / contact code).
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
     * Check the directory against our key (raising the ADR-0017
     * "published identity changed" alert on a mismatch), then re-publish
     * unconditionally. Best-effort: a failure never fails the sign-in.
     */
    REFRESH,

    /**
     * As [REFRESH], but publish only when the directory is missing the bundle
     * or holds different keys, and skip when it could not be read — a relaunch
     * must not pay an upload for a no-op.
     */
    REFRESH_IF_CHANGED,

    /**
     * Re-publish after a recover without reading first. The key is unchanged
     * (ADR-0017), so a genesis self-signature is valid; it is attached so a
     * recover that is the directory's first sight of the account still gets a
     * log entry. Best-effort.
     */
    REPUBLISH,
}

internal fun identityPublicationFor(landing: IdentityLanding): IdentityPublication =
    when (landing) {
        IdentityLanding.SIGNUP -> IdentityPublication.GENESIS
        IdentityLanding.FRESH_IDENTITY -> IdentityPublication.GENESIS
        IdentityLanding.SIGN_IN -> IdentityPublication.REFRESH
        IdentityLanding.SESSION_RESTORE -> IdentityPublication.REFRESH_IF_CHANGED
        IdentityLanding.RECOVER -> IdentityPublication.REPUBLISH
    }

/** The public keys the directory currently serves for the account. */
internal class PublishedIdentityKeys(
    val signaturePubkey: ByteArray,
    val x25519Pubkey: ByteArray,
)

/** What [publishIdentity] needs from the outside world. */
internal interface IdentityPublicationPorts {
    /** The canonical identity's Ed25519 public key. */
    val signaturePubkey: ByteArray

    /** The canonical identity's X25519 public key. */
    val x25519Pubkey: ByteArray

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

    /** Set (`true`) or clear (`false`) the "published identity changed" alert. */
    fun reportPublishedIdentity(changed: Boolean)
}

/** What a publish did. Diagnostic, and what the tests assert on. */
internal sealed interface IdentityPublicationResult {
    data class Uploaded(val withGenesisAuth: Boolean) : IdentityPublicationResult

    /** The directory already serves exactly our keys ([IdentityPublication.REFRESH_IF_CHANGED]). */
    data object UpToDate : IdentityPublicationResult

    /** The directory could not be read, so a change could not be established. */
    data object Unverified : IdentityPublicationResult

    /** The session ended while the read was out; nothing was reported or published. */
    data object Superseded : IdentityPublicationResult

    /** A best-effort upload failed (logged). */
    data object UploadFailed : IdentityPublicationResult
}

/**
 * Carry out [publication] through [ports].
 *
 * The refresh modes read BEFORE they write, and that order is what keeps the
 * ADR-0017 alert meaningful: re-publishing first would overwrite a replaced
 * bundle before anyone looked at it, and the check would always pass.
 * Re-publishing our key over a replaced one after raising the alert is the
 * web behaviour, and is what the discovery cool-down treats as a heal.
 *
 * Only [IdentityPublication.GENESIS] throws; the other modes log and report
 * [IdentityPublicationResult.UploadFailed]. Cancellation always propagates.
 */
internal suspend fun publishIdentity(
    publication: IdentityPublication,
    ports: IdentityPublicationPorts,
    log: (String, Throwable?) -> Unit,
): IdentityPublicationResult {
    when (publication) {
        IdentityPublication.GENESIS -> {
            ports.upload(withGenesisAuth = true)
            return IdentityPublicationResult.Uploaded(withGenesisAuth = true)
        }
        IdentityPublication.REPUBLISH ->
            return uploadBestEffort(ports, withGenesisAuth = true, log = log)
        IdentityPublication.REFRESH,
        IdentityPublication.REFRESH_IF_CHANGED,
        -> Unit
    }

    var readable = true
    val published: PublishedIdentityKeys? = try {
        ports.fetchPublished()
    } catch (cancel: CancellationException) {
        throw cancel
    } catch (cause: Throwable) {
        log("identity bundle self-check could not read the directory", cause)
        readable = false
        null
    }
    if (!ports.isStillCurrent()) return IdentityPublicationResult.Superseded

    val signatureMatches =
        published != null && published.signaturePubkey.contentEquals(ports.signaturePubkey)
    if (readable) {
        // A 404 is not a replacement: nothing is published yet.
        ports.reportPublishedIdentity(changed = published != null && !signatureMatches)
    }

    if (publication == IdentityPublication.REFRESH_IF_CHANGED) {
        if (!readable) return IdentityPublicationResult.Unverified
        if (signatureMatches && published?.x25519Pubkey?.contentEquals(ports.x25519Pubkey) == true) {
            return IdentityPublicationResult.UpToDate
        }
    }

    // Genesis authorisation only when the directory PROVABLY has nothing: that
    // publish is the account's first log entry (an account signed up before
    // signup published). A re-publish of a logged key needs none.
    return uploadBestEffort(ports, withGenesisAuth = readable && published == null, log = log)
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
