// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import java.util.concurrent.atomic.AtomicLong
import kotlin.coroutines.cancellation.CancellationException
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock

/**
 * How one [KeyPackageRepublisher.republish] request ended.
 */
public enum class KeyPackageRepublishOutcome {
    /** A KeyPackage was minted, durably persisted, then uploaded. */
    UPLOADED,

    /** A run that started after this request was made already served it. */
    COVERED,

    /** No bootstrapped session to publish for; nothing was done. */
    NO_SESSION,

    /** Minting the KeyPackage failed; nothing was persisted or uploaded. */
    CREATE_FAILED,

    /** The snapshot holding the new private keys did not persist, so the
     *  KeyPackage was NOT uploaded (fail closed). */
    PERSIST_FAILED,

    /** Persisted, but the upload failed; the directory still holds the
     *  previous KeyPackage. */
    UPLOAD_FAILED,
}

/**
 * The effects [KeyPackageRepublisher] orders. [S] is whatever binds one
 * session together (production: the canonical `Identity`, the account ids and
 * the bearer token), captured once per run so the mint, the persist and the
 * upload all speak for the same account.
 */
internal interface KeyPackageRepublishPorts<S : Any> {
    /** Run [block] holding the lock that guards the shared MLS provider. */
    suspend fun <T> exclusiveProvider(block: suspend () -> T): T

    /** The current session, or `null` when not bootstrapped. Read under the
     *  provider lock. */
    fun currentSession(): S?

    /** Mint a KeyPackage from the session's LONG-LIVED identity, leaving its
     *  private keys in that identity's provider. Under the provider lock. */
    fun createKeyPackage(session: S): ByteArray

    /** Durably persist the whole provider snapshot (which now carries the new
     *  private keys). Throws on any failure. Under the provider lock. */
    fun persistSnapshotDurably(session: S)

    /** Upload [keyPackage] to the directory. Outside the provider lock. */
    suspend fun upload(session: S, keyPackage: ByteArray)
}

/**
 * Publishes a KeyPackage this device can actually be added with.
 *
 * ## Why this exists
 *
 * A KeyPackage is only useful if its init / leaf PRIVATE keys survive until a
 * Welcome built against it arrives. The previous upload sites minted it from
 * a throwaway `Identity.fromSeed(seed).use {}` handle, so the private keys
 * were gone the moment the block ended and every Welcome to this account
 * failed to open — the account could never be added to a group. Two further
 * facts make the fix more than "mint from the right identity":
 *
 *  - OpenMLS deletes a KeyPackage's private key when a Welcome consumes it
 *    (Lakko never sets `last_resort`), and the directory keeps ONE KeyPackage
 *    per account (an upsert). So after any Welcome join the published row is
 *    dead and must be replaced — hence a republish after every join attempt,
 *    successful or not.
 *  - The private keys live only in memory until the provider snapshot is
 *    written. Upload a KeyPackage before that write lands, crash, and the
 *    published row names keys this device no longer has. Hence the ordering
 *    below: **mint, persist durably, and only then upload** — and never upload
 *    at all when the persist failed.
 *
 * ## Serialization (single flight)
 *
 * Requests are numbered. One run at a time holds [runLock]; a run serves every
 * request numbered before it read `target`, which is strictly before it mints.
 * A request that queued behind a run which then served it returns [COVERED].
 * So a burst of requests that arrive before a run mints collapses into ONE
 * upload, while a request made AFTER a run minted always gets a run of its
 * own — that is the case that matters, because a Welcome join that consumed
 * the in-flight KeyPackage can only happen after that KeyPackage was minted
 * and uploaded, and its follow-up request must not be answered with the very
 * KeyPackage it just killed.
 *
 * A failed run still marks its requests served: the next trigger (the next
 * session start or the next Welcome join) retries, instead of every queued
 * request re-running a failing upload in a row.
 */
internal class KeyPackageRepublisher<S : Any>(
    private val ports: KeyPackageRepublishPorts<S>,
    private val log: (String, Throwable?) -> Unit,
) {
    private val requested = AtomicLong(0L)
    private val runLock = Mutex()

    /** Highest request number a completed run has served. Guarded by [runLock]. */
    private var served = 0L

    /**
     * Number a new request NOW, before anything suspends. Callers that launch
     * the work asynchronously call this synchronously first, so the ordering
     * of "the join finished" and "the request exists" is fixed by the caller.
     */
    fun markRequested(): Long = requested.incrementAndGet()

    /** Serve [ticket] (from [markRequested]), running a publish if needed. */
    suspend fun serve(ticket: Long): KeyPackageRepublishOutcome = runLock.withLock {
        if (served >= ticket) return@withLock KeyPackageRepublishOutcome.COVERED
        // Every request numbered up to here is served by this run: the
        // KeyPackage below is minted strictly after all of them were made.
        val target = requested.get()
        val outcome = runOnce()
        served = target
        outcome
    }

    /** Number a request and serve it. */
    suspend fun republish(): KeyPackageRepublishOutcome = serve(markRequested())

    private sealed interface Prepared<out S> {
        data object NoSession : Prepared<Nothing>
        data class Failed(val outcome: KeyPackageRepublishOutcome) : Prepared<Nothing>
        class Ready<S>(val session: S, val keyPackage: ByteArray) : Prepared<S>
    }

    private suspend fun runOnce(): KeyPackageRepublishOutcome {
        val prepared: Prepared<S> = ports.exclusiveProvider {
            val session = ports.currentSession() ?: return@exclusiveProvider Prepared.NoSession
            val keyPackage = try {
                ports.createKeyPackage(session)
            } catch (cause: CancellationException) {
                throw cause
            } catch (cause: Exception) {
                log("KeyPackage republish: mint failed", cause)
                return@exclusiveProvider Prepared.Failed(KeyPackageRepublishOutcome.CREATE_FAILED)
            }
            try {
                ports.persistSnapshotDurably(session)
            } catch (cause: CancellationException) {
                throw cause
            } catch (cause: Exception) {
                // Fail closed: the private keys are not on disk, so the public
                // half must not reach the directory.
                log("KeyPackage republish: snapshot persist failed; NOT uploading", cause)
                return@exclusiveProvider Prepared.Failed(KeyPackageRepublishOutcome.PERSIST_FAILED)
            }
            Prepared.Ready(session, keyPackage)
        }
        return when (prepared) {
            Prepared.NoSession -> {
                log("KeyPackage republish: no bootstrapped session; skipped", null)
                KeyPackageRepublishOutcome.NO_SESSION
            }
            is Prepared.Failed -> prepared.outcome
            is Prepared.Ready -> try {
                ports.upload(prepared.session, prepared.keyPackage)
                KeyPackageRepublishOutcome.UPLOADED
            } catch (cause: CancellationException) {
                throw cause
            } catch (cause: Exception) {
                log("KeyPackage republish: upload failed", cause)
                KeyPackageRepublishOutcome.UPLOAD_FAILED
            }
        }
    }
}
