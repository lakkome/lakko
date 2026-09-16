// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.theme

import android.content.Context
import android.content.SharedPreferences
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.ColorScheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Shapes
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

/**
 * The Lakko colour tokens — docs/ux/client-redesign.md section 4, mirrored
 * one-to-one from the web `styles.css` variables. One indigo hue, flat
 * surfaces, no violet; the dark half is what the call surfaces use.
 *
 * Never reference a hex colour from a screen: read `MaterialTheme.colorScheme`
 * (built below from these tokens) or, for a surface that is dark by design,
 * [LakkoDark].
 */
public interface LakkoTokens {
    public val background: Color
    public val surface: Color
    public val sunken: Color
    public val selected: Color
    public val border: Color
    public val text: Color
    public val textMuted: Color
    public val accent: Color
    public val accentFg: Color
    public val accentSoft: Color
    public val danger: Color
    public val dangerSoft: Color
    public val dangerFg: Color
    public val success: Color
    public val warning: Color
}

/** Light tokens. */
public object LakkoLight : LakkoTokens {
    override val background: Color = Color(0xFFF6F7FB)
    override val surface: Color = Color(0xFFFFFFFF)
    override val sunken: Color = Color(0xFFEEF0F6)
    override val selected: Color = Color(0xFFE0E7FF)
    override val border: Color = Color(0xFFE4E7EF)
    override val text: Color = Color(0xFF1A1D2B)
    override val textMuted: Color = Color(0xFF6B7280)
    override val accent: Color = Color(0xFF3730A3)
    override val accentFg: Color = Color(0xFFFFFFFF)
    override val accentSoft: Color = Color(0xFFE0E7FF)
    override val danger: Color = Color(0xFFDC2626)
    override val dangerSoft: Color = Color(0xFFFEF2F2)
    override val dangerFg: Color = Color(0xFFFFFFFF)
    override val success: Color = Color(0xFF16A34A)
    override val warning: Color = Color(0xFFB45309)
}

/** Dark tokens. Calls are dark by design and read these directly. */
public object LakkoDark : LakkoTokens {
    override val background: Color = Color(0xFF0F1220)
    override val surface: Color = Color(0xFF171B2E)
    override val sunken: Color = Color(0xFF1F2439)
    override val selected: Color = Color(0xFF26295A)
    override val border: Color = Color(0xFF2A3145)
    override val text: Color = Color(0xFFE6E9F4)
    override val textMuted: Color = Color(0xFF9AA3BD)
    override val accent: Color = Color(0xFF818CF8)
    override val accentFg: Color = Color(0xFF0F1220)
    override val accentSoft: Color = Color(0xFF26295A)
    override val danger: Color = Color(0xFFF87171)
    override val dangerSoft: Color = Color(0xFF3A1F24)
    override val dangerFg: Color = Color(0xFF14080A)
    override val success: Color = Color(0xFF4ADE80)
    override val warning: Color = Color(0xFFF59E0B)
}

/**
 * Avatar tints (design system section 2): four background / foreground pairs
 * chosen by a hash of the identity, indigo / teal / slate / sand. Never violet.
 */
public data class AvatarTint(val background: Color, val foreground: Color)

public fun avatarTints(dark: Boolean): List<AvatarTint> = if (dark) {
    listOf(
        AvatarTint(Color(0xFF26295A), Color(0xFFA5B4FC)),
        AvatarTint(Color(0xFF12313A), Color(0xFF6FD0E0)),
        AvatarTint(Color(0xFF252A3D), Color(0xFFAEB6D0)),
        AvatarTint(Color(0xFF3A2F17), Color(0xFFE5B96B)),
    )
} else {
    listOf(
        AvatarTint(Color(0xFFE0E7FF), Color(0xFF3730A3)),
        AvatarTint(Color(0xFFE0F2F6), Color(0xFF0F6F80)),
        AvatarTint(Color(0xFFE9EBF3), Color(0xFF46506E)),
        AvatarTint(Color(0xFFF6EFE3), Color(0xFF8A5A17)),
    )
}

/** The tint for [seed] (a name, a key, an id) — stable across launches. */
public fun avatarTintFor(seed: String, dark: Boolean): AvatarTint {
    val tints = avatarTints(dark)
    var h = 0
    for (ch in seed) h = (h * 31 + ch.code) and 0x7fffffff
    return tints[h % tints.size]
}

