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
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
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
import me.lakko.app.chat.ChatRepository
import me.lakko.app.contacts.ContactsRepository
import me.lakko.app.ui.auth.LoginScreen
import me.lakko.app.ui.auth.LoginViewModel
import me.lakko.app.ui.auth.RecoverScreen
import me.lakko.app.ui.auth.RecoverViewModel
import me.lakko.app.ui.auth.SignupScreen
import me.lakko.app.ui.auth.SignupViewModel
import me.lakko.app.ui.chat.ChatListScreen
import me.lakko.app.ui.chat.ChatScreen
import me.lakko.app.ui.chat.ChatViewModel
import me.lakko.app.ui.contacts.ContactsScreen
import me.lakko.app.ui.contacts.ContactsViewModel
import me.lakko.app.ui.home.HomeScreen

/**
 * Single-activity Compose host.
 *
 * Phase 1.15f4c — drives the **signup-or-login-or-home-or-
 * contacts-or-chats-or-chat** flow:
 *
 *   * `Anon` → [LoginScreen] / [SignupScreen] / [RecoverScreen].
 *   * `Session` → [HomeScreen] / [ContactsScreen] /
 *     [ChatListScreen] / [ChatScreen] (per groupIdHex).
 *
 * Three repositories live at the Activity scope:
 *   * [AuthRepository] — auth state + identity secrets.
 *   * [ContactsRepository] — pending / sent / accepted contacts.
 *   * [ChatRepository] — MLS groups + per-thread message timeline.
 *
 * Lifecycle wire-up (1.15f4c):
 *   * On every transition into a `Session` state we observe
 *     [AuthRepository.identitySecrets]; the first non-null
 *     value drives [ChatRepository.bootstrap]. Subsequent
 *     re-emissions of the same secrets are idempotent.
 *   * Logout → `contacts.clear()` → `chat.clear()` →
 *     `auth.logout()`. Order matters: drop ephemeral state
 *     BEFORE flipping the auth state, so the AnonRoot doesn't
 *     briefly observe a Session-side flow.
 *   * Forget device → `contacts.clear()` → `chat.forgetDevice()`
 *     → `auth.forgetDevice()` — same order, but
 *     `chat.forgetDevice()` additionally wipes the on-disk
 *     snapshot envelope under the current accountId before
 *     [AuthRepository.forgetDevice] clears the
 *     [me.lakko.app.auth.SessionStore] marker. (If we cleared
 *     the SessionStore first, ChatRepository would lose the
 *     accountId reference and skip the deleteSnapshot.)
 */
