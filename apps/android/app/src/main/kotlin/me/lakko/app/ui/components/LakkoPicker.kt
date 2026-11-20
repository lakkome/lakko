// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.components

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import me.lakko.app.R

/** The label [LakkoPicker] shows for [selected]: its option's text, else empty. */
internal fun <T> lakkoPickerLabel(selected: T, options: List<Pair<T, String>>): String =
    options.firstOrNull { it.first == selected }?.second ?: ""

/**
 * One value from a short list (`.claude/skills/ux-ui/design-system.md`,
 * Picker): a field-shaped button showing the current value and a chevron;
 * activating it opens an in-app bottom sheet of 52dp rows with a check mark
 * on the current value, and picking closes it. Dismissing the sheet changes
 * nothing.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
public fun <T> LakkoPicker(
    selected: T,
    options: List<Pair<T, String>>,
    onPick: (T) -> Unit,
    modifier: Modifier = Modifier,
    enabled: Boolean = true,
    heading: String? = null,
) {
    var open by remember { mutableStateOf(false) }
    val scheme = MaterialTheme.colorScheme
    Surface(
        onClick = { open = true },
        enabled = enabled,
        shape = RoundedCornerShape(8.dp),
        color = scheme.surfaceVariant,
        contentColor = if (enabled) scheme.onSurface else scheme.onSurfaceVariant,
        modifier = modifier.heightIn(min = 44.dp).widthIn(min = 120.dp),
    ) {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(8.dp),
            modifier = Modifier.padding(horizontal = 12.dp),
        ) {
            Text(
                lakkoPickerLabel(selected, options),
                fontSize = 15.sp,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.weight(1f, fill = false),
            )
            LkIcon(
                R.drawable.ic_lk_chevron_up_down,
                contentDescription = null,
                tint = scheme.onSurfaceVariant,
                size = LkIconSize.Row,
            )
        }
    }
    if (open) {
        val sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)
        ModalBottomSheet(onDismissRequest = { open = false }, sheetState = sheetState) {
            Column(
                modifier = Modifier
                    .fillMaxWidth()
                    .verticalScroll(rememberScrollState())
                    .navigationBarsPadding(),
            ) {
                if (heading != null) {
                    Text(
                        heading,
                        fontSize = 13.sp,
                        fontWeight = FontWeight.Medium,
                        color = scheme.onSurfaceVariant,
                        modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp),
                    )
                }
                for ((key, label) in options) {
                    val isCurrent = key == selected
                    Row(
                        verticalAlignment = Alignment.CenterVertically,
                        modifier = Modifier
                            .fillMaxWidth()
                            .heightIn(min = 52.dp)
                            .semantics { this.selected = isCurrent }
                            .clickable(role = Role.RadioButton) {
                                open = false
                                onPick(key)
                            }
                            .padding(horizontal = 16.dp),
                    ) {
                        Text(label, fontSize = 15.sp, modifier = Modifier.weight(1f))
                        if (isCurrent) {
                            LkIcon(
                                R.drawable.ic_lk_checkmark,
                                contentDescription = null,
                                tint = scheme.primary,
                                size = LkIconSize.Row,
                            )
                        }
                    }
                }
                Spacer(Modifier.height(8.dp))
            }
        }
    }
}
