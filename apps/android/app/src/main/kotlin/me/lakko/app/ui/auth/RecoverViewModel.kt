// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.auth

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import me.lakko.app.auth.AuthClientArgError
import me.lakko.app.auth.AuthHttpError
import me.lakko.app.auth.AuthNetworkError
import me.lakko.app.auth.AuthRepository
import me.lakko.app.auth.ContactKind
import me.lakko.app.auth.isPlausibleContact
import me.lakko.app.auth.normalizeTotpCode
import me.lakko.app.recovery.ShamirBridge
import me.lakko.app.recovery.ShamirBridgeException
import uniffi.lakko_ffi_uniffi.FfiException

/**
 * State + dispatcher for the [RecoverScreen].
 *
 * Mirror of `apps/web/src/routes/Recover.tsx`'s local signals.
 * Single-stage form (mnemonic + new password + confirm). On
 * submit drives [AuthRepository.recover], which rotates the
 * password envelope on the server, re-derives the device root
 * key from the new OPAQUE export key, and flips the auth state.
 *
 * The [cachedUsername] constructor arg drives the "Account on
 * this device: @alice" line in the UI. When it's `null` the
 * screen renders a "no cached account" stub instead (the
 * recovery flow can't run without an `accountId` on disk; that's
 * a 1.15e/discovery sprint problem).
 */
