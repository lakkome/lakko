// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.components

import android.content.ActivityNotFoundException
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.util.Log
import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.gestures.awaitFirstDown
import androidx.compose.foundation.gestures.waitForUpOrCancellation
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.input.pointer.PointerEventPass
import androidx.compose.ui.input.pointer.PointerInputScope
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalViewConfiguration
import androidx.compose.ui.platform.ViewConfiguration
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.LinkAnnotation
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.TextLinkStyles
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.style.TextDecoration
import androidx.compose.ui.text.withLink
import androidx.compose.ui.unit.DpSize
import androidx.compose.ui.util.fastAny
import androidx.compose.ui.util.fastForEach
import me.lakko.app.config.ApiEndpoints
import me.lakko.app.contacts.LakkoLink
import me.lakko.app.contacts.lakkoLinkAt
import uniffi.lakko_ffi_uniffi.MessageTextPiece
import uniffi.lakko_ffi_uniffi.splitMessageLinks

private const val TAG = "MessageLinkText"

/**
 * Hands a Lakko link of this deployment to the app's link intake, the one an
 * opened app link reaches, so the screen is the one the person gets from
 * outside. `null`, the default, means no intake is here and the link goes to
 * the browser, which shows the same screen.
 */
internal val LocalOpenLakkoLink = staticCompositionLocalOf<((LakkoLink) -> Unit)?> { null }

/** Where a tap on a link in a message goes (`docs/ux/message-links.md` section 2). */
public sealed interface MessageLinkOpen {
    /** A Lakko link of this deployment: its screen, in place. */
    public data class InApp(val link: LakkoLink) : MessageLinkOpen

    /** Anything else: the system's default browser. */
    public data class Browser(val url: String) : MessageLinkOpen
}

/** The decision alone, from the address, so it can be tested without a screen. */
public fun messageLinkOpenOf(url: String, ownOrigin: String): MessageLinkOpen {
    val own = lakkoLinkAt(url, ownOrigin) ?: return MessageLinkOpen.Browser(url)
    return MessageLinkOpen.InApp(own)
}

/** The text as pieces. If the rule cannot be asked the whole text is one plain
 *  piece: a link that cannot be tapped loses nothing, a bubble that does not
 *  draw loses the message. */
internal fun messageTextPieces(
    text: String,
    split: (String) -> List<MessageTextPiece>,
): List<MessageTextPiece> =
    try {
        split(text)
    } catch (cause: RuntimeException) {
        listOf(MessageTextPiece(text = text, isLink = false))
    } catch (cause: LinkageError) {
        // The native library that holds the rule is not loaded.
        listOf(MessageTextPiece(text = text, isLink = false))
    }

/**
 * Text of a message or a channel post with its links live. Which runs are
 * links is the core's rule (`splitMessageLinks`); no client searches text
 * itself. A text with no link is the plain [Text] it always was.
 *
 * [onLongPress] is what a long press does on the text around the links, passed
 * in so that it also works ON a link: see [watchLongPress].
 */
@Composable
internal fun MessageLinkText(
    text: String,
    style: TextStyle,
    linkColor: Color,
    color: Color = Color.Unspecified,
    onLongPress: (() -> Unit)? = null,
    split: (String) -> List<MessageTextPiece> = ::splitMessageLinks,
) {
    val pieces = remember(text) { messageTextPieces(text, split) }
    if (pieces.none { it.isLink }) {
        Text(text = text, style = style, color = color)
        return
    }
    val context = LocalContext.current
    val openLakkoLink = LocalOpenLakkoLink.current
    val open by rememberUpdatedState { url: String ->
        val target = messageLinkOpenOf(url, ApiEndpoints.webOrigin)
        if (target is MessageLinkOpen.InApp && openLakkoLink != null) {
            openLakkoLink(target.link)
        } else {
            openInSystemBrowser(context, url)
        }
    }
    // Kept across recompositions: restarting the gesture watcher on every new
    // lambda would lose a press that is under way when a poll redraws the row.
    val latestLongPress by rememberUpdatedState(onLongPress)
    val annotated = remember(pieces, linkColor) { linkedText(pieces, linkColor) { url -> open(url) } }
    val viewConfiguration = LocalViewConfiguration.current
    val exactTouch = remember(viewConfiguration) { ExactTouch(viewConfiguration) }
    CompositionLocalProvider(LocalViewConfiguration provides exactTouch) {
        Text(
            text = annotated,
            style = style,
            color = color,
            modifier = Modifier.pointerInput(Unit) { watchLongPress { latestLongPress } },
        )
    }
}

