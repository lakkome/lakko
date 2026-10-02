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
import me.lakko.app.auth.normalizeTotpCode
import me.lakko.app.l10n.UiText
import uniffi.lakko_ffi_uniffi.FfiException

/**
 * State + dispatcher for the [RecoverScreen]: the list of ways back in and
 * one form per way. Mirror of `apps/web/src/routes/Recover.tsx` and the
 * forms under `apps/web/src/auth/recover/`.
 *
 *   - phrase: the 12 words plus a new password ([AuthRepository.recover]);
 *   - another device: [AuthRepository.completeFromDeviceRestore];
 *   - passkey: [AuthRepository.loginWithPasskey], no username needed;
 *   - three words: checked on this device against the phrase it holds
 *     ([AuthRepository.recoverWithThreeWords]), with the local guess budget;
 *   - authenticator code: [AuthRepository.recoverWithTotp].
 *
 * Every success flips the auth state and the app root replaces the screen.
 * [cachedUsername] is the account this device remembers, or `null`.
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
    /** Draws the three word numbers of one prompt (the core's
     *  `phraseCheckRandomPositions`: distinct, 1..=12, in drawn order). */
    private val drawPositions: () -> List<Int> =
        { uniffi.lakko_ffi_uniffi.phraseCheckRandomPositions().map { it.toInt() and 0xFF } },
    private val nowMs: () -> Long = System::currentTimeMillis,
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

    /** ADR-0018 factor (c) — 6-digit authenticator code field. */
    public fun onTotpCodeChange(value: String) {
        mutableState.update { it.copy(totpCode = value) }
    }

    /** Switch the recovery method being filled. Clears the resolved restore
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
            outcome.phraseEntropy.fill(0)
            throw AuthClientArgError("Enter your username first.", UiText("recovery.username_required"))
        }
        val done = deviceRestoreCompletion(target, outcome)
        repo.completeFromDeviceRestore(
            accountIdBytes = done.accountIdBytes,
            accountIdB64 = done.accountIdB64,
            username = done.username,
            phraseEntropy = done.phraseEntropy,
            sessionToken = done.sessionToken,
            sessionExpiresAtMs = done.sessionExpiresAtMs,
            // ADR-0056 security pass: the pairing's held writes, run by the
            // repository only after the seed verified and the typed name
            // resolved at discovery to the redeemed account.
            persistJoin = outcome.persistJoin ?: {},
            // Reached from the recovery list: offer a new password once the
            // session lands.
            forgotPassword = true,
        )
    }

    // ---- three words ---------------------------------------------------

    /**
     * The account the three-word row signs in, or `null` when the row is not
     * offered on this device. Local facts only; reads Keystore-backed
     * storage, so the screen calls it off the main thread.
     */
    public fun threeWordsAccount(): me.lakko.app.auth.ThreeWordsAccount? = repo.threeWordsAccount()

    /** Open the three-word form for [account]: read its budget and draw the
     *  first three positions. */
    public fun openWords(account: me.lakko.app.auth.ThreeWordsAccount) {
        val budget = me.lakko.app.auth.budgetAt(repo.threeWordsBudget(account.accountIdB64), nowMs())
        mutableState.update {
            it.copy(
                error = null,
                words = ThreeWordsUi(
                    accountIdB64 = account.accountIdB64,
                    username = account.username,
                    positions = drawPositions(),
                    budget = budget,
                ),
            )
        }
    }

    public fun onWordChange(index: Int, value: String) {
        mutableState.update { state ->
            val words = state.words ?: return@update state
            if (index !in words.typed.indices) return@update state
            state.copy(words = words.copy(typed = words.typed.toMutableList().also { it[index] = value }))
        }
    }

    /**
     * Submit the three words. The stored budget is re-read first; while the
     * route is paused nothing is checked. A miss is counted, persisted, and
     * answered with NEW positions and empty fields; nothing is sent
     * anywhere. A match signs in (the repository flips the auth state) and
     * clears the budget. A failure after a match never charges the budget.
     */
    public fun onSubmitWords() {
        val snapshot = mutableState.value
        val words = snapshot.words ?: return
        if (snapshot.busy) return
        val now = nowMs()
        val current = me.lakko.app.auth.budgetAt(repo.threeWordsBudget(words.accountIdB64), now)
        if (me.lakko.app.auth.pauseRemainingMs(current, now) > 0L) {
            mutableState.update { it.copy(error = null, words = words.copy(budget = current)) }
            return
        }
        if (!words.complete) return
        mutableState.update { it.copy(busy = true, error = null, words = words.copy(budget = current)) }
        viewModelScope.launch {
            try {
                val matched = repo.recoverWithThreeWords(
                    positions = ByteArray(words.positions.size) { words.positions[it].toByte() },
                    words = words.typed,
                )
                if (!matched) {
                    val next = repo.recordThreeWordsMiss(words.accountIdB64, nowMs())
                    mutableState.update {
                        it.copy(
                            busy = false,
                            error = UiText("recovery.words.error.no_match"),
                            // New positions for every prompt.
                            words = words.copy(
                                positions = drawPositions(),
                                typed = List(me.lakko.app.auth.THREE_WORDS) { "" },
                                budget = next,
                            ),
                        )
                    }
                    return@launch
                }
                // Signed in: the app root shows "Choose a new password" next.
                mutableState.update { it.copy(busy = false, error = null, words = null) }
            } catch (err: Throwable) {
                if (err is kotlinx.coroutines.CancellationException) throw err
                val failure = me.lakko.app.auth.threeWordsFailureOf(err)
                mutableState.update { state ->
                    val w = state.words ?: words
                    when (failure) {
                        is me.lakko.app.auth.ThreeWordsFailure.RateLimited -> state.copy(
                            busy = false,
                            words = w.copy(serverWaitUntilMs = nowMs() + failure.retryAfterSecs * 1000L),
                        )
                        me.lakko.app.auth.ThreeWordsFailure.Unavailable ->
                            state.copy(busy = false, words = w.copy(unavailable = true))
                        me.lakko.app.auth.ThreeWordsFailure.Rejected ->
                            state.copy(busy = false, error = UiText("recovery.words.error.rejected"))
                        // The route is now suspended on this device (nothing
                        // was deleted): the form closes on the explanation,
                        // and the list reads the row again (it is gone).
                        me.lakko.app.auth.ThreeWordsFailure.Suspended -> state.copy(
                            busy = false,
                            error = null,
                            words = w.copy(suspended = true),
                            wordsRevision = state.wordsRevision + 1,
                        )
                        // The phrase stored here is not registered yet (a
                        // replace is owed); nothing was removed.
                        me.lakko.app.auth.ThreeWordsFailure.CredentialPending -> state.copy(
                            busy = false,
                            error = UiText("recovery_phrase.fresh.credential_pending"),
                        )
                        me.lakko.app.auth.ThreeWordsFailure.Offline -> state.copy(busy = false, error = OFFLINE)
                        is me.lakko.app.auth.ThreeWordsFailure.Sentence ->
                            state.copy(busy = false, error = failure.text)
                        me.lakko.app.auth.ThreeWordsFailure.Generic ->
                            state.copy(busy = false, error = UiText("recovery.words.error.generic"))
                    }
                }
            }
        }
    }

    // ---- passkey ---------------------------------------------------------

    /**
     * Sign in with a passkey. No username is needed: the system sheet lists
     * the passkeys saved for Lakko. A device that holds the keys lands
     * signed in; a passkey that carries the keys restores them; otherwise
     * the restore prompt opens and says the passkey brought no keys.
     * [activityContext] hosts the sheet.
     */
    public fun onSubmitPasskey(activityContext: android.content.Context) {
        val snapshot = mutableState.value
        if (snapshot.busy) return
        mutableState.update { it.copy(busy = true, error = null) }
        viewModelScope.launch {
            try {
                repo.loginWithPasskey(
                    activityContext = activityContext,
                    usernameHint = snapshot.username.takeIf { it.isNotBlank() } ?: cachedUsername,
                    forgotPassword = true,
                )
                mutableState.update { it.copy(busy = false, error = null) }
            } catch (_: me.lakko.app.auth.PasskeyCancelled) {
                // Dismissing the sheet is not an error: back to idle.
                mutableState.update { it.copy(busy = false) }
            } catch (err: AuthClientArgError) {
                mutableState.update {
                    it.copy(
                        busy = false,
                        error = err.text ?: UiText("auth.passkey.error.generic"),
                        passkeyUnavailable = err.text?.key == "auth.passkey.error.server_disabled",
                    )
                }
            } catch (err: Throwable) {
                if (err is kotlinx.coroutines.CancellationException) throw err
                mutableState.update { it.copy(busy = false, error = UiText("auth.passkey.error.generic")) }
            }
        }
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
        // Only the Phrase mode reaches the OPAQUE recovery flow below.
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
            RecoverMode.Passkey, RecoverMode.Words -> {
                // These sign in without a new password ([onSubmitPasskey],
                // [onSubmitWords]). Defensive no-op.
                mutableState.update { it.copy(error = UiText("recovery.error.generic")) }
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

/**
 * Which way back in the user is filling.
 *
 * Bring back the chats: [AnotherDevice] (a device that is still signed in
 * hands the keys over), [Passkey] (signs in; restores the keys too when
 * that was turned on for the passkey), [Phrase] (the 12 words plus a new
 * password).
 *
 * Sign in on this device only: [Words] (three words of the phrase, checked
 * on this device; listed only where the device holds the phrase and the
 * keys), [Authenticator] (a six-digit code).
 */
public enum class RecoverMode { Phrase, AnotherDevice, Passkey, Words, Authenticator }

/** The three-word form. */
public data class ThreeWordsUi(
    val accountIdB64: String,
    val username: String,
    /** The three word numbers of this prompt, in the order drawn. */
    val positions: List<Int>,
    /** `typed[i]` answers `positions[i]`. */
    val typed: List<String> = List(me.lakko.app.auth.THREE_WORDS) { "" },
    val budget: me.lakko.app.auth.ThreeWordsBudget = me.lakko.app.auth.ThreeWordsBudget.FRESH,
    /** A server limit on the sign-in after a match: when it ends (unix ms). */
    val serverWaitUntilMs: Long? = null,
    /** The route is switched off on this server. */
    val unavailable: Boolean = false,
    /** The sign-in with the stored phrase was refused and the route is now
     *  suspended: the form shows why and offers the way back to the list. */
    val suspended: Boolean = false,
) {
    /** Whether all three fields hold a word of the phrase word list. */
    val complete: Boolean
        get() = me.lakko.app.auth.threeWordsSubmittable(typed)
}

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
    public val phraseEntropy: ByteArray,
    public val sessionToken: String,
    public val sessionExpiresAtMs: Long,
)

/**
 * Check a join-by-name outcome can complete the restore: the grant was
 * redeemed, so the session and the account id (from the redeem response,
 * ADR-0056) are present. A keys-only outcome zeroizes the entropy and throws.
 */
public fun deviceRestoreCompletion(
    target: DeviceRestoreTarget,
    outcome: me.lakko.app.devices.PairingOrchestrator.JoinRecoveryOutcome,
): DeviceRestoreCompletion {
    val token = outcome.sessionToken
    val expiry = outcome.sessionExpiresAtMs
    val accountId = outcome.accountId
    if (token == null || expiry == null || accountId == null) {
        // Keys-only outcome: the grant redeem failed, so this device has
        // the keys but no session and no account id. Refuse rather than
        // leave a half-restored profile; the user retries (a fresh
        // pairing mints a fresh grant).
        outcome.phraseEntropy.fill(0)
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
        phraseEntropy = outcome.phraseEntropy,
        sessionToken = token,
        sessionExpiresAtMs = expiry,
    )
}

/** Snapshot of every field the [RecoverScreen] renders. */
public data class RecoverUiState(
    val mode: RecoverMode = RecoverMode.Phrase,
    val phrase: String = "",
    /** ADR-0017 — username for the fresh-device recover path; only
     *  rendered + consumed when no account is cached on this device. */
    val username: String = "",
    val password: String = "",
    val confirm: String = "",
    val busy: Boolean = false,
    /** The form's error sentence (ADR-0061: rendered from its key). */
    val error: UiText? = null,
    /** ADR-0018 — non-null once the AnotherDevice mode turned the
     *  username into the login handle its pairing code carries
     *  (ADR-0056; no lookup). */
    val deviceRestoreTarget: DeviceRestoreTarget? = null,
    /** ADR-0018 factor (c) — the 6-digit authenticator code field. */
    val totpCode: String = "",
    /** The three-word form, once opened. */
    val words: ThreeWordsUi? = null,
    /** Bumped when what this device offers for three words may have changed
     *  (the route was suspended), so the list reads the row again. */
    val wordsRevision: Int = 0,
    /** Passkeys are switched off on this server: the form offers the list. */
    val passkeyUnavailable: Boolean = false,
)