public class RecoverViewModel(
    private val repo: AuthRepository,
    public val cachedUsername: String?,
    /** ADR-0018 — factory for the discovery client the AnotherDevice
     *  mode uses to resolve the typed username to an account id.
     *  Overridable for tests. */
    private val discoveryClientFactory: () -> me.lakko.app.discovery.DiscoveryClient = {
        me.lakko.app.discovery.DiscoveryClient()
    },
    /** ADR-0048 §4 — the core's username rule (`validateUsername`). */
    private val validateUsername: (String) -> String =
        { uniffi.lakko_ffi_uniffi.validateUsername(it) },
) : ViewModel() {

    /**
     * ADR-0048 §4 — gate a typed username before any lookup: the normalised
     * name, or `null` after surfacing the rule's reason as the form error.
     */
    private fun usernameOrRefuse(typed: String): String? =
        when (val decision = me.lakko.app.profile.searchDecision(typed, validateUsername)) {
            is me.lakko.app.profile.SearchDecision.Lookup -> decision.normalized
            is me.lakko.app.profile.SearchDecision.Refuse -> {
                mutableState.update { it.copy(error = decision.message) }
                null
            }
        }


    private val mutableState: MutableStateFlow<RecoverUiState> =
        MutableStateFlow(RecoverUiState())

    public val state: StateFlow<RecoverUiState> = mutableState.asStateFlow()

    public fun onPhraseChange(value: String) {
        mutableState.update { it.copy(phrase = value) }
    }

    /** ADR-0017 — username for the FRESH-device recover path. When
     *  no account is cached on this device, the user types their
     *  username so [AuthRepository.recover] can resolve the
     *  account_id via the discovery service. Ignored (the cached id
     *  wins) when an account is already on this device. */
    public fun onUsernameChange(value: String) {
        mutableState.update { it.copy(username = value) }
    }

    /** 1.21c3 — Shamir-shares textarea content. */
    public fun onSharesInputChange(value: String) {
        mutableState.update { it.copy(sharesInput = value) }
    }

    /** ADR-0018 factor (c) — 6-digit authenticator code field. */
    public fun onTotpCodeChange(value: String) {
        mutableState.update { it.copy(totpCode = value) }
    }

    /** ADR-0018 factor (d) — Email / SMS toggle for the Contact mode.
     *  Clears the error so a kind switch starts clean. */
    public fun onContactKindChange(kind: Int) {
        mutableState.update { it.copy(contactKind = kind, error = null) }
    }

    /** ADR-0018 factor (d) — phone / email field for the Contact mode. */
    public fun onContactChange(value: String) {
        mutableState.update { it.copy(contact = value) }
    }

    /** ADR-0018 factor (d) — 6-digit delivered-code field. */
    public fun onContactCodeChange(value: String) {
        mutableState.update { it.copy(contactCode = value) }
    }

    /** 1.21c3 — toggle between Phrase and Shares input modes
     *  (ADR-0018: + AnotherDevice). Clears the resolved restore
     *  target when leaving the AnotherDevice mode. */
    public fun onModeChange(mode: RecoverMode) {
        mutableState.update {
            it.copy(
                mode = mode,
                error = null,
                deviceRestoreTarget = if (mode == RecoverMode.AnotherDevice) {
                    it.deviceRestoreTarget
                } else {
                    null
                },
                // ADR-0018 factor (d): leaving the Contact mode resets
                // its two-step state so a re-entry starts at the
                // request step (a stale "code" step would otherwise
                // dangle with no pending challenge).
                contactStep = if (mode == RecoverMode.Contact) {
                    it.contactStep
                } else {
                    ContactRecoverStep.Request
                },
            )
        }
    }

    /**
     * ADR-0018 factor (a), path (b) — resolve the typed username to
     * an account id via the discovery service so the AnotherDevice
     * restore can build the pairing QR. Anonymous (OPRF-blinded
     * lookup, no session needed).
     */
    public fun lookupForDeviceRestore() {
        val snapshot = mutableState.value
        if (snapshot.busy) return
        val typed = snapshot.username.trim()
        if (typed.isEmpty()) {
            mutableState.update {
                it.copy(error = "Enter your username first.")
            }
            return
        }
        val lookupName = usernameOrRefuse(typed) ?: return
        mutableState.update { it.copy(busy = true, error = null) }
        viewModelScope.launch {
            try {
                val resolved = discoveryClientFactory().use { discovery ->
                    discovery.lookup(lookupName)
                }
                val account = resolved.account
                if (account == null) {
                    mutableState.update {
                        it.copy(
                            busy = false,
                            error = "No account with that username. Check the spelling.",
                        )
                    }
                    return@launch
                }
                mutableState.update {
                    it.copy(
                        busy = false,
                        deviceRestoreTarget = DeviceRestoreTarget(
                            accountIdBytes = account,
                            accountIdB64 = me.lakko.app.auth.base64Encode(account),
                            username = resolved.normalisedUsername,
                        ),
                    )
                }
            } catch (cause: Throwable) {
                mutableState.update {
                    it.copy(
                        busy = false,
                        error = cause.message ?: "Could not look up the account. Try again.",
                    )
                }
            }
        }
    }

    /**
     * ADR-0018 factor (a), path (b) — complete the no-password
     * restore with the identity + redeemed session token received
     * from the other device. Suspend passthrough to
     * [AuthRepository.completeFromDeviceRestore]; verification
     * failures propagate to the calling panel. Requires the redeemed
     * token (without it this device has no ACCESS) — refused
     * otherwise with a clear message.
     */
    public suspend fun completeFromDeviceRestore(
        outcome: me.lakko.app.devices.PairingOrchestrator.JoinRecoveryOutcome,
    ) {
        val target = mutableState.value.deviceRestoreTarget
            ?: throw AuthClientArgError("no resolved account — look up your username first")
        val token = outcome.sessionToken
        val expiry = outcome.sessionExpiresAtMs
        if (token == null || expiry == null) {
            // Seed-only outcome: the grant redeem failed, so this
            // device has IDENTITY but no ACCESS. Refuse rather than
            // leave a half-restored profile; the user retries (a
            // fresh pairing mints a fresh grant).
            outcome.phraseSeed.fill(0)
            throw AuthClientArgError(
                "The other device transferred your identity but the " +
                    "sign-in grant could not be redeemed. Try again.",
            )
        }
        repo.completeFromDeviceRestore(
            accountIdBytes = target.accountIdBytes,
            accountIdB64 = target.accountIdB64,
            username = target.username,
            phraseSeed = outcome.phraseSeed,
            sessionToken = token,
            sessionExpiresAtMs = expiry,
        )
    }

    public fun onPasswordChange(value: String) {
        mutableState.update { it.copy(password = value) }
    }

    public fun onConfirmChange(value: String) {
        mutableState.update { it.copy(confirm = value) }
    }

    /**
     * Submit the recover form.
     *
     * Validates locally (new password ≥ 8, confirm matches),
     * drives [AuthRepository.recover] in `viewModelScope`,
     * surfaces success as an [AuthState.Session] transition (the
     * top-level Activity router catches it via
     * `collectAsStateWithLifecycle`), surfaces failures as inline
     * form messages.
     *
     * The mnemonic-parse failure (bad checksum / unknown word)
     * collapses to one user-facing message — same posture as the
     * web side, preserves BIP39's no-probing property.
     */
    public fun onSubmit() {
        val snapshot = mutableState.value
        if (snapshot.busy) return  // ignore double-tap

        if (snapshot.password != snapshot.confirm) {
            mutableState.update {
                it.copy(error = "The passwords do not match. Type the same password twice.")
            }
            return
        }
        if (snapshot.password.length < MIN_PASSWORD_LEN) {
            mutableState.update {
                it.copy(
                    error = "Use at least $MIN_PASSWORD_LEN characters for the new password.",
                )
            }
            return
        }
        // 1.21c3 — resolve the phrase based on input mode.
        // Shares mode reconstructs the canonical phrase via
        // `ShamirBridge.combineSharesToMnemonic` before falling
        // through to the existing OPAQUE recovery flow.
        val resolvedPhrase: String = when (snapshot.mode) {
            RecoverMode.Phrase -> {
                if (snapshot.phrase.isBlank()) {
                    mutableState.update {
                        it.copy(error = "Enter all 24 words first.")
                    }
                    return
                }
                snapshot.phrase
            }
            RecoverMode.Shares -> {
                val lines = snapshot.sharesInput
                    .lines()
                    .map { it.trim() }
                    .filter { it.isNotEmpty() }
                if (lines.size < 2) {
                    mutableState.update {
                        it.copy(
                            error = "Paste at least three pieces, one per line.",
                        )
                    }
                    return
                }
                try {
                    ShamirBridge.combineSharesToMnemonic(lines)
                } catch (e: ShamirBridgeException) {
                    mutableState.update {
                        it.copy(
                            busy = false,
                            error = "These pieces do not fit together. Check each line and try again.",
                        )
                    }
                    return
                }
            }
            RecoverMode.AnotherDevice -> {
                // ADR-0018 — the AnotherDevice mode never reaches the
                // OPAQUE submit: the RestoreViaDevicePanel drives
                // completeFromDeviceRestore directly. Defensive no-op.
                mutableState.update {
                    it.copy(error = "Scan the code with your other device to continue.")
                }
                return
            }
            RecoverMode.Authenticator -> {
                // ADR-0018 factor (c) — the Authenticator mode submits
                // via [onSubmitTotp] (no new password is set on that
                // path). Defensive no-op.
                mutableState.update {
                    it.copy(error = "Enter the code from your authenticator app.")
                }
                return
            }
            RecoverMode.Contact -> {
                // ADR-0018 factor (d) — the Contact mode submits via
                // [onSendContactCode] / [onSubmitContactCode] (no new
                // password is set on that path). Defensive no-op.
                mutableState.update {
                    it.copy(error = "Enter the code that was sent to you.")
                }
                return
            }
        }

        // ADR-0048 §4 — a typed username must pass the rule before the
        // fresh-device path resolves it through discovery.
        val typedUsername = snapshot.username.trim()
        val recoverUsername = if (typedUsername.isEmpty()) {
            null
        } else {
            usernameOrRefuse(typedUsername) ?: return
        }
        mutableState.update { it.copy(busy = true, error = null) }
        viewModelScope.launch {
            val passwordBytes = snapshot.password.toByteArray(Charsets.UTF_8)
            try {
                // ADR-0017: pass the typed username so a FRESH-device
                // recover can resolve the account_id via discovery.
                // `null` when blank — the cached id wins on a device
                // that already has an account.
                repo.recover(
                    resolvedPhrase,
                    passwordBytes,
                    username = recoverUsername,
                )
                // `repo.recover` already flipped AuthState to
                // Session. The screen will be replaced by the
                // home destination on the next recomposition;
                // wipe the textfield buffers so they don't hold
                // the phrase / password past the navigation.
                mutableState.update {
                    it.copy(
                        busy = false,
                        error = null,
                        phrase = "",
                        sharesInput = "",
                        username = "",
                        password = "",
                        confirm = "",
                    )
                }
            } catch (err: AuthClientArgError) {
                mutableState.update {
                    it.copy(busy = false, error = err.message ?: "Invalid input.")
                }
            } catch (_: FfiException.Mnemonic) {
                // Unknown word + bad checksum both flow here via
                // `RecoveryMnemonic.fromPhrase`. Generic copy is
                // intentional — the BIP39 spec keeps these
                // indistinguishable.
                mutableState.update {
                    it.copy(
                        busy = false,
                        error = "That is not a valid phrase. Check the spelling and order of all 24 words.",
                    )
                }
            } catch (err: AuthHttpError) {
                mutableState.update {
                    it.copy(busy = false, error = userFacingHttpError(err))
                }
            } catch (_: AuthNetworkError) {
                mutableState.update {
                    it.copy(
                        busy = false,
                        error = "No connection. Check your network and try again.",
                    )
                }
            } catch (err: Throwable) {
                mutableState.update {
                    it.copy(busy = false, error = err.message ?: "Could not recover the account. Try again.")
                }
            } finally {
                passwordBytes.fill(0)
            }
        }
    }

    /**
     * ADR-0018 factor (c) — submit the Authenticator mode: username
     * (cached account wins, else the typed field) + the current
     * 6-digit code. [AuthRepository.recoverWithTotp] drives the state
     * flip — [me.lakko.app.auth.AuthState.Session] on the warm
     * same-device path, [me.lakko.app.auth.AuthState.NeedsRecoveryPhrase]
     * on a fresh device — and MainActivity routes automatically.
     */
    public fun onSubmitTotp() {
        val snapshot = mutableState.value
        if (snapshot.busy) return  // ignore double-tap

        val typedUser = (cachedUsername ?: snapshot.username).trim()
        if (typedUser.isEmpty()) {
            mutableState.update {
                it.copy(error = "Enter your username first.")
            }
            return
        }
        val user = usernameOrRefuse(typedUser) ?: return
        val normalized = normalizeTotpCode(snapshot.totpCode)
        if (normalized == null) {
            mutableState.update {
                it.copy(
                    error = "Enter the six-digit code from your authenticator app.",
                )
            }
            return
        }

        mutableState.update { it.copy(busy = true, error = null) }
        viewModelScope.launch {
            try {
                repo.recoverWithTotp(user, normalized)
                // The repository flipped the auth state; wipe the form
                // buffers before the router replaces this screen.
                mutableState.update {
                    it.copy(busy = false, error = null, totpCode = "")
                }
            } catch (err: AuthClientArgError) {
                mutableState.update {
                    it.copy(busy = false, error = err.message ?: "Invalid input.")
                }
            } catch (err: AuthHttpError) {
                mutableState.update {
                    it.copy(busy = false, error = totpUserFacingHttpError(err))
                }
            } catch (_: AuthNetworkError) {
                mutableState.update {
                    it.copy(
                        busy = false,
                        error = "No connection. Check your network and try again.",
                    )
                }
            } catch (err: Throwable) {
                mutableState.update {
                    it.copy(busy = false, error = err.message ?: "Could not recover the account. Try again.")
                }
            }
        }
    }

    /**
     * ADR-0018 factor (d), step 1 — send a one-time code to a
     * previously-enrolled phone / email. Username (cached account
     * wins, else the typed field) + the chosen kind + contact.
     * [AuthRepository.recoverWithContactStart] is ENUMERATION-SAFE:
     * it returns the same result whether or not the contact matches a
     * confirmed row, so on success we ALWAYS advance to the code step.
     */
    public fun onSendContactCode() {
        val snapshot = mutableState.value
        if (snapshot.busy) return  // ignore double-tap

        val typedUser = (cachedUsername ?: snapshot.username).trim()
        if (typedUser.isEmpty()) {
            mutableState.update {
                it.copy(error = "Enter your username first.")
            }
            return
        }
        val user = usernameOrRefuse(typedUser) ?: return
        // Light client-side shape check — saves a PoW solve on an
        // obviously-malformed contact. The server is authoritative.
        if (!isPlausibleContact(snapshot.contactKind, snapshot.contact)) {
            mutableState.update {
                it.copy(
                    error = if (snapshot.contactKind == ContactKind.EMAIL) {
                        "That does not look like an email address. Check it and try again."
                    } else {
                        "Enter the phone number with its country code, " +
                            "like +15551234567."
                    },
                )
            }
            return
        }

        mutableState.update { it.copy(busy = true, error = null) }
        viewModelScope.launch {
            try {
                repo.recoverWithContactStart(
                    user,
                    snapshot.contactKind,
                    snapshot.contact,
                )
                // Enumeration-safe: advance regardless of whether the
                // contact matched — the server reveals nothing either
                // way.
                mutableState.update {
                    it.copy(
                        busy = false,
                        error = null,
                        contactStep = ContactRecoverStep.Code,
                        contactCode = "",
                    )
                }
            } catch (err: AuthClientArgError) {
                mutableState.update {
                    it.copy(busy = false, error = err.message ?: "Invalid input.")
                }
            } catch (err: AuthHttpError) {
                mutableState.update {
                    it.copy(busy = false, error = contactUserFacingHttpError(err))
                }
            } catch (_: AuthNetworkError) {
                mutableState.update {
                    it.copy(
                        busy = false,
                        error = "No connection. Check your network and try again.",
                    )
                }
            } catch (err: Throwable) {
                mutableState.update {
                    it.copy(busy = false, error = err.message ?: "Could not recover the account. Try again.")
                }
            }
        }
    }

    /**
     * ADR-0018 factor (d), step 2 — submit the delivered code.
     * [AuthRepository.recoverWithContactFinish] drives the state flip
     * (Session on the warm same-device path, NeedsRecoveryPhrase on a
     * fresh device) and MainActivity routes automatically.
     */
    public fun onSubmitContactCode() {
        val snapshot = mutableState.value
        if (snapshot.busy) return  // ignore double-tap

        val normalized = normalizeTotpCode(snapshot.contactCode)
        if (normalized == null) {
            mutableState.update {
                it.copy(error = "Enter the six-digit code that was sent to you.")
            }
            return
        }

        mutableState.update { it.copy(busy = true, error = null) }
        viewModelScope.launch {
            try {
                repo.recoverWithContactFinish(normalized)
                // The repository flipped the auth state; wipe the form
                // buffers before the router replaces this screen.
                mutableState.update {
                    it.copy(busy = false, error = null, contactCode = "")
                }
            } catch (err: AuthClientArgError) {
                mutableState.update {
                    it.copy(busy = false, error = err.message ?: "Invalid input.")
                }
            } catch (err: AuthHttpError) {
                mutableState.update {
                    it.copy(busy = false, error = contactUserFacingHttpError(err))
                }
            } catch (_: AuthNetworkError) {
                mutableState.update {
                    it.copy(
                        busy = false,
                        error = "No connection. Check your network and try again.",
                    )
                }
            } catch (err: Throwable) {
                mutableState.update {
                    it.copy(busy = false, error = err.message ?: "Could not recover the account. Try again.")
                }
            }
        }
    }

    /** Map a phone/email-OTP recovery failure onto a friendly inline
     *  message. The server is enumeration-safe: 401 covers unknown-
     *  account / no-live-challenge / wrong-code / replayed-code
     *  uniformly (mirror of the web `contactRecoverErrorMessage`). */
    private fun contactUserFacingHttpError(err: AuthHttpError): String =
        when (err.status) {
            401 -> "That code is wrong or has expired. Request a new one."
            429 -> "Too many attempts. Wait a while, then request a new code."
            503 -> "Email and phone recovery are not available right now. Try another method."
            400 -> "Check the address or number and try again."
            in 500..599 -> "Something went wrong on our side. Try again in a moment."
            else -> "Could not recover the account. Try again."
        }

    /** Map a `recoverWithTotp` failure onto a friendly inline message.
     *  The server is enumeration-safe: 401 covers unknown-account /
     *  unconfirmed-enrollment / wrong-code / replayed-code uniformly
     *  (mirror of the web `totpRecoverErrorMessage`). */
    private fun totpUserFacingHttpError(err: AuthHttpError): String =
        when (err.status) {
            401 -> "That code is wrong or has expired. Try the next one."
            429 -> "Too many attempts. Wait 15 minutes, then try again."
            503 -> "Authenticator recovery is not available right now. Try another method."
            in 500..599 -> "Something went wrong on our side. Try again in a moment."
            else -> "Could not recover the account. Try again."
        }

    private fun userFacingHttpError(err: AuthHttpError): String =
        when (err.status) {
            // The server's `/password/reset/{start,finish}` pair
            // returns 401 on a wrong mnemonic (the OPAQUE recovery
            // login fails at AKE finalisation). Surface generic
            // copy — wrong-mnemonic and wrong-account collapse
            // into the same response by design.
            401 -> "These words did not unlock the account. Check them and try again."
            429 -> "Too many attempts. Wait a moment, then try again."
            in 500..599 -> "Something went wrong on our side. Try again in a moment."
            else -> "Could not recover the account. Try again."
        }

    public companion object {
        /** Mirror of [SignupViewModel.MIN_PASSWORD_LEN]. The
         *  recovery flow sets a **new** password so the same
         *  floor applies. */
        public const val MIN_PASSWORD_LEN: Int = 8
    }
}