private fun schemeOf(t: LakkoTokens, dark: Boolean): ColorScheme {
    val base = if (dark) darkColorScheme() else lightColorScheme()
    return base.copy(
        primary = t.accent,
        onPrimary = t.accentFg,
        primaryContainer = t.accentSoft,
        onPrimaryContainer = if (dark) t.text else t.accent,
        inversePrimary = if (dark) LakkoLight.accent else LakkoDark.accent,
        secondary = t.textMuted,
        onSecondary = t.surface,
        secondaryContainer = t.selected,
        onSecondaryContainer = if (dark) t.text else t.accent,
        tertiary = t.accent,
        onTertiary = t.accentFg,
        tertiaryContainer = t.accentSoft,
        onTertiaryContainer = if (dark) t.text else t.accent,
        background = t.background,
        onBackground = t.text,
        surface = t.surface,
        onSurface = t.text,
        surfaceVariant = t.sunken,
        onSurfaceVariant = t.textMuted,
        surfaceTint = t.accent,
        inverseSurface = if (dark) LakkoLight.surface else LakkoDark.surface,
        inverseOnSurface = if (dark) LakkoLight.text else LakkoDark.text,
        error = t.danger,
        onError = t.dangerFg,
        errorContainer = t.dangerSoft,
        onErrorContainer = t.danger,
        outline = t.border,
        outlineVariant = t.border,
        scrim = Color.Black,
        surfaceBright = t.surface,
        surfaceDim = t.background,
        surfaceContainer = t.surface,
        surfaceContainerLowest = t.surface,
        surfaceContainerLow = t.surface,
        surfaceContainerHigh = t.sunken,
        surfaceContainerHighest = t.sunken,
    )
}

/** Material 3 light scheme built from [LakkoLight]. Dynamic colour is off. */
public val LakkoLightColorScheme: ColorScheme = schemeOf(LakkoLight, dark = false)

/** Material 3 dark scheme built from [LakkoDark]. */
public val LakkoDarkColorScheme: ColorScheme = schemeOf(LakkoDark, dark = true)

/** Radius 8 for controls, 12 for cards and rows, 18 for bubbles. */
public val LakkoShapes: Shapes = Shapes(
    extraSmall = RoundedCornerShape(8.dp),
    small = RoundedCornerShape(8.dp),
    medium = RoundedCornerShape(12.dp),
    large = RoundedCornerShape(18.dp),
    extraLarge = RoundedCornerShape(28.dp),
)

/** The user's theme override (Settings > Appearance). */
public enum class ThemeMode { System, Light, Dark }

/**
 * Persisted theme preference. Follows the system by default; the override is
 * stored in plain `SharedPreferences` (it is not sensitive).
 */
public object ThemePreference {
    private const val PREFS = "lakko-appearance"
    private const val KEY = "theme-mode"
    private val flow = MutableStateFlow(ThemeMode.System)
    private var prefs: SharedPreferences? = null

    public val mode: StateFlow<ThemeMode> = flow.asStateFlow()

    /** Load the stored value once per process. Safe to call repeatedly. */
    public fun init(context: Context) {
        if (prefs != null) return
        val p = context.applicationContext.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
        prefs = p
        flow.value = p.getString(KEY, null)
            ?.let { stored -> ThemeMode.entries.firstOrNull { it.name == stored } }
            ?: ThemeMode.System
    }

    public fun set(mode: ThemeMode) {
        flow.value = mode
        prefs?.edit()?.putString(KEY, mode.name)?.apply()
    }
}

/**
 * The app theme. With [dark] unset it follows [ThemePreference] and the system;
 * `LakkoTheme(dark = true)` pins a subtree (the call surfaces) to the dark
 * scheme regardless of the setting.
 */
@Composable
public fun LakkoTheme(
    dark: Boolean? = null,
    content: @Composable () -> Unit,
) {
    val mode by ThemePreference.mode.collectAsStateWithLifecycle()
    val systemDark = isSystemInDarkTheme()
    val useDark = dark ?: when (mode) {
        ThemeMode.System -> systemDark
        ThemeMode.Light -> false
        ThemeMode.Dark -> true
    }
    MaterialTheme(
        colorScheme = if (useDark) LakkoDarkColorScheme else LakkoLightColorScheme,
        shapes = LakkoShapes,
        typography = MaterialTheme.typography,
        content = content,
    )
}

/** `true` when the active scheme is the dark one (for avatar tints). */
@Composable
public fun isLakkoDark(): Boolean = MaterialTheme.colorScheme.background == LakkoDark.background
