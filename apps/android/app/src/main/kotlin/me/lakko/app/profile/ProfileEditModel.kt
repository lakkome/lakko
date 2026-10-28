// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.profile

import java.io.File

/*
 * docs/ux/profile-screen.md §3-§6 — the Settings > Profile state model, the
 * same on every client: `saved` (photo, name, username) and `pending` (each
 * of the three unchanged or a new value). Save is enabled by
 * `pending != saved` with every open field valid; leaving with anything
 * pending asks first. Pure: the host JVM runs every rule here.
 */

/**
 * One row edited in place (§3 the name, §4 the username).
 *
 * [value] is what the row shows (and, closed, what is pending); [before] is
 * the value when this edit opened, which Escape returns to.
 */
public data class InlineField(
    val saved: String,
    val value: String = saved,
    val open: Boolean = false,
    val before: String = value,
) {
    /** The pencil (or the row) was tapped: the field opens on the current text. */
    public fun opened(): InlineField = copy(open = true, before = value)

    /** The person typed [text]. */
    public fun typed(text: String): InlineField = copy(value = text)

    /** Focus left, Return or Enter: closes when [valid]; an invalid field
     *  stays open with its reason (§3). */
    public fun closed(valid: Boolean): InlineField = if (valid) copy(open = false) else this

    /** Escape: the field closes and the row returns to what it was before
     *  this edit. */
    public fun reverted(): InlineField = copy(value = before, open = false)

    /** The row's saved value moved to [newSaved] (a save, or another of the
     *  account's devices). A row the person has not changed follows it. */
    public fun withSaved(newSaved: String): InlineField =
        if (!open && value == saved) InlineField(newSaved) else copy(saved = newSaved)
}

/** The photo half of the pending state. */
public sealed interface PhotoEdit {
    /** Nothing changed. */
    public data object Unchanged : PhotoEdit

    /** `Remove photo`: the initials show, pending. */
    public data object Removed : PhotoEdit

    /**
     * `Use` on the crop screen: a new still (a square JPEG within the ADR
     * bounds) and, for a profile video, the encoded video file beside it.
     * [uploaded] is set once step 1 of Save succeeded, so a retry after a
     * later step failed does not upload again.
     */
    public data class New(
        val still: ByteArray,
        val stillSide: Int,
        val video: File? = null,
        val videoSide: Int? = null,
        val videoDurationMs: Int? = null,
        val uploaded: AvatarRefs? = null,
    ) : PhotoEdit {
        override fun equals(other: Any?): Boolean =
            other is New && other.still.contentEquals(still) && other.stillSide == stillSide &&
                other.video == video && other.videoSide == videoSide &&
                other.videoDurationMs == videoDurationMs && other.uploaded == uploaded

        override fun hashCode(): Int = still.contentHashCode() * 31 + (video?.hashCode() ?: 0)
    }
}

/** The whole screen's edit state. */
public data class ProfileEdit(
    val name: InlineField,
    val username: InlineField,
    val photo: PhotoEdit = PhotoEdit.Unchanged,
    /** The photo currently published, or `null`. */
    val savedAvatar: AvatarRefs? = null,
    /** A save is running: the edit buttons are disabled. */
    val saving: Boolean = false,
)

/** The pending name for [text] under the core's rule (`null` = no name), or
 *  `null`-with-invalid when the rule refuses it. */
public fun pendingName(text: String, normalize: (String) -> DisplayNameCheck = { checkDisplayName(it) }): DisplayNameCheck =
    normalize(text)

/** The canonical pending username, or `null` when the rule refuses it. */
public fun pendingUsername(text: String, check: (String) -> UsernameCheck = { checkUsername(it) }): String? =
    (check(text) as? UsernameCheck.Valid)?.normalized

/** Whether [edit] differs from what is saved. Rules are injected so the host
 *  JVM runs without the core. */
public fun ProfileEdit.hasChanges(
    nameRule: (String) -> DisplayNameCheck = { checkDisplayName(it) },
    usernameRule: (String) -> UsernameCheck = { checkUsername(it) },
): Boolean {
    val nameChanged = nameDiffers(name, nameRule)
    val usernameChanged = usernameDiffers(username, usernameRule)
    val photoChanged = when (photo) {
        PhotoEdit.Unchanged -> false
        PhotoEdit.Removed -> savedAvatar != null
        is PhotoEdit.New -> true
    }
    return nameChanged || usernameChanged || photoChanged
}

