// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.auth

import android.app.Activity
import android.content.Context
import android.content.ContextWrapper
import android.os.Build
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.Stable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.unit.dp
import androidx.compose.ui.platform.LocalContext
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import androidx.lifecycle.viewmodel.compose.viewModel
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import me.lakko.app.auth.AuthRepository
import me.lakko.app.auth.formatCountdown
import me.lakko.app.l10n.t
import me.lakko.app.l10n.text

/**
 * The state holder of one proof block (`docs/ux/identity-recovery.md`
 * section 17.1). One per surface; it lives as long as the surface is open
 * and holds the typed password in memory only (never saved instance state).
 * It is kept by a ViewModel ([ProofHolder]) so a rotation does not lose what
 * was typed; back, cancel and an accepted proof clear it.
 * Every decision is [proofBlockView] / [afterProofFailure]; this only runs
 * the two proofs against [AuthRepository] and moves the state.
 */
@Stable
public class ProofController(
    private val repo: AuthRepository,
    public val surface: ProofSurface,
) {
    public var state: ProofBlockState by mutableStateOf(ProofBlockState())
        private set

    /** Whether a fresh-auth proof is live; re-read by [refresh]. */
    public var proofLive: Boolean by mutableStateOf(repo.freshProofLive())
        private set

    /** Whether the account has a passkey; `null` until [load] answered. */
    public var hasPasskey: Boolean? by mutableStateOf(null)
        private set

    public val usernameKnown: Boolean = repo.proofUsernameKnown()

    /** The account has a passkey AND this device can use passkeys. */
    public val hasUsablePasskey: Boolean
        get() = hasPasskey == true && PASSKEYS_SUPPORTED

    /** The block as it is now. */
    public fun view(nowMs: Long = System.currentTimeMillis()): ProofBlockView = proofBlockView(
        proofLive = proofLive,
        hasUsablePasskey = hasUsablePasskey,
        usernameKnown = usernameKnown,
        state = state,
        nowMs = nowMs,
    )

    /** Ask whether the account has a passkey (once per surface), and open on
     *  the passkey notice where the surface does. */
    public suspend fun load() {
        // Once per controller: a rotation re-attaches the same one and must
        // not move the person back to the passkey notice.
        if (hasPasskey != null) return
        val has = repo.accountHasPasskey()
        hasPasskey = has ?: false
        if (hasUsablePasskey && proofOpensOnPasskey(surface) && state.password.isEmpty()) {
            state = state.copy(mode = ProofMode.PasskeyNotice)
        }
    }

    /** Re-read whether a proof is live: one that ran out while the screen
     *  was open makes the block appear, with no error styling. */
    public fun refresh() {
        val live = repo.freshProofLive()
        if (proofLive && !live) state = afterProofRequired(state)
        proofLive = live
    }

    public fun onPasswordChange(value: String) {
        state = state.copy(password = value, line = null)
    }

    public fun onUsernameChange(value: String) {
        state = state.copy(username = value, line = null)
    }

    /** `Use your password instead` / back to the passkey notice. */
    public fun setMode(mode: ProofMode) {
        state = state.copy(mode = mode, line = null)
    }

    /**
     * Make sure a proof is live before the surface does its work: `true`
     * when one already is, or the mode's proof (the typed password, or the
     * passkey gesture) was accepted. `false` leaves the reason on the block
     * and NOTHING of the surface's action has been sent.
     */
    public suspend fun ensure(activityContext: Context): Boolean {
        refresh()
        if (proofLive) return true
        val view = view()
        return if (view.showPasskeyNotice) provePasskey(activityContext) else provePassword()
    }

    /** `Use a passkey instead`: the gesture alone satisfies the block. */
    public suspend fun provePasskey(activityContext: Context): Boolean {
        if (state.phase != ProofPhase.Idle) return false
        state = state.copy(phase = ProofPhase.PasskeySheet, line = null)
        return try {
            repo.proveWithPasskey(activityContext)
            proved()
            true
        } catch (cause: Throwable) {
            if (cause is kotlinx.coroutines.CancellationException) {
                state = state.copy(phase = ProofPhase.Idle)
                throw cause
            }
            failed(cause, viaPasskey = true)
            false
        }
    }

    private suspend fun provePassword(): Boolean {
        if (state.phase != ProofPhase.Idle) return false
        val snapshot = state
        if (snapshot.password.isEmpty()) return false
        val bytes = snapshot.password.toByteArray(Charsets.UTF_8)
        state = snapshot.copy(phase = ProofPhase.Checking, line = null)
        return try {
            repo.proveWithPassword(bytes, snapshot.username.takeIf { it.isNotBlank() })
            proved()
            true
        } catch (cause: Throwable) {
            if (cause is kotlinx.coroutines.CancellationException) {
                state = state.copy(phase = ProofPhase.Idle)
                throw cause
            }
            failed(cause, viaPasskey = false)
            false
        } finally {
            bytes.fill(0)
        }
    }

    private fun proved() {
        // The password is not kept a moment longer than the check.
        state = state.copy(phase = ProofPhase.Idle, password = "", line = null, waitUntilMs = null)
        proofLive = repo.freshProofLive()
    }

    private fun failed(cause: Throwable, viaPasskey: Boolean) {
        android.util.Log.w("lakko-proof", "the proof was not accepted: ${cause.javaClass.simpleName}")
        state = afterProofFailure(state, proofFailureOf(cause, viaPasskey), surface, System.currentTimeMillis())
        proofLive = repo.freshProofLive()
    }

    /** The surface's gated call answered `403 fresh-auth-required` (the
     *  proof ran out between the steps): the block appears, nothing else
     *  changes, no error. */
    public fun onProofRefused() {
        repo.dropFreshProof()
        proofLive = false
        state = afterProofRequired(state)
    }

    /** The surface closed: the typed password and name are not kept. */
    public fun clear() {
        state = ProofBlockState(mode = state.mode)
    }

    private companion object {
        /** Credential Manager passkeys need Android 9. */
        val PASSKEYS_SUPPORTED: Boolean = Build.VERSION.SDK_INT >= Build.VERSION_CODES.P
    }
}

