// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.profile

import android.graphics.Bitmap
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import me.lakko.app.auth.RenameResult
import me.lakko.app.avatar.AvatarMedia
import me.lakko.app.l10n.UiText
import me.lakko.app.profile.AvatarBlob
import me.lakko.app.profile.AvatarRefs
import me.lakko.app.profile.DisplayNameCheck
import me.lakko.app.profile.InlineField
import me.lakko.app.profile.PhotoEdit
import me.lakko.app.profile.ProfileEdit
import me.lakko.app.profile.SaveStep
import me.lakko.app.profile.afterSaved
import me.lakko.app.profile.avatarToPublish
import me.lakko.app.profile.checkDisplayName
import me.lakko.app.profile.pendingUsername
import me.lakko.app.profile.saveSteps

/** What a save needs from the rest of the app. */
public interface ProfileSaveDeps {
    /** Encrypt and upload [bytes]; the reference, or a throw. */
    public suspend fun upload(bytes: ByteArray, mime: String, side: Int): AvatarBlob

    /** Whether a fresh-auth proof is live now (a username change needs one). */
    public fun proofLive(): Boolean

    /** The existing rename operation (ADR-0076). */
    public suspend fun changeUsername(newUsername: String): RenameResult

    /** Store and republish the own profile with [name] and [avatar]. */
    public fun publish(name: String?, avatar: AvatarRefs?)
}

/** How one save ended. */
public sealed interface SaveOutcome {
    public data object Saved : SaveOutcome
    public data class Failed(val line: UiText) : SaveOutcome

    /** The person closed the proof block: the save stopped there. */
    public data object Stopped : SaveOutcome
}

/**
 * docs/ux/profile-screen.md — the Settings > Profile state, held across a
 * pushed screen (the crop and camera screens, a link) and while the app is
 * in the background (§6: that is not a navigation).
 */
public class ProfileEditViewModel : ViewModel() {
    public var edit: ProfileEdit? by mutableStateOf(null)
        private set

    /** The pending still to show at once (§2.4). */
    public var pendingStill: Bitmap? by mutableStateOf(null)
        private set

    /** The error line under Save, or `null`. */
    public var error: UiText? by mutableStateOf(null)
        private set

    /** `Saved. Your contacts will see the change.` is showing. */
    public var savedLine: Boolean by mutableStateOf(false)
        private set

    /** The crop screen is open on this source. */
    public var cropSource: AvatarMedia.Source? by mutableStateOf(null)

    /** The camera screen is open. */
    public var cameraOpen: Boolean by mutableStateOf(false)

    /** The proof block is asked for; completed `true` when a proof is live,
     *  `false` when the person closed it. */
    public var proofRequest: CompletableDeferred<Boolean>? by mutableStateOf(null)
        private set

    private var savedLineJob: Job? = null

    /** Seed from the stored profile; later calls follow the saved values. */
    public fun bind(savedName: String, savedUsername: String, savedAvatar: AvatarRefs?) {
        val current = edit
        edit = if (current == null) {
            ProfileEdit(InlineField(savedName), InlineField(savedUsername), savedAvatar = savedAvatar)
        } else {
            current.copy(
                name = current.name.withSaved(savedName),
                username = current.username.withSaved(savedUsername),
                savedAvatar = savedAvatar,
            )
        }
    }

    public fun update(transform: (ProfileEdit) -> ProfileEdit) {
        val e = edit ?: return
        edit = transform(e)
        error = null
        savedLine = false
    }

    /** `Use` on the crop screen. */
    public fun usePhoto(encoded: AvatarMedia.Encoded) {
        update {
            it.copy(
                photo = PhotoEdit.New(
                    still = encoded.still,
                    stillSide = encoded.stillSide,
                    video = encoded.video,
                    videoSide = encoded.videoSide,
                    videoDurationMs = encoded.videoDurationMs,
                ),
            )
        }
        pendingStill = encoded.stillBitmap
        cropSource = null
    }

    /** `Remove photo`. */
    public fun removePhoto() {
        update { it.copy(photo = PhotoEdit.Removed) }
        pendingStill = null
    }

    /** `Don't save` (§6): every pending change dropped. */
    public fun discard() {
        val e = edit ?: return
        edit = ProfileEdit(InlineField(e.name.saved), InlineField(e.username.saved), savedAvatar = e.savedAvatar)
        pendingStill = null
        error = null
    }

    /** The proof block's outcome. */
    public fun answerProof(live: Boolean) {
        proofRequest?.complete(live)
        proofRequest = null
    }

