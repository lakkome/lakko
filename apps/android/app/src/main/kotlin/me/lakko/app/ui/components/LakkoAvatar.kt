// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.components

import android.graphics.Bitmap
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Groups
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalInspectionMode
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import me.lakko.app.avatar.AvatarImages
import me.lakko.app.l10n.LocalStrings
import me.lakko.app.profile.AvatarRefs
import me.lakko.app.ui.chat.initialsOf
import me.lakko.app.ui.theme.avatarTintFor
import me.lakko.app.ui.theme.isLakkoDark
import java.io.File

/**
 * The one avatar circle of the client (design system section 2; UX
 * `docs/ux/profile-screen.md` §7).
 *
 * Initials on one of four tints derived from [seed]; a group without a name
 * gets the group glyph. For a PERSON, pass [personKey] (the raw Ed25519
 * signature key): the circle shows their profile photo when this device
 * holds it — downloaded lazily on first display, never from background code —
 * and the initials meanwhile or when it failed (tried again on a later
 * display after a minute). [avatar] names the references directly (the own
 * profile); [localStill] / [localVideo] show bytes not yet uploaded (the
 * pending photo on the profile screen).
 *
 * A profile video plays muted and looped only when [large] (contact detail,
 * a person's info sheet, Settings > Profile); everywhere else its still.
 */
@Composable
public fun LakkoAvatar(
    name: String,
    seed: String = name,
    size: Dp = 40.dp,
    group: Boolean = false,
    modifier: Modifier = Modifier,
    personKey: ByteArray? = null,
    avatar: AvatarRefs? = null,
    large: Boolean = false,
    localStill: Bitmap? = null,
    localVideo: File? = null,
) {
    val tint = avatarTintFor(seed, isLakkoDark())
    val inspection = LocalInspectionMode.current
    val refs = if (inspection || group) avatar else avatar ?: personAvatar(personKey)
    Box(
        contentAlignment = Alignment.Center,
        modifier = modifier
            .size(size)
            .clip(CircleShape)
            .background(tint.background),
    ) {
        // The unnamed-group stand-in is rendered in the interface language
        // (`chat.group.fallback_name`), so the glyph keys on that, not on
        // the English word.
        if (group && name == LocalStrings.current["chat.group.fallback_name"]) {
            Icon(
                Icons.Filled.Groups,
                contentDescription = null,
                tint = tint.foreground,
                modifier = Modifier.size(size * 0.55f),
            )
        } else {
            Text(
                text = initialsOf(name),
                color = tint.foreground,
                fontWeight = FontWeight.SemiBold,
                fontSize = (size.value * 0.4f).sp,
                style = MaterialTheme.typography.labelLarge,
            )
        }
        when {
            localStill != null -> {
                AvatarBitmap(localStill)
                if (large && localVideo != null) AvatarVideo(localVideo)
            }
            refs != null && !inspection -> StoredAvatar(refs, large)
        }
    }
}

/** The stored avatar references of the person with signature key [key]. */
@Composable
private fun personAvatar(key: ByteArray?): AvatarRefs? {
    if (key == null || key.isEmpty()) return null
    val context = LocalContext.current
    val directory = remember(context) { me.lakko.app.profile.ProfileDirectory.get(context) }
    val version by directory.version.collectAsState()
    val keyB64 = remember(key.contentHashCode()) { java.util.Base64.getEncoder().encodeToString(key) }
    return remember(keyB64, version) { directory.profiles.get(keyB64)?.avatar }
}

@Composable
private fun StoredAvatar(refs: AvatarRefs, large: Boolean) {
    val context = LocalContext.current
    val revision by AvatarImages.revision.collectAsState()
    val still by produceState(AvatarImages.cachedBitmap(refs.still), refs.still.blobIdHex, revision) {
        if (value == null) value = AvatarImages.still(context, refs.still)
    }
    val bitmap = still ?: return
    AvatarBitmap(bitmap)
    val video = refs.video
    if (large && video != null) {
        val file by produceState<File?>(null, video.blobIdHex, revision) {
            value = AvatarImages.videoFile(context, video)
        }
        file?.let { AvatarVideo(it) }
    }
}