/**
 * The proof controllers that are alive, by slot, and which of them wait for
 * their surface to come back after a configuration change. Pure bookkeeping
 * ([ProofHolder] adds the clock); [clear] wipes what a controller holds.
 */
public class ProofSlots<T : Any>(private val clear: (T) -> Unit) {
    private val held = mutableMapOf<String, T>()
    private val awaiting = mutableSetOf<String>()

    /** The controller of [slot], made by [create] when there is none. A
     *  surface that came back is no longer awaited. */
    public fun attach(slot: String, create: () -> T): T {
        awaiting.remove(slot)
        return held.getOrPut(slot, create)
    }

    /**
     * The surface of [slot] left the screen. Back, cancel, a closed dialog:
     * cleared and dropped now. A configuration change: kept, and awaited;
     * returns `true` so the caller starts the wait that ends in [expire].
     */
    public fun detach(slot: String, changingConfigurations: Boolean): Boolean {
        if (slot !in held) return false
        if (changingConfigurations) {
            awaiting.add(slot)
            return true
        }
        drop(slot)
        return false
    }

    /** The wait is over: a surface that did not come back keeps nothing. */
    public fun expire(slot: String) {
        if (slot in awaiting) drop(slot)
    }

    /** The owner is gone (the screen was left for good). */
    public fun clearAll() {
        held.values.forEach(clear)
        held.clear()
        awaiting.clear()
    }

    public fun holds(slot: String): Boolean = slot in held

    private fun drop(slot: String) {
        awaiting.remove(slot)
        held.remove(slot)?.let(clear)
    }
}

/**
 * Keeps a screen's proof controllers across a configuration change, in the
 * screen's ViewModel store: memory only, never saved instance state, so the
 * typed password survives a rotation and nothing else (not process death).
 */
public class ProofHolder : ViewModel() {
    private val slots = ProofSlots<ProofController> { it.clear() }

    public fun attach(slot: String, repo: AuthRepository, surface: ProofSurface): ProofController =
        slots.attach(slot) { ProofController(repo, surface) }

    public fun detach(slot: String, changingConfigurations: Boolean) {
        if (slots.detach(slot, changingConfigurations)) {
            viewModelScope.launch {
                delay(RETURN_AFTER_ROTATION_MS)
                slots.expire(slot)
            }
        }
    }

    override fun onCleared() {
        slots.clearAll()
    }

    private companion object {
        /** How long a rotated surface has to come back before its typed
         *  password is dropped (a dialog whose open flag did not survive). */
        const val RETURN_AFTER_ROTATION_MS: Long = 2_000L
    }
}

/** A [ProofController] for [surface], loaded once, kept fresh while the
 *  surface is composed, kept across a rotation by the screen's
 *  [ProofHolder], and cleared when the surface leaves for any other reason. */
