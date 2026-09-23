// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import java.security.SecureRandom

/**
 * The invite link of a call that is already happening — `docs/ux/call-links.md`
 * sections 3 and 4.
 *
 * ## What changed
 *
 * The link used to be an object a person built before any call existed: a form
 * asked four questions — audio or video, when it expires, how many uses, what
 * to label it — and handed back a string to carry somewhere. Section 1 deleted
 * that shape. A link is now a property of a live call, offered when wanted and
 * absent from the mind when not, so there is nothing left to ask and the four
 * answers are filled in here, once, for every client.
 *
 * ## The values (section 4)
 *
 * `call_id` is the live call's, which is what makes the link lead HERE rather
 * than to a new call; the rest are [CALL_INVITE_LINK_MEDIA_VIDEO],
 * [callInviteLinkExpiryMs], [callInviteLinkMaxUses] and
 * [CALL_INVITE_LINK_LABEL].
 *
 * ## When it is minted
 *
 * At the FIRST `Invite` of a call, never at `New call`. Minting costs a network
 * round trip and a proof-of-work solve, so paying it at the start would make
 * every call slower for the sake of the links most calls never need. That is
 * also why the busy state here says what it is doing instead of only spinning:
 * the wait is seconds, not milliseconds.
 *
 * Pure and Android-free, so every branch is unit-tested on the host JVM.
 */

/** Length of the `call_id` a capability carries, in bytes. */
public const val CALL_ID_LEN: Int = 16

/**
 * How long a call's invite link stays valid, and how many people it admits.
 *
 * **Both come from `lakko_contacts::call_invite`, over the FFI**, and neither
 * is a number held here. They are fields of a SIGNED capability, so the wire
 * is what they have to agree with, and a client that quietly carried its own
 * would sign something the rest of the product does not mean.
 *
 * That is not hypothetical: this file shipped with twenty-four hours while
 * the web and iOS clients shipped twelve, from three people reading the same
 * sentence in section 4 and each picking a defensible number. The shared
 * value ends the argument; the bounds it has to satisfy are `const`
 * assertions in Rust, so a value outside them fails the build rather than a
 * test.
 *
 * The expiry is never `CALL_NEVER_EXPIRES` (the zero sentinel) and saturates
 * rather than wrapping — both guaranteed by the shared implementation, not
 * re-checked here.
 */
public fun callInviteLinkExpiryMs(nowMs: Long): Long =
    uniffi.lakko_ffi_uniffi.callInviteExpiryMs(nowMs.toULong()).toLong()

/** `max_uses`, from the shared value: unbounded. The host admits every
 *  arrival one at a time, so a count here would cap a meeting without adding
 *  a check anybody makes. */
public fun callInviteLinkMaxUses(): UInt = uniffi.lakko_ffi_uniffi.callInviteMaxUses()

/** `media`: there is one kind of call, and it carries video (section 1). */
public const val CALL_INVITE_LINK_MEDIA_VIDEO: Boolean = true

/** `label`: empty. Nothing to say that the call screen does not. */
public const val CALL_INVITE_LINK_LABEL: String = ""

/**
 * Whether the `Invite` control is drawn for a call in this state.
 *
 * The call's OWNER, and only once the call is live. ADR-0032 makes the owner
 * the sole founder and a capability names its signer as the owner, so a link
 * minted by anyone else would send its arrival to elect a committer that device
 * is not — a shape nothing in the protocol is tested against. The person who
 * pressed `New call` is the host, and is the one section 3 gives the control to.
 */
public fun showsCallInvite(owner: Boolean, phase: LinkCallPhase): Boolean =
    owner && phase == LinkCallPhase.LIVE

/** A fresh `call_id` for a call nobody has started yet. */
public fun newCallId(random: SecureRandom = SecureRandom()): ByteArray =
    ByteArray(CALL_ID_LEN).also { random.nextBytes(it) }

/**
 * Where the `Invite` control is, for the call on screen.
 *
 * One state per call: [LinkCallHost] resets it when a call starts, so a link
 * minted for one call is never offered as another call's.
 */
public sealed interface CallInviteLinkState {
    /** Nothing asked for yet. */
    public data object Idle : CallInviteLinkState

    /** Signing and uploading the capability; includes the proof-of-work solve. */
    public data object Preparing : CallInviteLinkState

    /** Minted. A second `Invite` re-offers this rather than minting again. */
    public data class Ready(val url: String) : CallInviteLinkState

