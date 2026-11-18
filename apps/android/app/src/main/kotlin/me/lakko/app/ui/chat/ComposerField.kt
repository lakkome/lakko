// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

@file:OptIn(
    androidx.compose.foundation.ExperimentalFoundationApi::class,
    androidx.compose.material3.ExperimentalMaterial3Api::class,
)

package me.lakko.app.ui.chat

import androidx.compose.foundation.content.ReceiveContentListener
import androidx.compose.foundation.content.TransferableContent
import androidx.compose.foundation.content.consume
import androidx.compose.foundation.content.contentReceiver
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.layout.defaultMinSize
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.text.input.TextFieldDecorator
import androidx.compose.foundation.text.input.TextFieldLineLimits
import androidx.compose.foundation.text.input.TextFieldState
import androidx.compose.foundation.text.selection.LocalTextSelectionColors
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextFieldDefaults
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.text.input.VisualTransformation
import androidx.compose.ui.unit.dp
import me.lakko.app.chat.PastedMedia
import me.lakko.app.chat.pasteAttachesMime
import me.lakko.app.chat.resolvePastedMime

/**
 * The conversation composer's text field: the pill of UX 5.2, five lines then
 * scrolling.
 *
 * It is a state-based [BasicTextField] rather than the value-based
 * `OutlinedTextField` it replaced, because only the state-based field takes
 * [contentReceiver] in Compose Foundation 1.7. That receiver is what makes a
 * pasted photo or video, or an image or GIF the keyboard inserts, attach
 * (docs/ux/attachments.md 2.5): the clipboard's Paste and the keyboard's rich
 * content both arrive there. Items that are not a photo or video, and plain
 * text, are handed back and paste as text, as before.
 *
 * With [onPasteMedia] null the field takes no media at all and a paste is
 * text only: a place that cannot attach (a channel subscriber) passes null.
 *
 * The decoration is Material's own outlined decoration box, so the field looks
 * as the `OutlinedTextField` did.
 */
@Composable
internal fun ComposerTextField(
    state: TextFieldState,
    placeholder: String,
    textStyle: TextStyle,
    onPasteMedia: ((List<PastedMedia>) -> Unit)?,
    modifier: Modifier = Modifier,
) {
    val context = LocalContext.current
    val currentOnPasteMedia by rememberUpdatedState(onPasteMedia)
    val takesMedia = onPasteMedia != null
    val receiver = remember(context) {
        ReceiveContentListener { content ->
            val handler = currentOnPasteMedia
            if (handler == null) content else takePastedMedia(context, content, handler)
        }
    }
    val interactionSource = remember { MutableInteractionSource() }
    val colors = OutlinedTextFieldDefaults.colors(
        focusedContainerColor = MaterialTheme.colorScheme.surfaceVariant,
        unfocusedContainerColor = MaterialTheme.colorScheme.surfaceVariant,
        disabledContainerColor = MaterialTheme.colorScheme.surfaceVariant,
        focusedBorderColor = Color.Transparent,
        unfocusedBorderColor = Color.Transparent,
        disabledBorderColor = Color.Transparent,
    )
    val shape = RoundedCornerShape(24.dp)
    CompositionLocalProvider(LocalTextSelectionColors provides colors.textSelectionColors) {
        BasicTextField(
            state = state,
            modifier = modifier
                .defaultMinSize(minHeight = OutlinedTextFieldDefaults.MinHeight)
                .then(if (takesMedia) Modifier.contentReceiver(receiver) else Modifier),
            textStyle = textStyle.copy(color = colors.focusedTextColor),
            keyboardOptions = KeyboardOptions(capitalization = KeyboardCapitalization.Sentences),
            lineLimits = TextFieldLineLimits.MultiLine(maxHeightInLines = 5),
            interactionSource = interactionSource,
            cursorBrush = SolidColor(colors.cursorColor),
            decorator = TextFieldDecorator { innerTextField ->
                OutlinedTextFieldDefaults.DecorationBox(
                    value = state.text.toString(),
                    innerTextField = innerTextField,
                    enabled = true,
                    singleLine = false,
                    visualTransformation = VisualTransformation.None,
                    interactionSource = interactionSource,
                    placeholder = { Text(placeholder) },
                    colors = colors,
                    container = {
                        OutlinedTextFieldDefaults.Container(
                            enabled = true,
                            isError = false,
                            interactionSource = interactionSource,
                            colors = colors,
                            shape = shape,
                        )
                    },
                )
            },
        )
    }
}

/**
 * Take the photos and videos out of [content] and hand them on; return what is
 * left (null when nothing is) so the field pastes it as text. Each item's
 * type comes from its content provider, or from the clip when the provider
 * says nothing. The bytes are read right after this returns (see
 * `ChatViewModel.openSendPreview`), while the read grant that came with the
 * clip or the keyboard insertion still holds.
 */
private fun takePastedMedia(
    context: android.content.Context,
    content: TransferableContent,
    onPasteMedia: (List<PastedMedia>) -> Unit,
): TransferableContent? {
    val description = content.clipMetadata.clipDescription
    val clipTypes = (0 until description.mimeTypeCount).map { description.getMimeType(it) }
    val taken = mutableListOf<PastedMedia>()
    val rest = content.consume { item ->
        val uri = item.uri ?: return@consume false
        val itemType = runCatching { context.contentResolver.getType(uri) }.getOrNull()
        val mime = resolvePastedMime(itemType, clipTypes)
        if (mime != null && pasteAttachesMime(mime)) {
            taken += PastedMedia(uri, mime)
            true
        } else {
            false
        }
    }
    if (taken.isNotEmpty()) onPasteMedia(taken)
    return rest
}