/** Underlined as well as coloured, so colour is not the only signal. */
private fun linkedText(
    pieces: List<MessageTextPiece>,
    linkColor: Color,
    onOpen: (String) -> Unit,
): AnnotatedString {
    val linkStyles = TextLinkStyles(SpanStyle(color = linkColor, textDecoration = TextDecoration.Underline))
    return buildAnnotatedString {
        pieces.fastForEach { piece ->
            if (piece.isLink) {
                val link = LinkAnnotation.Url(piece.text, linkStyles) { onOpen(piece.text) }
                withLink(link) { append(piece.text) }
            } else {
                append(piece.text)
            }
        }
    }
}

/**
 * No grown touch target for a link in running text. Compose grows anything
 * clickable to 48dp, and the rectangle it grows is that of the whole link, so
 * the word on the line below opened the link, and on a link that wraps so did
 * everything beside its last line. Compose 1.7 lets a link's clip shape limit a
 * direct hit but not that near hit, and with no near hit the shape is exact.
 */
private class ExactTouch(base: ViewConfiguration) : ViewConfiguration by base {
    override val minimumTouchTargetSize: DpSize get() = DpSize.Zero
}

/**
 * A long press on a link is the message menu, never the link. Compose draws a
 * link as a child that takes the press for itself, so the bubble's own long
 * press never sees a finger that came down on a link. This watches from the
 * outside, in the pass that runs before any child, with the timing and the
 * cancellation of the bubble's own gesture, and takes nothing away from a tap.
 * Once the press has become a long press the rest of the gesture is consumed,
 * so lifting the finger does not open the link. On the text between links both
 * this and the bubble answer, and both only open the same menu.
 *
 * With no menu here (a channel post) a long press still does not open the link,
 * and the finger stays free to scroll.
 */
private suspend fun PointerInputScope.watchLongPress(current: () -> (() -> Unit)?) {
    awaitEachGesture {
        awaitFirstDown(requireUnconsumed = false, pass = PointerEventPass.Initial)
        var ended = false
        withTimeoutOrNull(viewConfiguration.longPressTimeoutMillis) {
            waitForUpOrCancellation(PointerEventPass.Initial)
            ended = true
        }
        if (ended) return@awaitEachGesture
        val onLongPress = current()
        if (onLongPress == null) {
            waitForUpOrCancellation(PointerEventPass.Initial)?.consume()
            return@awaitEachGesture
        }
        onLongPress()
        do {
            val event = awaitPointerEvent(PointerEventPass.Initial)
            event.changes.fastForEach { it.consume() }
        } while (event.changes.fastAny { it.pressed })
    }
}

/** `CATEGORY_BROWSABLE` so that only what a browser could open is reached, and
 *  a normalised scheme because the rule accepts `HTTPS://` while an intent
 *  filter matches the scheme exactly. The address is not logged. */
/** Open [url] in the system's default browser; no in-app web view. */
internal fun openInSystemBrowser(context: Context, url: String) {
    val intent = Intent(Intent.ACTION_VIEW, Uri.parse(url).normalizeScheme())
        .addCategory(Intent.CATEGORY_BROWSABLE)
    try {
        context.startActivity(intent)
    } catch (e: ActivityNotFoundException) {
        Log.w(TAG, "no activity opens this link", e)
    }
}