    /** §5 — run the save in order; [onDone] gets the outcome. */
    public fun save(deps: ProfileSaveDeps, onDone: (SaveOutcome) -> Unit = {}) {
        val start = edit ?: return
        if (start.saving) return
        edit = start.copy(saving = true, name = start.name.closed(true), username = start.username.closed(true))
        error = null
        savedLine = false
        viewModelScope.launch {
            val outcome = try {
                runSave(deps)
            } catch (e: Throwable) {
                if (e is kotlinx.coroutines.CancellationException) throw e
                SaveOutcome.Failed(UiText("profile.save.error.generic"))
            }
            edit = edit?.copy(saving = false)
            when (outcome) {
                SaveOutcome.Saved -> showSavedLine()
                is SaveOutcome.Failed -> error = outcome.line
                SaveOutcome.Stopped -> Unit
            }
            onDone(outcome)
        }
    }

    private suspend fun runSave(deps: ProfileSaveDeps): SaveOutcome {
        val steps = edit?.saveSteps() ?: return SaveOutcome.Saved
        if (SaveStep.UPLOAD_PHOTO in steps) {
            val photo = edit?.photo as? PhotoEdit.New ?: return SaveOutcome.Failed(UiText("profile.save.error.generic"))
            val uploaded = try {
                val still = deps.upload(photo.still, "image/jpeg", photo.stillSide)
                val videoFile = photo.video
                val video = if (videoFile != null && photo.videoSide != null && photo.videoDurationMs != null) {
                    deps.upload(videoFile.readBytes(), "video/mp4", photo.videoSide)
                } else {
                    null
                }
                AvatarRefs(still, video, if (video != null) photo.videoDurationMs else null)
            } catch (e: Throwable) {
                if (e is kotlinx.coroutines.CancellationException) throw e
                return SaveOutcome.Failed(UiText(me.lakko.app.media.profilePhotoUploadFailureKey(e)))
            }
            edit = edit?.copy(photo = photo.copy(uploaded = uploaded))
        }
        if (SaveStep.CHANGE_USERNAME in steps) {
            val wanted = edit?.let { pendingUsername(it.username.value) }
                ?: return SaveOutcome.Failed(UiText("profile.save.error.generic"))
            var attempts = 0
            while (true) {
                attempts += 1
                if (!deps.proofLive()) {
                    val ask = CompletableDeferred<Boolean>()
                    proofRequest = ask
                    if (!ask.await()) return SaveOutcome.Stopped
                }
                when (val result = deps.changeUsername(wanted)) {
                    is RenameResult.Done -> {
                        edit = edit?.let { it.copy(username = InlineField(result.username)) }
                        break
                    }
                    is RenameResult.Refused -> when {
                        result.needsProof && attempts < 2 -> continue
                        result.line.key == TAKEN_KEY -> {
                            edit = edit?.let { it.copy(username = it.username.opened()) }
                            return SaveOutcome.Failed(
                                UiText("profile.username.taken", texts = mapOf("username" to "⁦@$wanted⁩")),
                            )
                        }
                        else -> return SaveOutcome.Failed(UiText("profile.save.error.generic"))
                    }
                    is RenameResult.Pending -> return SaveOutcome.Failed(UiText("profile.save.error.generic"))
                }
            }
        }
        val now = edit ?: return SaveOutcome.Saved
        if (SaveStep.PUBLISH in steps) {
            val name = (checkDisplayName(now.name.value) as? DisplayNameCheck.Valid)?.normalized
            val avatar = now.avatarToPublish()
            try {
                deps.publish(name, avatar)
            } catch (e: Throwable) {
                if (e is kotlinx.coroutines.CancellationException) throw e
                return SaveOutcome.Failed(UiText("profile.save.error.generic"))
            }
            edit = now.afterSaved(name.orEmpty(), now.username.saved, avatar).copy(saving = true)
            // The uploaded bytes are in the avatar cache: the saved photo
            // shows from there now.
            pendingStill = null
        } else {
            edit = now.afterSaved(now.name.saved, now.username.saved, now.savedAvatar).copy(saving = true)
        }
        return SaveOutcome.Saved
    }

    private fun showSavedLine() {
        savedLine = true
        savedLineJob?.cancel()
        savedLineJob = viewModelScope.launch {
            delay(SAVED_LINE_MS)
            savedLine = false
        }
    }

    private companion object {
        const val TAKEN_KEY = "auth.signup.error.username_taken"
        const val SAVED_LINE_MS = 4_000L
    }
}
