// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import androidx.lifecycle.ViewModel
import androidx.lifecycle.ViewModelProvider
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import me.lakko.app.auth.AuthRepository
import me.lakko.app.auth.AuthState
import me.lakko.app.auth.SessionAccount
import me.lakko.app.contacts.ContactsRepository
import me.lakko.app.ui.auth.LoginScreen
import me.lakko.app.ui.auth.LoginViewModel
import me.lakko.app.ui.auth.RecoverScreen
import me.lakko.app.ui.auth.RecoverViewModel
import me.lakko.app.ui.auth.SignupScreen
import me.lakko.app.ui.auth.SignupViewModel
import me.lakko.app.ui.contacts.ContactsScreen
import me.lakko.app.ui.contacts.ContactsViewModel
import me.lakko.app.ui.home.HomeScreen

/**
 * Single-activity Compose host.
 *
 * Phase 1.15d5 — drives the **signup-or-login-or-home** flow:
 *
 *   * [AuthRepository.state] is a [kotlinx.coroutines.flow.StateFlow]
 *     that resolves to [AuthState.Loading], [AuthState.Anon], or
 *     [AuthState.Session] after rehydration.
 *   * `Anon` → either [LoginScreen] or [SignupScreen]. The default
 *     pick depends on whether there's a cached username on this
 *     device:
 *       - cached present → Login (the user is returning); the form
 *         pre-fills the username.
 *       - cached absent  → Signup (fresh install).
 *     Both screens carry "switch to the other one" footer links
 *     so the choice is never trapped.
 *   * `Session` → [HomeScreen] placeholder with both `logout`
 *     (keeps device material) and `forgetDevice` (wipes
 *     SessionStore + Keystore envelope) actions. Contacts (1.15e)
 *     and chat (1.15f) take this real estate over.
 *
 * No NavController yet — Compose Navigation lands in 1.15d7. The
 * top-level `when` over [AuthState] + the local `showLogin` toggle
 * for the Anon screens is plenty for three destinations.
 */
class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)

        // `applicationContext` (not `this`) so the repositories
        // don't hold an Activity reference past rotation.
        val authRepository = AuthRepository(applicationContext)
        // Single ContactsRepository shared by every ContactsViewModel
        // for the Activity's lifetime — the in-memory state
        // (pending requests, sent log) survives composition
        // recreations but drops on process death.
        val contactsRepository = ContactsRepository(authRepository)

        setContent {
            MaterialTheme {
                Surface(
                    modifier = Modifier.fillMaxSize(),
                    color = MaterialTheme.colorScheme.background,
                ) {
                    AppRoot(
                        authRepository = authRepository,
                        contactsRepository = contactsRepository,
                    )
                }
            }
        }
    }
}

@Composable
private fun AppRoot(
    authRepository: AuthRepository,
    contactsRepository: ContactsRepository,
) {
    val state by authRepository.state.collectAsStateWithLifecycle()

    when (val current = state) {
        AuthState.Loading -> LoadingScreen()
        is AuthState.Anon -> AnonRoot(
            repository = authRepository,
            cachedUsername = current.cachedUsername,
        )
        is AuthState.Session -> SessionRoot(
            authRepository = authRepository,
            contactsRepository = contactsRepository,
            account = current.account,
        )
    }
}

/**
 * Session-state subgraph — local toggle between Home and
 * Contacts destinations. Compose Navigation lands later
 * (deferred 1.15d7 work); the local `when` + a remembered
 * destination value is enough for the two screens 1.15e6 ships.
 *
 * Logging out / forgetting the device flips the top-level
 * AuthState back to Anon and we exit this composition entirely;
 * the local destination resets to [SessionDestination.Home]
 * automatically next time a Session lands.
 */
@Composable
private fun SessionRoot(
    authRepository: AuthRepository,
    contactsRepository: ContactsRepository,
    account: SessionAccount,
) {
    var destination by remember { mutableStateOf(SessionDestination.Home) }

    when (destination) {
        SessionDestination.Home -> HomeScreen(
            account = account,
            onOpenContacts = { destination = SessionDestination.Contacts },
            onLogout = {
                contactsRepository.clear()
                authRepository.logout()
            },
            onForgetDevice = {
                contactsRepository.clear()
                authRepository.forgetDevice()
            },
        )
        SessionDestination.Contacts -> {
            val contactsVm: ContactsViewModel = viewModel(
                factory = singletonViewModelFactory {
                    ContactsViewModel(contactsRepository)
                },
            )
            ContactsScreen(
                viewModel = contactsVm,
                onNavigateBack = { destination = SessionDestination.Home },
            )
        }
    }
}

