// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.call

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.selection.selectableGroup
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.RadioButton
import androidx.compose.material3.RadioButtonDefaults
import androidx.compose.material3.Text
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import me.lakko.app.calls.SettingsSection
import me.lakko.app.calls.SettingsSectionKind
import me.lakko.app.l10n.t
import me.lakko.app.ui.theme.LakkoDark

/**
 * The call's Settings sheet (`docs/ux/calls.md` section 4.1): which
 * microphone, camera and speaker the call uses.
 *
 * The app's own radio lists, never a system picker and never the system's
 * output switcher. Which sections and rows exist is decided by
 * `callSettingsSections`; this draws them. A choice applies at once and holds
 * for the call.
 *
 * It replaced a sheet of device pickers nothing fed, a "low-data mode" switch
 * nothing read and a mirror switch, all behind an overflow menu.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun CallSettingsSheet(
    sections: List<SettingsSection>,
    onChoose: (kind: SettingsSectionKind, id: String?) -> Unit,
    onDismiss: () -> Unit,
) {
    val sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)
    ModalBottomSheet(
        onDismissRequest = onDismiss,
        sheetState = sheetState,
        containerColor = LakkoDark.surface,
    ) {
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .verticalScroll(rememberScrollState())
                .padding(horizontal = 20.dp)
                .padding(bottom = 28.dp),
            verticalArrangement = Arrangement.spacedBy(6.dp),
        ) {
            Text(
                t("call.settings.title"),
                color = LakkoDark.text,
                fontSize = 20.sp,
                fontWeight = FontWeight.SemiBold,
                modifier = Modifier
                    .padding(bottom = 6.dp)
                    .semantics { heading() },
            )
            if (sections.isEmpty()) {
                Text(
                    t("call.settings.no_devices"),
                    color = LakkoDark.textMuted,
                    fontSize = 14.sp,
                )
            }
            for (section in sections) {
                Text(
                    section.title,
                    color = LakkoDark.textMuted,
                    fontSize = 13.sp,
                    fontWeight = FontWeight.SemiBold,
                    modifier = Modifier
                        .padding(top = 10.dp)
                        .semantics { heading() },
                )
                Column(modifier = Modifier.selectableGroup()) {
                    for (option in section.options) {
                        Row(
                            modifier = Modifier
                                .fillMaxWidth()
                                .heightIn(min = 48.dp)
                                .selectable(
                                    selected = option.selected,
                                    role = Role.RadioButton,
                                    onClick = { onChoose(section.kind, option.id) },
                                ),
                            verticalAlignment = Alignment.CenterVertically,
                            horizontalArrangement = Arrangement.spacedBy(12.dp),
                        ) {
                            RadioButton(
                                selected = option.selected,
                                // The row is the control; the dot only shows it.
                                onClick = null,
                                colors = RadioButtonDefaults.colors(
                                    selectedColor = LakkoDark.accent,
                                    unselectedColor = LakkoDark.textMuted,
                                ),
                            )
                            Text(option.label, color = LakkoDark.text, fontSize = 15.sp)
                        }
                    }
                }
            }
        }
    }
}