/** 1.21c3 — which input mode the user is filling. ADR-0018 adds
 *  [AnotherDevice]: restore via an already-linked device (no
 *  password, no phrase typing); [Authenticator]: recover ACCESS with
 *  a 6-digit RFC 6238 code (factor (c)); and [Contact]: recover
 *  ACCESS with a one-time code sent to a previously-enrolled phone /
 *  email (factor (d)). */
public enum class RecoverMode { Phrase, Shares, AnotherDevice, Authenticator, Contact }

/** ADR-0018 factor (d) — the two steps of the phone/email recovery
 *  flow. [Request] collects the username + contact and sends a code;
 *  [Code] collects the 6-digit code and finishes. */
public enum class ContactRecoverStep { Request, Code }

/** ADR-0018 factor (a), path (b) — the resolved restore target the
 *  AnotherDevice mode pairs against. */
public data class DeviceRestoreTarget(
    val accountIdBytes: ByteArray,
    val accountIdB64: String,
    val username: String,
) {
    override fun equals(other: Any?): Boolean {
        if (this === other) return true
        if (other !is DeviceRestoreTarget) return false
        return accountIdBytes.contentEquals(other.accountIdBytes) &&
            accountIdB64 == other.accountIdB64 &&
            username == other.username
    }

    override fun hashCode(): Int {
        var h = accountIdBytes.contentHashCode()
        h = 31 * h + accountIdB64.hashCode()
        h = 31 * h + username.hashCode()
        return h
    }
}

