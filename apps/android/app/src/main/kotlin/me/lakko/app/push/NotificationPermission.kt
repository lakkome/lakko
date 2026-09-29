// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.push

/**
 * Whether to raise the system notification prompt, and what Settings says when
 * the answer was no — `docs/ux/calls.md` section 2.1.
 *
 * ## The defect this exists to close
 *
 * Section 2.1 requires the phone to ring when the app is backgrounded, closed
 * or locked. On Android 13 and later that needs `POST_NOTIFICATIONS`, which is
 * a runtime permission: declaring it in the manifest grants nothing. It was
 * declared and never requested, so on any modern phone it stayed denied,
 * `IncomingCallRinger` caught the `SecurityException` it got for posting the
 * incoming-call notification, and the phone silently never rang — exactly the
 * case the feature exists for.
 *
 * It was a gap in a mirror rather than a platform difference. The iOS
 * `PushSubscriber.subscribe()` opens with `requestAuthorization`; the Android
 * [PushSubscriber] is otherwise the same shape — token, pseudonym, register,
 * result — and simply had no first step. It could not have one: Android's
 * subscribe runs on an application-scoped coroutine holding only an
 * application context, and a runtime permission dialog needs an Activity. So
 * the ask belongs to the UI layer, and the rule it follows belongs here.
 *
 * Pure of Android and of Compose, so the host JVM suite can run it.
 */

/** The API level at which posting a notification became a runtime permission. */
public const val NOTIFICATION_PERMISSION_SDK: Int = 33

/**
 * What to do about the notification permission right now.
 *
 * A named value rather than a boolean because the three "do not ask" outcomes
 * are not the same thing to the user: [AlreadyGranted] needs no words at all,
 * [AlreadyAsked] is the one that owes them a way to change their mind, and
 * [NotNeeded] is a phone where the question does not exist.
 */
public enum class NotificationPermissionAsk {
    /** Raise the system prompt. */
    Ask,

    /** Below [NOTIFICATION_PERMISSION_SDK] — the permission is implicit. */
    NotNeeded,

    /** Already held; there is nothing to ask for. */
    AlreadyGranted,

    /** This install has asked once already. Do not ask again. */
    AlreadyAsked,
}

/**
 * Should the system notification prompt be raised now?
 *
 * The only outcome that asks is a phone that needs the permission, does not
 * hold it, and has not been asked by this install before.
 *
 * **Asking a second time is the rule that matters.** Android permanently
 * auto-denies after the prompt has been dismissed twice: the launcher returns
 * "not granted" without anything appearing on screen. A second ask would
 * therefore show the user nothing while looking, from the code's side, like a
 * fair question — and burn the one remaining chance the platform allows. Once
 * this install has asked, system settings is where a refusal gets changed, and
 * [NOTIFICATIONS_OFF_NOTICE] is what says so.
 *
 * @param sdkInt this device's `Build.VERSION.SDK_INT`.
 * @param granted the permission is already held.
 * @param askedBefore this install has raised the prompt before (durable, so it
 *        survives the process death that a prompt can itself cause).
 */
public fun notificationPermissionAsk(
    sdkInt: Int,
    granted: Boolean,
    askedBefore: Boolean,
): NotificationPermissionAsk = when {
    sdkInt < NOTIFICATION_PERMISSION_SDK -> NotificationPermissionAsk.NotNeeded
    granted -> NotificationPermissionAsk.AlreadyGranted
    askedBefore -> NotificationPermissionAsk.AlreadyAsked
    else -> NotificationPermissionAsk.Ask
}

/**
 * The Settings row that says notifications are off and opens the one place
 * that can turn them on.
 *
 * Both lines are here rather than in the screen so a test can hold them, and
 * so the promise stays the honest one: Lakko rings while it is open whatever
 * the user chose, and notifications are what extend that to when it is not.
 * Nothing here claims more than the app does.
 */
public const val NOTIFICATIONS_OFF_TITLE: String = "Turn on notifications"

/** Catalog key of [NOTIFICATIONS_OFF_TITLE] (ADR-0058); the screen renders
 *  the key, the constant pins the English the catalog carries. */
public const val NOTIFICATIONS_OFF_TITLE_KEY: String = "settings.notifications.off.title"

/** Catalog key of [NOTIFICATIONS_OFF_NOTICE]. */
public const val NOTIFICATIONS_OFF_NOTICE_KEY: String = "settings.notifications.off.body"

/** The supporting line under [NOTIFICATIONS_OFF_TITLE]. */
public const val NOTIFICATIONS_OFF_NOTICE: String =
    "Notifications are off, so Lakko can only ring while it is open. " +
        "Turn them on so calls and messages reach you when it is not."
