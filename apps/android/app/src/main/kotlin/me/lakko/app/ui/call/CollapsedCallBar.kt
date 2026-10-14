// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.call

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Lock
import androidx.compose.material.icons.filled.Videocam
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.draw.clip
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.disabled
import androidx.compose.ui.semantics.role
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.StateFlow
import me.lakko.app.calls.AppCallHost
import me.lakko.app.calls.CallBarModel
import me.lakko.app.calls.CallControlEmphasis
import me.lakko.app.calls.CallKind
import me.lakko.app.calls.CallSession
import me.lakko.app.calls.CallUiState
import me.lakko.app.calls.GroupCallHost
import me.lakko.app.calls.LinkCallHost
import me.lakko.app.calls.OneToOneCallHost
import me.lakko.app.calls.callBarMicControl
import me.lakko.app.calls.callBarPhaseOf
import me.lakko.app.calls.groupCallBarPhaseOf
import me.lakko.app.calls.callBarStatusText
import me.lakko.app.calls.callScreenTitle
import me.lakko.app.calls.showsCallEncryptionChip
import me.lakko.app.ui.theme.LakkoDark
import me.lakko.app.ui.theme.LakkoLight
import me.lakko.app.ui.theme.isLakkoDark
import me.lakko.app.l10n.LocalStrings
import me.lakko.app.l10n.joinList
import me.lakko.app.l10n.text
import uniffi.lakko_ffi_uniffi.CallBarPhase
import uniffi.lakko_ffi_uniffi.CallBarStatus
import uniffi.lakko_ffi_uniffi.callBarStatus
import uniffi.lakko_ffi_uniffi.callBarVisible

/**
 * `docs/ux/calls.md` section 4.5 — the live call, put away into one bar across
 * the top of the app.
 *
 * ## The defect this closes
 *
 * An answered call owned the whole app and nothing brought the app back except
 * hanging up. The system back did not even collapse it: the maintainer pressed
 * it on a live call and the call screen stayed up with the timer running. So a
 * person on a call could not look up the address they were being asked for,
 * could not read the message that had just arrived, and could not answer the one
 * question the call was about.
 *
 * ## Where the decisions live
 *
 * Not here. Whether the bar is on screen and which of four things its line says
 * are decided in `crates/lakko-ui-rules/src/call_bar.rs` and reached over
 * UniFFI; the phase translations and the wording are in
 * [me.lakko.app.calls.CollapsedCallBar]'s pure functions, which the host JVM
 * suite covers. This file draws the answer and nothing else — there is no
 * Robolectric and no compose-ui-test on this client, so everything that CAN be
 * decided outside a composable is.
 *
 * ## There is no End on the bar
 *
 * Deliberately, and section 4.5 gives the reason: ending is the one irreversible
 * control in a call, the call screen sets it apart by twice the gap and paints
 * it the only red on the screen, and a strip a thumb crosses on the way to a
 * chat is the wrong place for it. Mute is here because it is the control a
 * person needs in the second they need it, and pressing it the wrong way round
 * is undone by pressing it again.
 */

/** What the bar occupies at the top of the app, above the screen's content. */
private val CALL_BAR_HEIGHT = 56.dp

/**
 * The minimum touch target. The mute circle is drawn smaller than this and its
 * clickable box is not: a 36 dp circle in a 48 dp target reads as a strip
 * control rather than a call-screen button and is still comfortably hittable.
 */
private val MUTE_TARGET = 48.dp
private val MUTE_CIRCLE = 36.dp

/** Section 4.1.1: "Disabled is 40 percent opacity, never a hidden button." */
private const val DISABLED_ALPHA = 0.4f

/**
 * Everything the bar needs, or `null` when there is no bar to draw.
 *
 * `null` is the answer in every case section 4.5 gives: no call, a call that is
 * only ringing in, a call that is over, and a call whose own screen is up. The
 * last one is not branched on here — it is fed to the rule as its
 * `callScreenUp` argument, because "one state, one place" is the rule's
 * sentence to enforce and not this file's.
 *
 * @param nameForOneToOne the other person's resolved name for a 1x1 chat.
 * @param groupNameFor the group's own name (ADR-0051), or `null`.
 */
@Composable
internal fun rememberCollapsedCallBar(
    oneToOne: OneToOneCallHost<CallSession>,
    group: GroupCallHost,
    link: LinkCallHost,
    nameForOneToOne: (groupIdHex: String) -> String?,
    groupNameFor: (groupIdHex: String) -> String?,
): CallBarModel? {
    val occupant by AppCallHost.occupant.collectAsStateWithLifecycle()
    return when (occupant) {
        null -> null
        CallKind.ONE_TO_ONE -> oneToOneCallBar(oneToOne, nameForOneToOne)
        CallKind.GROUP -> hostedCallBar(
            ui = group.ui,
            collapsedFlow = group.collapsed,
            connectedSinceFlow = group.connectedSinceMs,
            // Section 4: a group call is titled by its group, never by a
            // participant — the same function the call screen's title comes from.
            groupName = group.slot.collectAsStateWithLifecycle().value
                ?.groupIdHex
                ?.let(groupNameFor),
        )
        CallKind.LINK -> hostedCallBar(
            ui = link.ui,
            collapsedFlow = link.collapsed,
            connectedSinceFlow = link.connectedSinceMs,
            // A link call has no group to be named by; [callScreenTitle] gives
            // it the same neutral word its own screen shows.
            groupName = null,
        )
    }
}