class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)

        val authRepository = AuthRepository(applicationContext)
        // ChatRepository before ContactsRepository — the
        // contacts side optionally bridges to chat via
        // joinGroupFromWelcome, and the order of construction
        // makes the dependency obvious in the call site.
        val chatRepository = ChatRepository(applicationContext)
        val contactsRepository = ContactsRepository(
            authRepo = authRepository,
            chatRepo = chatRepository,
        )

        setContent {
            MaterialTheme {
                Surface(
                    modifier = Modifier.fillMaxSize(),
                    color = MaterialTheme.colorScheme.background,
                ) {
                    AppRoot(
                        authRepository = authRepository,
                        contactsRepository = contactsRepository,
                        chatRepository = chatRepository,
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
    chatRepository: ChatRepository,
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
            chatRepository = chatRepository,
            account = current.account,
        )
    }
}

/**
 * Session-state subgraph. Adds ChatRepository bootstrap +
 * a four-way [SessionDestination] router (Home / Contacts /
 * Chats / Chat(groupIdHex)). Compose Navigation lands later
 * (deferred 1.15d7); the local destination state + a
 * `String?`-shaped Chat argument is plenty for four screens.
 */
@Composable
private fun SessionRoot(
    authRepository: AuthRepository,
    contactsRepository: ContactsRepository,
    chatRepository: ChatRepository,
    account: SessionAccount,
) {
    // Drive ChatRepository.bootstrap whenever a fresh
    // identity-secrets value lands. The first non-null emission
    // happens immediately after signup / login / recover; the
    // bootstrap call is idempotent for the same account id, so
    // repeated emissions (state restoration across config
    // changes) cost nothing.
    val identitySecrets by authRepository.identitySecrets.collectAsStateWithLifecycle()
    LaunchedEffect(identitySecrets) {
        val secrets = identitySecrets ?: return@LaunchedEffect
        chatRepository.bootstrap(secrets)
    }

    // Auto-clear chat in-memory state when we leave the
    // Session entirely (e.g. via process death after logout).
    // The explicit logout / forgetDevice handlers below cover
    // the user-initiated paths; this DisposableEffect catches
    // the natural composition exit.
    DisposableEffect(Unit) {
        onDispose { /* keep state — the next SessionRoot re-mount
                       will bootstrap again from identitySecrets */ }
    }

    var destination: SessionDestination by remember {
        mutableStateOf(SessionDestination.Home)
    }

    when (val current = destination) {
        SessionDestination.Home -> HomeScreen(
            account = account,
            onOpenContacts = { destination = SessionDestination.Contacts },
            onOpenChats = { destination = SessionDestination.Chats },
            onLogout = {
                contactsRepository.clear()
                chatRepository.clear()
                authRepository.logout()
            },
            onForgetDevice = {
                contactsRepository.clear()
                // Note: chat.forgetDevice() BEFORE
                // auth.forgetDevice() — see the file-level
                // docstring for the ordering rationale.
                chatRepository.forgetDevice()
                authRepository.forgetDevice()
            },
        )

        SessionDestination.Contacts -> {
            val contactsVm: ContactsViewModel = viewModel(
                factory = singletonViewModelFactory {
                    ContactsViewModel(
                        repo = contactsRepository,
                        chatRepo = chatRepository,
                    )
                },
            )
            ContactsScreen(
                viewModel = contactsVm,
                onNavigateBack = { destination = SessionDestination.Home },
                onOpenChat = { groupIdHex ->
                    destination = SessionDestination.Chat(groupIdHex)
                },
            )
        }

        SessionDestination.Chats -> ChatListScreen(
            repository = chatRepository,
            onNavigateBack = { destination = SessionDestination.Home },
            onOpenThread = { groupIdHex ->
                destination = SessionDestination.Chat(groupIdHex)
            },
        )

        is SessionDestination.Chat -> {
            val groupIdHex = current.groupIdHex
            // Key the ChatViewModel by groupIdHex so navigating
            // to a different thread rebuilds the VM with the
            // right per-thread state binding.
            val chatVm: ChatViewModel = viewModel(
                key = "chat-$groupIdHex",
                factory = singletonViewModelFactory {
                    ChatViewModel(chatRepository, groupIdHex)
                },
            )
            ChatScreen(
                viewModel = chatVm,
                // Back from a 1×1 chat goes to the chat list,
                // not Home — standard chat-app navigation
                // shape.
                onNavigateBack = { destination = SessionDestination.Chats },
            )
        }
    }
}

/**
 * Four Session-state destinations. Compose Navigation lands in
 * a later sprint — this sealed hierarchy holds the place. The
 * [Chat] variant carries a `groupIdHex` argument so the
 * ChatViewModel knows which thread to project.
 */
private sealed interface SessionDestination {
    data object Home : SessionDestination
    data object Contacts : SessionDestination
    data object Chats : SessionDestination
    data class Chat(val groupIdHex: String) : SessionDestination
}

/**
 * Anon-state subgraph — picks Login, Signup, or Recover. The
 * default destination is keyed on [cachedUsername]. All three
 * screens carry footer links to switch between them.
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

private enum class AnonDestination {
    Login,
    Signup,
    Recover,
}

/**
 * Tiny inline [ViewModelProvider.Factory] that constructs a single
 * VM via the supplied [block].
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
 * Loading splash for the brief window between Activity start
 * and [AuthRepository] rehydration. Practically invisible
 * (rehydrate is synchronous), but rendered for symmetry.
 */
@Composable
private fun LoadingScreen() {
    Text(
        text = "Lakko",
        style = MaterialTheme.typography.displaySmall,
        modifier = Modifier.padding(24.dp),
    )
}
