// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.transport

import android.content.Context
import android.net.ConnectivityManager
import android.net.Network
import java.io.IOException
import java.util.concurrent.atomic.AtomicBoolean
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asSharedFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.withTimeoutOrNull
import uniffi.lakko_ffi_uniffi.FfiException

/** The three states of `docs/ux/connection-state.md` section 1. */
public enum class ConnectionPhase { ONLINE, WAITING, CONNECTING }

/** What can happen to the state (section 3). */
public enum class ConnectionEvent {
    /** A request got no HTTP answer. */
    NETWORK_FAILURE,

    /** A request got an HTTP answer, any status. */
    ANSWERED,

    /** The system says there is no usable route. */
    OS_NO_ROUTE,

    /** The system says a usable route is back. */
    OS_ROUTE_BACK,
}

/** The transition table of `docs/ux/connection-state.md` section 3. Pure. */
public fun nextConnectionPhase(phase: ConnectionPhase, event: ConnectionEvent): ConnectionPhase = when (event) {
    ConnectionEvent.NETWORK_FAILURE, ConnectionEvent.OS_NO_ROUTE -> ConnectionPhase.WAITING
    ConnectionEvent.ANSWERED -> ConnectionPhase.ONLINE
    ConnectionEvent.OS_ROUTE_BACK -> if (phase == ConnectionPhase.WAITING) ConnectionPhase.CONNECTING else phase
}

/**
 * Whether a failed request got no HTTP answer: an I/O failure from OkHttp or
 * Ktor (no route, DNS, refused, reset, TLS, a timeout - every one of them an
 * [IOException]) or the anti-censorship tunnel failing to connect. A
 * cancellation is not one, nor is the suspension gate's local refusal, nor
 * anything a caller threw itself. Pure.
 */
public fun isNetworkFailure(cause: Throwable): Boolean = when (cause) {
    is CancellationException -> false
    // The suspension gate's refusal is an IOException so callers treat it as
    // a request that could not be made, but nothing about the network is
    // wrong (`docs/ux/connection-state.md` section 7.1: "Not sent").
    is me.lakko.app.auth.SuspendedWriteRefused -> false
    is IOException -> true
    is FfiException.Transport -> true
    else -> false
}

/**
 * Whether [cause], or anything it wraps, is a network failure
 * ([isNetworkFailure]). The app's clients wrap the I/O failure in their own
 * error types (`DsNetworkError`, `StorageNetworkError`, `ChatError`, ...),
 * so a send path asks through the wrappers. A cancellation at the top is
 * never one. The walk is bounded and stops at a cycle. Pure.
 */
public fun causedByNetworkFailure(cause: Throwable): Boolean {
    if (cause is CancellationException) return false
    var current: Throwable? = cause
    var steps = 0
    while (current != null && steps < MAX_CAUSE_DEPTH) {
        if (isNetworkFailure(current)) return true
        val next = current.cause
        if (next === current) return false
        current = next
        steps += 1
    }
    return false
}

private const val MAX_CAUSE_DEPTH = 16

/**
 * A receive loop's pause after a failed pass: [millis], ended early by the
 * network coming back when [cause] was a network failure
 * (`docs/ux/connection-state.md` section 3), the full [millis] for any
 * other failure (an HTTP refusal is not cured by a route coming back).
 */
public suspend fun backoffAfterFailure(
    cause: Throwable,
    millis: Long,
    networkReturned: kotlinx.coroutines.flow.Flow<Unit> = ConnectionState.networkReturned,
) {
    if (causedByNetworkFailure(cause)) {
        withTimeoutOrNull(millis) { networkReturned.first() }
    } else {
        kotlinx.coroutines.delay(millis)
    }
}

/** The catalog key spoken on entering a phase (section 5). */
public fun connectionAnnouncementKey(phase: ConnectionPhase): String = when (phase) {
    ConnectionPhase.ONLINE -> "connection.restored_a11y"
    ConnectionPhase.WAITING -> "connection.waiting"
    ConnectionPhase.CONNECTING -> "connection.connecting"
}