@Composable
private fun oneToOneCallBar(
    host: OneToOneCallHost<CallSession>,
    nameForOneToOne: (groupIdHex: String) -> String?,
): CallBarModel? {
    val call = host.active.collectAsStateWithLifecycle().value ?: return null
    val session = call.session
    val state by session.state.collectAsStateWithLifecycle()
    val collapsed by host.collapsed.collectAsStateWithLifecycle()
    val connectedSince by host.connectedSinceMs.collectAsStateWithLifecycle()
    val micOn by session.micEnabled.collectAsStateWithLifecycle()
    val cameraOn by session.cameraEnabled.collectAsStateWithLifecycle()
    val e2eeActive by session.e2eeActive.collectAsStateWithLifecycle()
    val safetyNumber by session.safetyNumber.collectAsStateWithLifecycle()
    return callBarModel(
        who = callScreenTitle(
            groupName = null,
            peerName = nameForOneToOne(call.groupIdHex),
            groupCall = false,
            strings = LocalStrings.current,
        ),
        phase = callBarPhaseOf(state, accepted = call.accepted),
        collapsed = collapsed,
        connectedSinceMs = connectedSince,
        showsLock = showsCallEncryptionChip(e2eeActive, safetyNumber),
        cameraSending = cameraOn,
        micOn = micOn,
    )
}

/** The group (SFU) call and the call-link call: both driven through a host-owned
 *  [CallUiState], so both read the same way. */
@Composable
private fun hostedCallBar(
    ui: CallUiState,
    collapsedFlow: StateFlow<Boolean>,
    connectedSinceFlow: StateFlow<Long?>,
    groupName: String?,
): CallBarModel? {
    val phase by ui.phase.collectAsStateWithLifecycle()
    val collapsed by collapsedFlow.collectAsStateWithLifecycle()
    val connectedSince by connectedSinceFlow.collectAsStateWithLifecycle()
    val participants by ui.participants.collectAsStateWithLifecycle()
    val e2ee by ui.e2ee.collectAsStateWithLifecycle()
    val local = participants.firstOrNull { it.isLocal }
    return callBarModel(
        who = callScreenTitle(groupName = groupName, peerName = null, groupCall = true, strings = LocalStrings.current),
        phase = groupCallBarPhaseOf(phase),
        collapsed = collapsed,
        connectedSinceMs = connectedSince,
        showsLock = showsCallEncryptionChip(e2ee.active, e2ee.shortFingerprint),
        cameraSending = local?.camOn == true,
        micOn = local?.micOn != false,
    )
}

/**
 * The shared tail: ask the rule whether there is a bar, and if so what its line
 * says. One copy, so the three kinds of call cannot answer differently.
 */
@Composable
@Suppress("LongParameterList")
private fun callBarModel(
    who: String,
    phase: CallBarPhase,
    collapsed: Boolean,
    connectedSinceMs: Long?,
    showsLock: Boolean,
    cameraSending: Boolean,
    micOn: Boolean,
): CallBarModel? {
    // A call that is not put away has its own screen up; the rule turns that
    // into "no bar" so that "one state, one place" is stated once.
    if (!callBarVisible(phase = phase, callScreenUp = !collapsed)) return null

    // The tick. It exists only while there is a running time to show, and it
    // reads the wall clock rather than counting its own iterations, so a
    // dropped frame or a doze does not make the bar's clock drift away from the
    // call screen's.
    var nowMs by remember { mutableStateOf(System.currentTimeMillis()) }
    LaunchedEffect(connectedSinceMs) {
        if (connectedSinceMs == null) return@LaunchedEffect
        while (true) {
            nowMs = System.currentTimeMillis()
            delay(1_000)
        }
    }
    val connectedForMs = connectedSinceMs?.let { (nowMs - it).coerceAtLeast(0L) } ?: 0L
    return CallBarModel(
        who = who,
        status = callBarStatus(phase = phase, connectedForMs = connectedForMs.toULong()),
        showsLock = showsLock,
        cameraSending = cameraSending,
        // Section 4.1's state table: on the `Calling...` row only End is active,
        // and the bar has no End.
        mic = callBarMicControl(micOn = micOn, ringing = phase == CallBarPhase.CALLING),
        micOn = micOn,
    )
}

/**
 * The bar. Contents left to right exactly as section 4.5 lists them: who is on
 * the call, the state or the running time, a lock only when the call screen
 * would show its chip, a camera glyph while this device's camera is sending,
 * and one round mute button.
 *
 * Two touch targets and not one: everything but the mute button is the tap that
 * brings the call back, so TalkBack reads the call and the return separately
 * from the control, instead of swallowing the button into one merged row.
 */