@Composable
private fun AvatarBitmap(bitmap: Bitmap) {
    val image = remember(bitmap) { bitmap.asImageBitmap() }
    Image(
        bitmap = image,
        contentDescription = null,
        contentScale = ContentScale.Crop,
        modifier = Modifier.fillMaxSize(),
    )
}

@Composable
private fun AvatarVideo(file: File) {
    me.lakko.app.ui.chat.VideoSurface(
        file = file,
        playing = true,
        muted = true,
        looping = true,
        modifier = Modifier.fillMaxSize(),
    )
}

/**
 * The decoded profile still of the person with signature key [personKey],
 * or `null` while none is held (a call tile draws its own initials then).
 * Downloads lazily, like [LakkoAvatar].
 */
@Composable
public fun rememberPersonStill(personKey: ByteArray?, avatar: AvatarRefs? = null): Bitmap? {
    if (LocalInspectionMode.current) return null
    val refs = avatar ?: personAvatar(personKey) ?: return null
    val context = LocalContext.current
    val revision by AvatarImages.revision.collectAsState()
    val still by produceState(AvatarImages.cachedBitmap(refs.still), refs.still.blobIdHex, revision) {
        if (value == null) value = AvatarImages.still(context, refs.still)
    }
    return still
}

/**
 * This account's own published photo (the signed-in account's stored
 * profile), re-read when the profile store changes. For the own tile in a
 * call and other places that draw the owner without a profile at hand.
 */
@Composable
public fun rememberOwnAvatar(): AvatarRefs? {
    if (LocalInspectionMode.current) return null
    val context = LocalContext.current
    val revision by me.lakko.app.profile.OwnProfileStore.revision.collectAsState()
    val account = me.lakko.app.contacts.AcceptedContactsStore.boundAccountB64
    return remember(account, revision) {
        val id = account?.let {
            try {
                java.util.Base64.getDecoder().decode(it)
            } catch (_: IllegalArgumentException) {
                null
            }
        } ?: return@remember null
        try {
            me.lakko.app.profile.OwnProfileStore.load(context, id)?.avatar
        } catch (_: Throwable) {
            null
        }
    }
}

/**
 * The name the owner's initials come from when they have no photo: their
 * display name, their username when they have none. "You" is a label, not a
 * name, so a caller keeps [fallback] only when neither is known yet.
 */
@Composable
public fun rememberOwnAvatarName(fallback: String): String {
    if (LocalInspectionMode.current) return fallback
    val context = LocalContext.current
    val revision by me.lakko.app.profile.OwnProfileStore.revision.collectAsState()
    val account = me.lakko.app.contacts.AcceptedContactsStore.boundAccountB64
    return remember(account, revision, fallback) {
        val id = account?.let {
            try {
                java.util.Base64.getDecoder().decode(it)
            } catch (_: IllegalArgumentException) {
                null
            }
        } ?: return@remember fallback
        val own = try {
            me.lakko.app.profile.OwnProfileStore.load(context, id)
        } catch (_: Throwable) {
            null
        }
        own?.displayName?.takeIf { it.isNotBlank() }
            ?: own?.username?.takeIf { it.isNotBlank() }
            ?: fallback
    }
}

/**
 * A person's signature key from either of the two spellings the screens
 * carry it in (standard base64, the profile store's; unpadded base64url, the
 * contact routes'), or `null`.
 */
public fun personKeyBytes(encoded: String?): ByteArray? {
    val s = encoded?.takeIf { it.isNotEmpty() } ?: return null
    // A 32-byte key is 44 characters in padded standard base64 and 43 in
    // unpadded base64url.
    val bytes = try {
        if (s.endsWith("=")) java.util.Base64.getDecoder().decode(s) else java.util.Base64.getUrlDecoder().decode(s)
    } catch (_: IllegalArgumentException) {
        return null
    }
    return bytes.takeIf { it.size == 32 }
}