/** Snapshot of every field the [RecoverScreen] renders. */
public data class RecoverUiState(
    val mode: RecoverMode = RecoverMode.Phrase,
    val phrase: String = "",
    /** Shamir-shares textarea content (one share per line). */
    val sharesInput: String = "",
    /** ADR-0017 — username for the fresh-device recover path; only
     *  rendered + consumed when no account is cached on this device. */
    val username: String = "",
    val password: String = "",
    val confirm: String = "",
    val busy: Boolean = false,
    val error: String? = null,
    /** ADR-0018 — non-null once the AnotherDevice mode resolved the
     *  typed username to an account id via discovery. */
    val deviceRestoreTarget: DeviceRestoreTarget? = null,
    /** ADR-0018 factor (c) — the 6-digit authenticator code field. */
    val totpCode: String = "",
    /** ADR-0018 factor (d) — which step of the phone/email recovery
     *  flow the Contact mode is on. */
    val contactStep: ContactRecoverStep = ContactRecoverStep.Request,
    /** ADR-0018 factor (d) — [ContactKind.EMAIL] / [ContactKind.PHONE]
     *  toggle for the Contact mode. */
    val contactKind: Int = me.lakko.app.auth.ContactKind.EMAIL,
    /** ADR-0018 factor (d) — the phone / email the code is sent to. */
    val contact: String = "",
    /** ADR-0018 factor (d) — the 6-digit code delivered to the
     *  contact. */
    val contactCode: String = "",
)
