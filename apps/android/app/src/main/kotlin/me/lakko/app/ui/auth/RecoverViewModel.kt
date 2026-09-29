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
import me.lakko.app.l10n.UiText
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
    /** ADR-0056 — username to login handle for the AnotherDevice mode's
     *  pairing payload (the native core). Overridable for tests. */
    private val loginHandleSource: me.lakko.app.auth.LoginHandleSource =
        me.lakko.app.auth.UniffiLoginHandleSource,
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
                mutableState.update { it.copy(error = decision.text) }
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
     * ADR-0018 factor (a), path (b) — start the AnotherDevice restore:
     * the typed username (or the account cached on this device) becomes
     * the login handle the pairing payload carries (ADR-0056). Nothing is
     * looked up: the account id arrives only with the grant redeem, after
     * the other device confirms.
     */
    public fun startDeviceRestore() {
        val snapshot = mutableState.value
        if (snapshot.busy) return
        val target = try {
            deviceRestoreTargetFor(snapshot.username, cachedUsername, loginHandleSource)
        } catch (err: AuthClientArgError) {
            mutableState.update { it.copy(error = err.text ?: UiText("recovery.username_required")) }
            return
        }
        mutableState.update { it.copy(error = null, deviceRestoreTarget = target) }
    }

    /**
     * ADR-0018 factor (a), path (b) — complete the no-password
     * restore with the identity + redeemed session token received
     * from the other device. Suspend passthrough to
     * [AuthRepository.completeFromDeviceRestore]; verification
     * failures propagate to the calling panel. Requires the redeemed
     * token and the account id the redeem named (without them this
     * device has no ACCESS) — refused otherwise with a clear message.
     */
    public suspend fun completeFromDeviceRestore(
        outcome: me.lakko.app.devices.PairingOrchestrator.JoinRecoveryOutcome,
    ) {
        val target = mutableState.value.deviceRestoreTarget
        if (target == null) {
            outcome.phraseSeed.fill(0)
            throw AuthClientArgError("Enter your username first.", UiText("recovery.username_required"))
        }
        val done = deviceRestoreCompletion(target, outcome)
        repo.completeFromDeviceRestore(
            accountIdBytes = done.accountIdBytes,
            accountIdB64 = done.accountIdB64,
            username = done.username,
            phraseSeed = done.phraseSeed,
            sessionToken = done.sessionToken,
            sessionExpiresAtMs = done.sessionExpiresAtMs,
            // ADR-0056 security pass: the pairing's held writes, run by the
            // repository only after the seed verified and the typed name
            // resolved at discovery to the redeemed account.
            persistJoin = outcome.persistJoin ?: {},
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
                it.copy(error = UiText("auth.password.error.mismatch"))
            }
            return
        }
        if (snapshot.password.length < MIN_PASSWORD_LEN) {
            mutableState.update {
                it.copy(
                    error = UiText(
                        "auth.password.error.too_short",
                        counts = mapOf("count" to MIN_PASSWORD_LEN.toULong()),
                    ),
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
                        it.copy(error = UiText("recovery_phrase.error.incomplete"))
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
                            error = UiText("recovery.shares.error.too_few"),
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
                            error = UiText("recovery.shares.error.combine_failed"),
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
                    it.copy(error = UiText("recovery.error.generic"))
                }
                return
            }
            RecoverMode.Authenticator -> {
                // ADR-0018 factor (c) — the Authenticator mode submits
                // via [onSubmitTotp] (no new password is set on that
                // path). Defensive no-op.
                mutableState.update {
                    it.copy(error = UiText("recovery.totp.error.enter_code"))
                }
                return
            }
            RecoverMode.Contact -> {
                // ADR-0018 factor (d) — the Contact mode submits via
                // [onSendContactCode] / [onSubmitContactCode] (no new
                // password is set on that path). Defensive no-op.
                mutableState.update {
                    it.copy(error = UiText("recovery.contact.error.enter_code"))
                }
                return
            }
        }

        // ADR-0048 §4 — a typed username must pass the rule before its login
        // handle is sent (ADR-0054 S5).
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
                    it.copy(busy = false, error = err.text ?: UiText("recovery.error.generic"))
                }
            } catch (_: FfiException.Mnemonic) {
                // Unknown word + bad checksum both flow here via
                // `RecoveryMnemonic.fromPhrase`. Generic copy is
                // intentional — the BIP39 spec keeps these
                // indistinguishable.
                mutableState.update {
                    it.copy(
                        busy = false,
                        error = UiText("restore_identity.error.invalid_phrase"),
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
                        error = OFFLINE,
                    )
                }
            } catch (err: Throwable) {
                mutableState.update {
                    it.copy(busy = false, error = UiText("recovery.error.generic"))
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
                it.copy(error = UiText("recovery.username_required"))
            }
            return
        }
        val user = usernameOrRefuse(typedUser) ?: return
        val normalized = normalizeTotpCode(snapshot.totpCode)
        if (normalized == null) {
            mutableState.update {
                it.copy(
                    error = UiText("recovery.totp.error.enter_code"),
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
                    it.copy(busy = false, error = err.text ?: UiText("recovery.totp.error.generic"))
                }
            } catch (err: AuthHttpError) {
                mutableState.update {
                    it.copy(busy = false, error = totpUserFacingHttpError(err))
                }
            } catch (_: AuthNetworkError) {
                mutableState.update {
                    it.copy(
                        busy = false,
                        error = OFFLINE,
                    )
                }
            } catch (err: Throwable) {
                mutableState.update {
                    it.copy(busy = false, error = UiText("recovery.totp.error.generic"))
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
                it.copy(error = UiText("recovery.username_required"))
            }
            return
        }
        val user = usernameOrRefuse(typedUser) ?: return
        // Light client-side shape check — saves a round trip on an
        // obviously-malformed contact. The server is authoritative.
        if (!isPlausibleContact(snapshot.contactKind, snapshot.contact)) {
            mutableState.update {
                it.copy(
                    error = if (snapshot.contactKind == ContactKind.EMAIL) {
                        UiText("recovery.contact.error.invalid_email")
                    } else {
                        UiText("recovery.contact.error.invalid_phone")
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
                    it.copy(busy = false, error = err.text ?: UiText("recovery.contact.error.generic"))
                }
            } catch (err: AuthHttpError) {
                mutableState.update {
                    it.copy(busy = false, error = contactUserFacingHttpError(err))
                }
            } catch (_: AuthNetworkError) {
                mutableState.update {
                    it.copy(
                        busy = false,
                        error = OFFLINE,
                    )
                }
            } catch (err: Throwable) {
                mutableState.update {
                    it.copy(busy = false, error = UiText("recovery.contact.error.generic"))
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
                it.copy(error = UiText("recovery.contact.error.enter_code"))
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
                    it.copy(busy = false, error = err.text ?: UiText("recovery.contact.error.generic"))
                }
            } catch (err: AuthHttpError) {
                mutableState.update {
                    it.copy(busy = false, error = contactUserFacingHttpError(err))
                }
            } catch (_: AuthNetworkError) {
                mutableState.update {
                    it.copy(
                        busy = false,
                        error = OFFLINE,
                    )
                }
            } catch (err: Throwable) {
                mutableState.update {
                    it.copy(busy = false, error = UiText("recovery.contact.error.generic"))
                }
            }
        }
    }

    /** Map a phone/email-OTP recovery failure onto a friendly inline
     *  message. The server is enumeration-safe: 401 covers unknown-
     *  account / no-live-challenge / wrong-code / replayed-code
     *  uniformly (mirror of the web `contactRecoverErrorMessage`). */
    private fun contactUserFacingHttpError(err: AuthHttpError): UiText =
        me.lakko.app.auth.authLimitText(err) ?: if (me.lakko.app.auth.isLoginByNameDisabled(err)) {
            UiText("common.error.unavailable")
        } else when (err.status) {
            401 -> UiText("recovery.contact.error.wrong")
            429 -> UiText("common.error.too_many_attempts")
            503 -> UiText("recovery.contact.error.unavailable")
            400 -> UiText("recovery.contact.error.bad_contact")
            else -> UiText("recovery.contact.error.generic")
        }

    /** Map a `recoverWithTotp` failure onto a friendly inline message.
     *  The server is enumeration-safe: 401 covers unknown-account /
     *  unconfirmed-enrollment / wrong-code / replayed-code uniformly
     *  (mirror of the web `totpRecoverErrorMessage`). */
    private fun totpUserFacingHttpError(err: AuthHttpError): UiText =
        me.lakko.app.auth.authLimitText(err) ?: if (me.lakko.app.auth.isLoginByNameDisabled(err)) {
            UiText("common.error.unavailable")
        } else when (err.status) {
            401 -> UiText(if (cachedUsername == null) "recovery.totp.error.wrong_username" else "recovery.totp.error.wrong")
            429 -> UiText("common.error.too_many_attempts")
            503 -> UiText("recovery.totp.error.unavailable")
            else -> UiText("recovery.totp.error.generic")
        }

    private fun userFacingHttpError(err: AuthHttpError): UiText =
        me.lakko.app.auth.authLimitText(err) ?: when (err.status) {
            // The server's `/password/reset/{start,finish}` pair
            // returns 401 on a wrong mnemonic (the OPAQUE recovery
            // login fails at AKE finalisation). Surface generic
            // copy — wrong-mnemonic and wrong-account collapse
            // into the same response by design.
            401, 404, 400 -> UiText("recovery.error.no_match")
            429 -> UiText("common.error.too_many_attempts")
            503 -> UiText("common.error.unavailable")
            else -> UiText("recovery.error.generic")
        }

    public companion object {
        /** Mirror of [SignupViewModel.MIN_PASSWORD_LEN]. The
         *  recovery flow sets a **new** password so the same
         *  floor applies. */
        public const val MIN_PASSWORD_LEN: Int = 8

        private val OFFLINE = UiText("common.error.offline")
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

/** ADR-0018 factor (a), path (b) — what the AnotherDevice mode pairs
 *  against: the canonical username and its 32-byte login handle
 *  (ADR-0056). No account id: the fresh device learns it from the
 *  grant redeem. */
public class DeviceRestoreTarget(
    public val username: String,
    public val loginHandle: ByteArray,
) {
    override fun equals(other: Any?): Boolean {
        if (this === other) return true
        if (other !is DeviceRestoreTarget) return false
        return username == other.username && loginHandle.contentEquals(other.loginHandle)
    }

    override fun hashCode(): Int = 31 * username.hashCode() + loginHandle.contentHashCode()
}

/**
 * The [DeviceRestoreTarget] for the name in the username field ([typed]),
 * else the account cached on this device ([cachedUsername]).
 *
 * @throws AuthClientArgError with the form copy (no name, or the username
 *   rule's reason).
 */
public fun deviceRestoreTargetFor(
    typed: String,
    cachedUsername: String?,
    source: me.lakko.app.auth.LoginHandleSource,
): DeviceRestoreTarget {
    val named = me.lakko.app.devices.pairingNameFor(typed, cachedUsername, source)
    return DeviceRestoreTarget(username = named.username, loginHandle = named.handle)
}

/** The inputs of [AuthRepository.completeFromDeviceRestore], checked. */
public class DeviceRestoreCompletion(
    public val accountIdBytes: ByteArray,
    public val accountIdB64: String,
    public val username: String,
    public val phraseSeed: ByteArray,
    public val sessionToken: String,
    public val sessionExpiresAtMs: Long,
)

/**
 * Check a join-by-name outcome can complete the restore: the grant was
 * redeemed, so the session and the account id (from the redeem response,
 * ADR-0056) are present. A seed-only outcome zeroizes the seed and throws.
 */
public fun deviceRestoreCompletion(
    target: DeviceRestoreTarget,
    outcome: me.lakko.app.devices.PairingOrchestrator.JoinRecoveryOutcome,
): DeviceRestoreCompletion {
    val token = outcome.sessionToken
    val expiry = outcome.sessionExpiresAtMs
    val accountId = outcome.accountId
    if (token == null || expiry == null || accountId == null) {
        // Seed-only outcome: the grant redeem failed, so this device has
        // IDENTITY but no ACCESS and no account id. Refuse rather than
        // leave a half-restored profile; the user retries (a fresh
        // pairing mints a fresh grant).
        outcome.phraseSeed.fill(0)
        throw AuthClientArgError(
            "The other device transferred your identity but the " +
                "sign-in grant could not be redeemed. Try again.",
            UiText("device_link.error.not_let_in"),
        )
    }
    return DeviceRestoreCompletion(
        accountIdBytes = accountId.copyOf(),
        accountIdB64 = me.lakko.app.auth.base64Encode(accountId),
        username = target.username,
        phraseSeed = outcome.phraseSeed,
        sessionToken = token,
        sessionExpiresAtMs = expiry,
    )
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
    /** The form's error sentence (ADR-0058: rendered from its key). */
    val error: UiText? = null,
    /** ADR-0018 — non-null once the AnotherDevice mode turned the
     *  username into the login handle its pairing code carries
     *  (ADR-0056; no lookup). */
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