/** The catalog key for the header in a phase, or `null` when the normal text
 *  stands. */
public fun connectionHeaderKey(phase: ConnectionPhase): String? = when (phase) {
    ConnectionPhase.ONLINE -> null
    ConnectionPhase.WAITING -> "connection.waiting"
    ConnectionPhase.CONNECTING -> "connection.connecting"
}

/**
 * Whether this device can reach the Lakko servers - the one source behind the
 * "Waiting for network..." / "Connecting..." header
 * (`docs/ux/connection-state.md`).
 *
 * Fed by [AntiCensorshipEngine.execute], which every Ktor request of the app
 * passes through (direct or tunnelled), and by the system's default-network
 * callback. The system is an early hint, never the last word: an HTTP answer
 * always returns the state to online.
 */
public object ConnectionState {
    private val mutablePhase = MutableStateFlow(ConnectionPhase.ONLINE)
    private val returned = MutableSharedFlow<Unit>(extraBufferCapacity = 1)
    private val mutableAnnouncement = MutableStateFlow<ConnectionPhase?>(null)
    private val installed = AtomicBoolean(false)

    /** The current phase. */
    public val phase: StateFlow<ConnectionPhase> = mutablePhase.asStateFlow()

    /** The phase last entered by a transition, for the polite announcement
     *  (section 5); `null` until the first transition. */
    public val announcement: StateFlow<ConnectionPhase?> = mutableAnnouncement.asStateFlow()

    /** Emits when the network comes back (Waiting -> Connecting). */
    public val networkReturned: SharedFlow<Unit> = returned.asSharedFlow()

    /** Feed one event. */
    public fun note(event: ConnectionEvent) {
        while (true) {
            val before = mutablePhase.value
            val after = nextConnectionPhase(before, event)
            if (after == before) return
            if (mutablePhase.compareAndSet(before, after)) {
                mutableAnnouncement.value = after
                if (after == ConnectionPhase.CONNECTING) returned.tryEmit(Unit)
                return
            }
        }
    }

    /** A request that got an HTTP answer. */
    public fun noteAnswered(): Unit = note(ConnectionEvent.ANSWERED)

    /** A request that failed; only a network failure changes anything. */
    public fun noteFailure(cause: Throwable) {
        if (isNetworkFailure(cause)) note(ConnectionEvent.NETWORK_FAILURE)
    }

    /**
     * Sleep [millis], or less if the network comes back meanwhile, so a
     * receive loop retries at once rather than finishing its backoff
     * (section 3: "Connecting..." is short).
     */
    public suspend fun delayUnlessNetworkReturns(millis: Long) {
        withTimeoutOrNull(millis) { networkReturned.first() }
    }

    /** Listen to the system's default network. Idempotent. */
    public fun install(context: Context) {
        if (!installed.compareAndSet(false, true)) return
        val manager = context.applicationContext.getSystemService(ConnectivityManager::class.java) ?: return
        try {
            val active = manager.activeNetwork
            if (active == null) note(ConnectionEvent.OS_NO_ROUTE)
            manager.registerDefaultNetworkCallback(object : ConnectivityManager.NetworkCallback() {
                override fun onAvailable(network: Network) {
                    note(ConnectionEvent.OS_ROUTE_BACK)
                }

                override fun onLost(network: Network) {
                    if (manager.activeNetwork == null) note(ConnectionEvent.OS_NO_ROUTE)
                }
            })
        } catch (_: SecurityException) {
            // Without the network-state permission the requests alone decide.
        } catch (_: RuntimeException) {
            // Too many callbacks registered, or the service is gone: the
            // requests alone decide.
        }
    }

    /** Test seam: back to the starting state. */
    public fun resetForTest() {
        mutablePhase.value = ConnectionPhase.ONLINE
        mutableAnnouncement.value = null
    }
}