private enum class SessionDestination { Home, Contacts }

/**
 * Anon-state subgraph — picks Login, Signup, or Recover. The
 * default destination is keyed on [cachedUsername] so a
 * `forgetDevice` (which clears the cache) snaps the user from
 * Login back to Signup without manual toggling.
 *
 * Cached username present → default `Login` (returning user).
 * Cached absent             → default `Signup` (fresh install).
 *
 * All three screens carry footer links to switch between them.
 * The Recover screen additionally renders a "no cached account"
 * stub when [cachedUsername] is `null` — the recovery flow can't
 * target an account that isn't on disk.
 */
@Composable
private fun AnonRoot(
    repository: AuthRepository,
    cachedUsername: String?,
) {
    var destination by remember(cachedUsername) {
        mutableStateOf(
            if (cachedUsername != null) AnonDestination.Login
            else AnonDestination.Signup,
        )
    }

    when (destination) {
        AnonDestination.Login -> {
            // Key the ViewModel by cachedUsername so a returning
            // user ("alice" cached, signed out, came back) gets
            // a VM with the pre-filled username, while a freshly-
            // forgotten device builds a VM with an empty field.
            val loginVm: LoginViewModel = viewModel(
                key = "login-${cachedUsername ?: ""}",
                factory = singletonViewModelFactory {
                    LoginViewModel(repository, cachedUsername)
                },
            )
            LoginScreen(
                viewModel = loginVm,
                onNavigateToSignup = { destination = AnonDestination.Signup },
                onNavigateToRecover = { destination = AnonDestination.Recover },
            )
        }
        AnonDestination.Signup -> {
            val signupVm: SignupViewModel = viewModel(
                factory = singletonViewModelFactory { SignupViewModel(repository) },
            )
            SignupScreen(
                viewModel = signupVm,
                // Auth state flips to Session inside the repo on
                // commit; the top-level `collectAsStateWithLifecycle`
                // re-renders us into HomeScreen automatically. The
                // acknowledge callback intentionally does nothing
                // here — it exists for a future navigation
                // animation in 1.15d7.
                onAuthenticated = {},
                onNavigateToLogin = { destination = AnonDestination.Login },
            )
        }
        AnonDestination.Recover -> {
            val recoverVm: RecoverViewModel = viewModel(
                key = "recover-${cachedUsername ?: ""}",
                factory = singletonViewModelFactory {
                    RecoverViewModel(repository, cachedUsername)
                },
            )
            RecoverScreen(
                viewModel = recoverVm,
                onNavigateToLogin = { destination = AnonDestination.Login },
                onNavigateToSignup = { destination = AnonDestination.Signup },
            )
        }
    }
}

/**
 * Three Anon-state destinations. Compose Navigation lands in
 * 1.15d7 — this enum holds the place until then.
 */
private enum class AnonDestination {
    Login,
    Signup,
    Recover,
}

/**
 * Tiny inline [ViewModelProvider.Factory] that constructs a single
 * VM via the supplied [block]. The `androidx.lifecycle:viewmodel-ktx`
 * `viewModelFactory { initializer { ... } }` builder is the
 * canonical idiom but lives in a separate artefact; rolling our
 * own keeps the dep set tight for 1.15d4-5.
 */
private inline fun <reified VM : ViewModel> singletonViewModelFactory(
    crossinline block: () -> VM,
): ViewModelProvider.Factory = object : ViewModelProvider.Factory {
    @Suppress("UNCHECKED_CAST")
    override fun <T : ViewModel> create(modelClass: Class<T>): T {
        require(modelClass.isAssignableFrom(VM::class.java)) {
            "Unexpected ViewModel class: $modelClass"
        }
        return block() as T
    }
}

/**
 * Loading splash for the brief window between Activity start and
 * [AuthRepository] rehydration. Practically invisible (rehydrate
 * is synchronous), but rendered for symmetry with the web side.
 */
@Composable
private fun LoadingScreen() {
    Text(
        text = "Lakko",
        style = MaterialTheme.typography.displaySmall,
        modifier = Modifier.padding(24.dp),
    )
}