@Composable
public fun rememberProofController(repo: AuthRepository, surface: ProofSurface, key: Any? = null): ProofController {
    val holder: ProofHolder = viewModel()
    val slot = "${surface.name}|${key ?: ""}"
    val controller = remember(holder, repo, slot) { holder.attach(slot, repo, surface) }
    val activity = LocalContext.current.findActivity()
    LaunchedEffect(controller) { controller.load() }
    LaunchedEffect(controller) {
        while (true) {
            controller.refresh()
            delay(PROOF_REFRESH_MS)
        }
    }
    DisposableEffect(holder, slot) {
        onDispose { holder.detach(slot, changingConfigurations = activity?.isChangingConfigurations == true) }
    }
    return controller
}

private tailrec fun Context.findActivity(): Activity? = when (this) {
    is Activity -> this
    is ContextWrapper -> baseContext.findActivity()
    else -> null
}

/**
 * The proof block (UX 17.1), drawn inside the surface that already exists;
 * nothing when a proof is live. The sign-in screen's own password field with
 * its show / hide control, the surface's helper as the field's description,
 * one line under it, and `Use a passkey instead` only when the account has a
 * passkey this device can use.
 *
 * [onSubmit] is the surface's main action (the keyboard's Done runs it);
 * [onEngaged] tells the surface the person started on the block.
 */
@Composable
public fun ProofBlock(
    controller: ProofController,
    modifier: Modifier = Modifier,
    onSubmit: (() -> Unit)? = null,
    onPasskey: (() -> Unit)? = null,
    onEngaged: () -> Unit = {},
) {
    val view = controller.view()
    if (!view.visible) return
    val copy = proofCopy(controller.surface)
    val state = controller.state
    // The countdown ticks once a second; announced once (the line below is a
    // polite live region whose text a screen reader reads when it appears).
    val waitLeft = rememberRemainingMs(state.waitUntilMs)
    Column(modifier = modifier.fillMaxWidth()) {
        if (view.showPasswordField) {
            if (view.showUsernameField) {
                UsernameField(
                    value = state.username,
                    onValueChange = {
                        onEngaged()
                        controller.onUsernameChange(it)
                    },
                    enabled = view.fieldsEnabled && waitLeft <= 0L,
                )
            }
            PasswordField(
                value = state.password,
                onValueChange = {
                    onEngaged()
                    controller.onPasswordChange(it)
                },
                label = t("auth.password.label"),
                enabled = view.fieldsEnabled && waitLeft <= 0L,
                imeAction = ImeAction.Done,
                onDone = onSubmit,
                supportingText = t(copy.helper),
            )
        }
        if (view.showPasskeyNotice) {
            Text(t("proof.passkey.notice"), style = MaterialTheme.typography.bodyMedium)
        }
        if (state.phase == ProofPhase.PasskeySheet) {
            Text(
                t("recovery.passkey.waiting"),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(top = 8.dp),
            )
        }
        if (waitLeft > 0L) {
            ProofErrorLine(t("recovery.error.rate_limited", mapOf("time" to formatCountdown(waitLeft))), announce = false)
        }
        when (val line = state.line) {
            is ProofLine.Error -> ProofErrorLine(line.text.text(), announce = true)
            is ProofLine.Plain -> Text(
                line.text.text(),
                style = MaterialTheme.typography.bodyMedium,
                modifier = Modifier
                    .padding(top = 8.dp)
                    .semantics { liveRegion = LiveRegionMode.Polite },
            )
            null -> Unit
        }
        if (view.showPasskeyButton && onPasskey != null) {
            Spacer(Modifier.height(4.dp))
            TextButton(
                onClick = {
                    onEngaged()
                    onPasskey()
                },
                enabled = view.passkeyButtonEnabled,
            ) { Text(t("proof.passkey.use")) }
        }
        if (view.showUsePasswordInstead) {
            Spacer(Modifier.height(4.dp))
            TextButton(
                onClick = { controller.setMode(ProofMode.Password) },
                enabled = view.passkeyButtonEnabled,
            ) { Text(t("proof.password.use")) }
        }
    }
}

/** One error line in the danger colour, announced once when it appears
 *  (`common.error.a11y` wraps it for a screen reader). */
@Composable
private fun ProofErrorLine(message: String, announce: Boolean) {
    val spoken = t("common.error.a11y", mapOf("message" to message))
    Text(
        text = message,
        color = MaterialTheme.colorScheme.error,
        style = MaterialTheme.typography.bodyMedium,
        modifier = Modifier
            .padding(top = 8.dp)
            .semantics {
                contentDescription = spoken
                if (announce) liveRegion = LiveRegionMode.Polite
            },
    )
}

/** How often an open surface re-reads whether its proof is still live. */
private const val PROOF_REFRESH_MS: Long = 5_000L
