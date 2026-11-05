// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.theme

import android.content.Context
import android.content.SharedPreferences
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.compositionLocalOf
import androidx.compose.runtime.getValue
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.unit.Density
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

/**
 * `docs/ux/text-size.md` — the per-device text size of messages and the
 * reactions under them: Small 0.87, Medium 1.0 (the design system as it is),
 * Large 1.2. Nothing else in the interface follows it.
 */
public enum class TextSize(public val factor: Float, public val labelKey: String) {
    Small(0.87f, "settings.text_size.small"),
    Medium(1.0f, "settings.text_size.medium"),
    Large(1.2f, "settings.text_size.large"),
    ;

    public companion object {
        /** A stored value this client does not recognise reads as Medium. */
        public fun fromStored(stored: String?): TextSize = entries.firstOrNull { it.name == stored } ?: Medium
    }
}

/** The stored choice: plain `SharedPreferences` on this device only (it is
 *  never sent to the account's other devices or a server). */
public object TextSizePreference {
    internal const val PREFS: String = "lakko-appearance"
    internal const val KEY: String = "text-size"
    private val flow = MutableStateFlow(TextSize.Medium)
    private var prefs: SharedPreferences? = null

    public val size: StateFlow<TextSize> = flow.asStateFlow()

    /** Load the stored value once per process. Safe to call repeatedly. */
    public fun init(context: Context) {
        if (prefs != null) return
        val p = context.applicationContext.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
        prefs = p
        flow.value = TextSize.fromStored(p.getString(KEY, null))
    }

    /** Applies at once, everywhere a chat is drawn. */
    public fun set(size: TextSize) {
        flow.value = size
        prefs?.edit()?.putString(KEY, size.name)?.apply()
    }

    /** Tests only: forget the loaded preferences. */
    internal fun resetForTests() {
        prefs = null
        flow.value = TextSize.Medium
    }
}

/** The text-size factor of the subtree, 1.0 outside chat content. */
public val LocalChatTextFactor = compositionLocalOf { 1.0f }

/**
 * Draw [content] at the chat text size: the font scale inside is the
 * system's times [factor], so every `sp` grows with it (on Android 14 and
 * later the platform converts a font scale above 1 non-linearly, so the
 * largest sizes grow slightly less than the factor, as they do under the
 * system setting itself), and
 * [LocalChatTextFactor] carries the factor for `dp` values that scale with
 * it (the reaction chips). Nested use does not multiply twice.
 */
@Composable
public fun ChatTextScale(
    factor: Float = chatTextFactor(),
    content: @Composable () -> Unit,
) {
    val base = LocalUnscaledDensity.current ?: LocalDensity.current
    CompositionLocalProvider(
        LocalDensity provides if (factor == 1.0f) base else Density(base.density, base.fontScale * factor),
        LocalUnscaledDensity provides base,
        LocalChatTextFactor provides factor,
        content = content,
    )
}

/** The density outside the chat text scale, so a nested scale starts from
 *  the system's and never multiplies twice. */
private val LocalUnscaledDensity = compositionLocalOf<Density?> { null }


/** Draw [content] back at the system text size inside a scaled subtree
 *  (the action card of the message menu is a menu, not chat text). */
@Composable
public fun SystemTextScale(content: @Composable () -> Unit) {
    ChatTextScale(factor = 1.0f, content = content)
}

/** The current factor of the stored choice. */
@Composable
public fun chatTextFactor(): Float {
    val size by TextSizePreference.size.collectAsStateWithLifecycle()
    return size.factor
}
