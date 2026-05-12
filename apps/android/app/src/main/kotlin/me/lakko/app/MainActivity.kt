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
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import androidx.lifecycle.ViewModel
import androidx.lifecycle.ViewModelProvider
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import me.lakko.app.auth.AuthRepository
import me.lakko.app.auth.AuthState
import me.lakko.app.ui.auth.SignupScreen
import me.lakko.app.ui.auth.SignupViewModel
import me.lakko.app.ui.home.HomeScreen

/**
 * Single-activity Compose host.
 *
 * Phase 1.15d4 — drives the signup-or-home flow:
 *
 *   * [AuthRepository.state] is a [kotlinx.coroutines.flow.StateFlow]
 *     that resolves to [AuthState.Loading], [AuthState.Anon], or
 *     [AuthState.Session] after rehydration.
 *   * `Anon` → [SignupScreen]. (Login lands in 1.15d5; Recover in
 *     1.15d6; until then the user's only option is to sign up.)
 *   * `Session` → [HomeScreen] placeholder. Contacts (1.15e) and
 *     chat (1.15f) take it over once they ship.
 *
 * The repository lives at the Activity scope — built once in
 * [onCreate], remembered for the lifetime of the process. Compose
 * recompositions don't recreate it. A future migration to a DI
 * container (Hilt / Koin) is plausible but premature at one
 * repository + a couple of ViewModels.
 */
class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)

        // `applicationContext` (not `this`) so the repository
        // doesn't hold an Activity reference past rotation.
        val repository = AuthRepository(applicationContext)

        setContent {
            MaterialTheme {
                Surface(
                    modifier = Modifier.fillMaxSize(),
                    color = MaterialTheme.colorScheme.background,
                ) {
                    AppRoot(repository = repository)
                }
            }
        }
    }
}

@Composable
private fun AppRoot(repository: AuthRepository) {
    val state by repository.state.collectAsStateWithLifecycle()

    when (val current = state) {
        AuthState.Loading -> LoadingScreen()
        is AuthState.Anon -> {
            val signupVm: SignupViewModel = viewModel(
                factory = singletonViewModelFactory { SignupViewModel(repository) },
            )
            // `cachedUsername` will pre-fill the Login form
            // (1.15d5). On Signup we deliberately keep the field
            // empty — "create a new account" implies a fresh
            // handle.
            SignupScreen(
                viewModel = signupVm,
                // The auth state flips to Session inside the
                // repository the moment signup commits; the
                // top-level `collectAsStateWithLifecycle` above
                // re-renders us into [HomeScreen] automatically.
                // The acknowledge callback intentionally does
                // nothing here — it exists to be wired up to a
                // future navigation animation in 1.15d7.
                onAuthenticated = {},
            )
        }
        is AuthState.Session -> HomeScreen(
            account = current.account,
            onForgetDevice = repository::forgetDevice,
        )
    }
}

/**
 * Tiny inline [ViewModelProvider.Factory] that constructs a single
 * VM via the supplied [block]. The `androidx.lifecycle:viewmodel-ktx`
 * `viewModelFactory { initializer { ... } }` builder is the
 * canonical idiom but lives in a separate artefact; rolling our
 * own keeps the dep set tight for 1.15d4.
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
