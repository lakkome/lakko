// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import androidx.lifecycle.ViewModel
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import me.lakko.app.auth.base64UrlDecodeNoPad
import me.lakko.app.chat.ChatRepository

/**
 * ADR-0028 (G-3) — drives the `/g/<slug>` group join-ticket landing. Decodes +
 * verifies the capability, posts an external-join proposal (ADR-0027), and
 * polls until an admin approves and the rendezvous Welcome lands.
 */
public class JoinGroupLandingViewModel(
    private val repository: ChatRepository,
    private val slug: String,
) : ViewModel() {

    /** Local FSM: Requesting -> Waiting (poll loop) -> Joined / Failed. */
    public sealed interface State {
        public data object Requesting : State
        public data object Waiting : State
        public data class Joined(val groupIdHex: String) : State
        public data class Failed(val message: String) : State
    }

    private val mutableState: MutableStateFlow<State> = MutableStateFlow(State.Requesting)
    public val state: StateFlow<State> = mutableState.asStateFlow()
    private var started = false

    /** Drive the request + poll loop exactly once. */
    public fun start() {
        if (started) return
        started = true
        viewModelScope.launch {
            val wire = base64UrlDecodeNoPad(slug)
            if (wire == null) {
                mutableState.value = State.Failed("Malformed invite link.")
                return@launch
            }
            val nowMs = System.currentTimeMillis().toULong()
            val groupId: ByteArray
            val joinerKey: ByteArray
            try {
                val result = repository.requestToJoinFromLink(wire, nowMs)
                groupId = result.first
                joinerKey = result.second
            } catch (e: Throwable) {
                mutableState.value = State.Failed(e.message ?: e.toString())
                return@launch
            }
            mutableState.value = State.Waiting
            while (isActive) {
                try {
                    val hex = repository.pollForJoinWelcome(groupId, joinerKey)
                    if (hex != null) {
                        mutableState.value = State.Joined(hex)
                        return@launch
                    }
                } catch (e: Throwable) {
                    mutableState.value = State.Failed(e.message ?: e.toString())
                    return@launch
                }
                delay(3_000)
            }
        }
    }
}

/** The landing view for a tapped `/g/<slug>` group join-ticket link. */
@Composable
public fun JoinGroupLandingScreen(
    viewModel: JoinGroupLandingViewModel,
    onDone: () -> Unit,
) {
    val state by viewModel.state.collectAsStateWithLifecycle()
    LaunchedEffect(Unit) { viewModel.start() }
    Column(
        modifier = Modifier
            .fillMaxSize()
            .padding(24.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.Center,
    ) {
        Text("Join group", style = MaterialTheme.typography.titleLarge)
        Spacer(Modifier.height(16.dp))
        when (val s = state) {
            is JoinGroupLandingViewModel.State.Requesting -> {
                CircularProgressIndicator()
                Spacer(Modifier.height(8.dp))
                Text("Sending request…")
            }
            is JoinGroupLandingViewModel.State.Waiting -> {
                CircularProgressIndicator()
                Spacer(Modifier.height(8.dp))
                Text("Waiting for an admin to approve…")
            }
            is JoinGroupLandingViewModel.State.Joined -> {
                Text("Joined! You can find the group in your chats.")
                Spacer(Modifier.height(12.dp))
                Button(onClick = onDone) { Text("Done") }
            }
            is JoinGroupLandingViewModel.State.Failed -> {
                Text(s.message, color = MaterialTheme.colorScheme.error)
                Spacer(Modifier.height(12.dp))
                Button(onClick = onDone) { Text("Close") }
            }
        }
    }
}
