// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.donate

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlin.coroutines.cancellation.CancellationException

/** The screen's state (docs/ux/donations.md section 2.3). */
public sealed interface DonationsState {
    public data object Loading : DonationsState

    /** Nothing for this country: none sent, none verified, or no pinned key. */
    public data object None : DonationsState

    public data object Error : DonationsState

    public class Loaded(public val methods: List<DonateMethod>) : DonationsState
}

/** The phone line of one card (section 4.1 step 6). */
public sealed interface PhoneState {
    public data object Masked : PhoneState

    public data object Busy : PhoneState

    /** Show failed: the mask stays, Show can be pressed again. */
    public data object Failed : PhoneState

    public data class Shown(public val number: String) : PhoneState
}

/**
 * The Support Lakko screen's logic, kept apart from Compose so it runs on a
 * host JVM with a fake [DonationsSource]. The revealed numbers live here only
 * and are gone with the screen.
 */
public class DonationsModel(private val source: DonationsSource) {
    private val _state = MutableStateFlow<DonationsState>(DonationsState.Loading)
    public val state: StateFlow<DonationsState> = _state.asStateFlow()

    private val _phones = MutableStateFlow<Map<String, PhoneState>>(emptyMap())
    public val phones: StateFlow<Map<String, PhoneState>> = _phones.asStateFlow()

    /** Load (and, after an error, load again). */
    public suspend fun load() {
        _state.value = DonationsState.Loading
        _state.value = try {
            val methods = source.methods()
            if (methods.isEmpty()) DonationsState.None else DonationsState.Loaded(methods)
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (_: Exception) {
            DonationsState.Error
        }
    }

    public fun phoneState(id: String): PhoneState = _phones.value[id] ?: PhoneState.Masked

    /** Show: fetch and verify the number; any failure leaves the mask. */
    public suspend fun revealPhone(id: String) {
        if (phoneState(id) is PhoneState.Busy || phoneState(id) is PhoneState.Shown) return
        _phones.update { it + (id to PhoneState.Busy) }
        val next = try {
            PhoneState.Shown(source.phone(id))
        } catch (cancellation: CancellationException) {
            _phones.update { it + (id to PhoneState.Masked) }
            throw cancellation
        } catch (_: Exception) {
            PhoneState.Failed
        }
        _phones.update { it + (id to next) }
    }

    /** The verified QR bytes of a method, or `null` (the card shows no QR). */
    public suspend fun qr(method: DonateMethod): ByteArray? {
        val sha = method.qrSha256 ?: return null
        return try {
            source.qr(method.id, sha)
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (_: Exception) {
            null
        }
    }
}