/** The name row's pending value differs from the saved one (the saved
 *  value is always the core-normalised name, `""` for none). An invalid
 *  value differs. */
internal fun nameDiffers(field: InlineField, rule: (String) -> DisplayNameCheck): Boolean =
    when (val c = rule(field.value)) {
        is DisplayNameCheck.Valid -> (c.normalized ?: "") != field.saved
        is DisplayNameCheck.Invalid -> true
    }

/** The username row's pending value differs from the saved one. */
internal fun usernameDiffers(field: InlineField, rule: (String) -> UsernameCheck): Boolean =
    field.value != field.saved && (pendingUsername(field.value, rule) ?: field.value) != field.saved

/** Every OPEN field is valid (a closed one always is: it cannot close
 *  invalid). */
public fun ProfileEdit.openFieldsValid(
    nameRule: (String) -> DisplayNameCheck = { checkDisplayName(it) },
    usernameRule: (String) -> UsernameCheck = { checkUsername(it) },
): Boolean {
    val nameOk = !name.open || nameRule(name.value) is DisplayNameCheck.Valid
    val usernameOk = !username.open || username.value == username.saved ||
        usernameRule(username.value) is UsernameCheck.Valid
    return nameOk && usernameOk
}

/** §5: Save is enabled when something differs from what is saved and every
 *  open field is valid; never while a save runs. */
public fun ProfileEdit.saveEnabled(
    nameRule: (String) -> DisplayNameCheck = { checkDisplayName(it) },
    usernameRule: (String) -> UsernameCheck = { checkUsername(it) },
): Boolean = !saving && hasChanges(nameRule, usernameRule) && openFieldsValid(nameRule, usernameRule)

/** §6: what leaving the screen does. */
public enum class LeaveDecision {
    /** Nothing pending: leave at once. */
    LEAVE,

    /** Something pending: show `Save changes?`. */
    ASK,

    /** A save is running: stay (the buttons are disabled anyway). */
    STAY,
}

public fun leaveDecision(
    edit: ProfileEdit,
    nameRule: (String) -> DisplayNameCheck = { checkDisplayName(it) },
    usernameRule: (String) -> UsernameCheck = { checkUsername(it) },
): LeaveDecision = when {
    edit.saving -> LeaveDecision.STAY
    edit.hasChanges(nameRule, usernameRule) -> LeaveDecision.ASK
    else -> LeaveDecision.LEAVE
}

/** §5 order: the steps a save runs, in order. */
public enum class SaveStep { UPLOAD_PHOTO, CHANGE_USERNAME, PUBLISH }

public fun ProfileEdit.saveSteps(
    nameRule: (String) -> DisplayNameCheck = { checkDisplayName(it) },
    usernameRule: (String) -> UsernameCheck = { checkUsername(it) },
): List<SaveStep> {
    val steps = ArrayList<SaveStep>(3)
    val photoNew = photo as? PhotoEdit.New
    if (photoNew != null && photoNew.uploaded == null) steps += SaveStep.UPLOAD_PHOTO
    val newUsername = pendingUsername(username.value, usernameRule)
    if (newUsername != null && newUsername != username.saved) steps += SaveStep.CHANGE_USERNAME
    val nameChanged = nameDiffers(name, nameRule)
    val photoChanged = photoNew != null || (photo == PhotoEdit.Removed && savedAvatar != null)
    if (nameChanged || photoChanged) steps += SaveStep.PUBLISH
    return steps
}

/** The avatar a publish carries after step 1, or the saved one when the photo
 *  did not change. */
public fun ProfileEdit.avatarToPublish(): AvatarRefs? = when (val p = photo) {
    PhotoEdit.Unchanged -> savedAvatar
    PhotoEdit.Removed -> null
    is PhotoEdit.New -> p.uploaded
}

/** The edit after a whole save succeeded: what was pending is saved. */
public fun ProfileEdit.afterSaved(name: String, username: String, avatar: AvatarRefs?): ProfileEdit =
    ProfileEdit(
        name = InlineField(name),
        username = InlineField(username),
        photo = PhotoEdit.Unchanged,
        savedAvatar = avatar,
    )
