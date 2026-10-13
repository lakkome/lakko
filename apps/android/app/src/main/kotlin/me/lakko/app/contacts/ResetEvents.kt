// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.contacts

import android.util.Log
import java.util.concurrent.CopyOnWriteArraySet

/**
 * ADR-0073 R.2 - "a reset was accepted", as an in-process signal.
 *
 * The identity gate ([ContactsClient]) accepts a reset wherever a bundle is
 * fetched; the work that follows (moving the shared conversations onto keys
 * the old devices do not hold, R.5 to R.7) belongs to the chat layer. This is
 * the one seam between them. It carries no UI: nothing that listens may show
 * anything (R.0). Mirror of `apps/web/src/contacts/reset-events.ts`.
 */
public data class ResetAccepted(
    /** The signed-in account whose device accepted it (standard base64);
     *  empty without a session. The process-wide listener set serves every
     *  account that signs in on this phone, so a listener checks it. */
    val ownerAccountIdB64: String,
    /** Standard base64 of the 16-byte account id. */
    val accountIdB64: String,
    /** The key this device held for the account before the re-pin. */
    val previousKeyB64: String,
    val newKeyB64: String,
    /** The new identity's X25519 key from the same bundle, when known. */
    val newX25519B64: String? = null,
    val keyGeneration: Long,
    /** The attestation's `signed_at`, UNIX milliseconds. */
    val signedAtMs: Long,
    /** Every key the reset retired (the held key and its proven
     *  predecessors): display aliases of stored history from now on. */
    val retiredKeysB64: List<String> = emptyList(),
)

/** Process-wide, like the web module's listener set: every [ContactsClient]
 *  reports to the same listeners. */
public object ResetEvents {
    private val listeners = CopyOnWriteArraySet<(ResetAccepted) -> Unit>()

    /** Listen for accepted resets. Returns the unsubscribe. */
    public fun subscribe(listener: (ResetAccepted) -> Unit): () -> Unit {
        listeners.add(listener)
        return { listeners.remove(listener) }
    }

    private val named = java.util.concurrent.ConcurrentHashMap<String, (ResetAccepted) -> Unit>()

    /**
     * Listen under a NAME: a later listener of the same name replaces the
     * earlier one. A repository is rebuilt when its activity is, and the one
     * that listens must be the live one, once.
     */
    public fun subscribeAs(name: String, listener: (ResetAccepted) -> Unit) {
        named.put(name, listener)?.let { listeners.remove(it) }
        listeners.add(listener)
    }

    /** Tell every listener. A listener that throws does not stop the others
     *  and never reaches the gate that accepted the reset. */
    public fun notifyResetAccepted(event: ResetAccepted) {
        for (listener in listeners) {
            try {
                listener(event)
            } catch (cause: Throwable) {
                try {
                    Log.w("lakko-contacts", "reset-accepted listener failed: ${cause.javaClass.simpleName}")
                } catch (_: Throwable) {
                    // android.util.Log is absent on the host JVM.
                }
            }
        }
    }
}