    /** The mint failed. The call is unaffected; the control stays live. */
    public data class Failed(val message: String) : CallInviteLinkState
}

/** The `Invite` control, ready to draw. */
public data class CallInviteButton(
    /** The word on the control. Names the action, never the state. */
    val label: String,
    /** A spinner instead of the icon. */
    val busy: Boolean,
    val enabled: Boolean,
    /** What TalkBack reads. */
    val spokenLabel: String,
    /**
     * One quiet line under the control, or `null`. Carries the wait while the
     * link is being prepared and the reason when it failed.
     */
    val note: String?,
)

/** How the control reads in each state. */
public fun callInviteButton(state: CallInviteLinkState): CallInviteButton = when (state) {
    CallInviteLinkState.Idle -> CallInviteButton(
        label = "Invite",
        busy = false,
        enabled = true,
        spokenLabel = "Invite someone to this call",
        note = null,
    )
    CallInviteLinkState.Preparing -> CallInviteButton(
        // Section 5: the wait "says what it is doing rather than only spinning".
        label = "Preparing the link…",
        busy = true,
        enabled = false,
        spokenLabel = "Preparing the invite link",
        note = "This can take a few seconds.",
    )
    is CallInviteLinkState.Ready -> CallInviteButton(
        label = "Invite",
        busy = false,
        enabled = true,
        spokenLabel = "Share the invite link",
        note = null,
    )
    is CallInviteLinkState.Failed -> CallInviteButton(
        label = "Invite",
        busy = false,
        enabled = true,
        spokenLabel = "Try preparing the invite link again",
        note = state.message,
    )
}

/** Why a mint did not produce a link. */
public enum class CallInviteFailure {
    /** The session is gone: nothing can be signed for this account. */
    NO_SESSION,

    /** The device could not reach the server. */
    NETWORK,

    /** Anything else. */
    OTHER,
}

/**
 * Which [CallInviteFailure] a thrown cause is.
 *
 * [CallInviteFailure.NO_SESSION] is not here: a missing session is found by
 * reading it, not by a throw, so the caller names it directly rather than
 * wrapping it in an exception for this to unwrap again.
 *
 * Unknown causes are [CallInviteFailure.OTHER] rather than being guessed at, so
 * a new failure kind reads as a plain "could not be prepared" instead of as a
 * confident wrong diagnosis.
 */
public fun classifyCallInviteFailure(cause: Throwable?): CallInviteFailure = when (cause) {
    null -> CallInviteFailure.OTHER
    is me.lakko.app.discovery.DiscoveryNetworkError -> CallInviteFailure.NETWORK
    is java.io.IOException -> CallInviteFailure.NETWORK
    else -> CallInviteFailure.OTHER
}

/**
 * What the user is told when a mint fails — section 5: one sentence on what
 * happened, one on what to do, and never a word about the call, which is
 * unaffected and does not end.
 */
public fun callInviteFailureMessage(failure: CallInviteFailure): String = when (failure) {
    CallInviteFailure.NO_SESSION ->
        "You are signed out. Sign in again to invite someone."
    CallInviteFailure.NETWORK ->
        "The invite link could not be prepared. Check your connection and try again."
    CallInviteFailure.OTHER ->
        "The invite link could not be prepared. Try again."
}

/** [callInviteFailureMessage] for a thrown cause. */
public fun callInviteFailureMessage(cause: Throwable?): String =
    callInviteFailureMessage(classifyCallInviteFailure(cause))

/**
 * What the Calls destination says when `New call` did not land in a call, or
 * `null` when it did.
 *
 * [LinkCallStartOutcome.Busy] already carries a finished sentence ([callBusyMessage]).
 * [LinkCallStartOutcome.Failed] carries the host's own diagnostic, which names
 * configuration and endpoints; that belongs in the log, so the user gets one
 * plain sentence instead.
 */
public fun newCallStartMessage(outcome: LinkCallStartOutcome): String? = when (outcome) {
    LinkCallStartOutcome.Started, LinkCallStartOutcome.AlreadyInThisCall -> null
    is LinkCallStartOutcome.Busy -> outcome.message
    is LinkCallStartOutcome.Failed -> "The call could not be started. Try again."
}

/** What the Calls destination says when there is no session to start a call with. */
public const val NEW_CALL_SIGNED_OUT_MESSAGE: String =
    "You are signed out. Sign in again to start a call."