@Composable
internal fun CollapsedCallBar(
    model: CallBarModel,
    onExpand: () -> Unit,
    onToggleMute: (on: Boolean) -> Unit,
    modifier: Modifier = Modifier,
) {
    val strings = LocalStrings.current
    val status = callBarStatusText(model.status, strings)
    // What TalkBack reads for the return target (ADR-0061, the web's bar):
    // the return and who, then the state in words, then the lock and the
    // camera when they apply, joined as a list.
    val spokenState = when (val st = model.status) {
        CallBarStatus.Calling -> strings["call.status.calling_spoken"]
        CallBarStatus.Connecting -> strings["call.status.connecting_spoken"]
        CallBarStatus.Reconnecting -> strings["call.status.reconnecting_spoken"]
        is CallBarStatus.Elapsed ->
            me.lakko.app.calls.spokenCallDuration((st.ms / 1_000uL).toLong(), strings) ?: status
    }
    val spoken = joinList(
        buildList {
            add(strings.format("call.bar.return_a11y", mapOf("name" to model.who)))
            add(spokenState)
            if (model.showsLock) add(strings["call.bar.encrypted_a11y"])
            if (model.cameraSending) add(strings["call.bar.camera_on_a11y"])
        },
        strings,
    )
    Surface(
        modifier = modifier.fillMaxWidth(),
        color = MaterialTheme.colorScheme.primaryContainer,
        contentColor = MaterialTheme.colorScheme.onPrimaryContainer,
    ) {
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .height(CALL_BAR_HEIGHT),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Row(
                modifier = Modifier
                    .weight(1f)
                    .fillMaxHeight()
                    .clickable(onClick = onExpand)
                    .semantics(mergeDescendants = true) {
                        contentDescription = spoken
                        role = Role.Button
                    }
                    .padding(start = 16.dp, end = 8.dp),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                Text(
                    text = model.who,
                    style = MaterialTheme.typography.labelLarge,
                    fontWeight = FontWeight.SemiBold,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                    // The name gives way first; the state and the time are
                    // short and are what the bar is for.
                    modifier = Modifier.weight(1f, fill = false),
                )
                Text(
                    text = status,
                    style = MaterialTheme.typography.labelMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    maxLines = 1,
                )
                // Section 4.5: "a small lock, and only when the call screen
                // would show its chip. The bar never carries the word on its
                // own" — a claim about encryption belongs where the safety
                // number can be read, and that is one tap away.
                if (model.showsLock) {
                    Icon(
                        imageVector = Icons.Filled.Lock,
                        contentDescription = null,
                        tint = if (isLakkoDark()) LakkoDark.success else LakkoLight.success,
                        modifier = Modifier.size(14.dp),
                    )
                }
                // "so a collapsed video call is never a camera you forgot about"
                if (model.cameraSending) {
                    Icon(
                        imageVector = Icons.Filled.Videocam,
                        contentDescription = null,
                        tint = MaterialTheme.colorScheme.onSurfaceVariant,
                        modifier = Modifier.size(16.dp),
                    )
                }
            }
            CallBarMuteButton(model = model, onToggleMute = onToggleMute)
            Spacer(Modifier.width(8.dp))
        }
    }
}

/**
 * The one control — drawn from the call screen's own microphone control, so the
 * inversion that matters is decided once: a MUTED microphone is the solid,
 * slashed circle, because section 4.1.1 makes off the loud state.
 *
 * The palette is the app's rather than the call screen's, because the surface
 * is: the bar sits on a light-or-dark app screen, not on the call's own dark
 * stage. Only the colours differ; which glyph, which fill weight, whether it is
 * live and what TalkBack reads all come from [callBarMicControl].
 */
@Composable
private fun CallBarMuteButton(model: CallBarModel, onToggleMute: (on: Boolean) -> Unit) {
    val control = model.mic
    val solid = control.emphasis == CallControlEmphasis.SOLID
    val on = MaterialTheme.colorScheme.onPrimaryContainer
    val spoken = control.spokenLabel.text()
    Box(
        modifier = Modifier
            .size(MUTE_TARGET)
            .alpha(if (control.enabled) 1f else DISABLED_ALPHA)
            .clickable(
                enabled = control.enabled,
                // The bar's tap expands the call; this one must not, so it is
                // its own target rather than a child of that one.
                onClick = { onToggleMute(!model.micOn) },
            )
            .semantics(mergeDescendants = true) {
                contentDescription = spoken
                role = Role.Button
                if (!control.enabled) disabled()
            },
        contentAlignment = Alignment.Center,
    ) {
        Box(
            modifier = Modifier
                .size(MUTE_CIRCLE)
                .clip(CircleShape)
                .background(if (solid) on else on.copy(alpha = 0.12f)),
            contentAlignment = Alignment.Center,
        ) {
            Icon(
                imageVector = control.icon(),
                contentDescription = null,
                tint = if (solid) MaterialTheme.colorScheme.primaryContainer else on,
                modifier = Modifier
                    .size(20.dp)
                    .clearAndSetSemantics { },
            )
        }
    }
}
