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
import androidx.compose.foundation.selection.toggleable
import androidx.compose.foundation.verticalScroll
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.OutlinedTextFieldDefaults
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.RadioButton
import androidx.compose.material3.RadioButtonDefaults
import androidx.compose.material3.Switch
import androidx.compose.material3.SwitchDefaults
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
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import me.lakko.app.calls.CallNameControl
import me.lakko.app.calls.SettingsSection
import me.lakko.app.calls.SettingsSectionKind
import me.lakko.app.l10n.LocalStrings
import me.lakko.app.l10n.t
import me.lakko.app.l10n.text
import me.lakko.app.profile.DisplayNameCheck
import me.lakko.app.profile.checkDisplayName
import me.lakko.app.ui.theme.LakkoDark

/**
 * The call's Settings sheet (`docs/ux/calls.md` section 4.1): which
 * microphone, camera and speaker the call uses.
 *
 * At the top, on a group or link call only, `Your name in this call`
 * (section 4.1, `calls/CallName.kt`): the name everyone in the call sees on
 * this person's tile; the profile name does not change.
 *
 * Above the device lists, the `Sound` section with the `Noise suppression`
 * switch (section 4.1.3, ADR-0088): shown even when no device list is.
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
    noiseSuppressionOn: Boolean,
    onNoiseSuppressionChange: (Boolean) -> Unit,
    /** Saves the name typed for the call; `null` on a call that cannot carry
     *  one (a one-to-one call), which shows no name field. */
    callNameControl: CallNameControl? = null,
    /** The name already set for the call (`""` = none). */
    ownCallName: String = "",
    /** The profile name, the field's placeholder (`""` = none). */
    callNameDefault: String = "",
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
            if (callNameControl != null) {
                CallNameSection(callNameControl, ownCallName, callNameDefault)
            }
            NoiseSuppressionSection(noiseSuppressionOn, onNoiseSuppressionChange)
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

/** `Sound`: the noise suppression switch with its hint (UX 4.1.3). The whole
 *  row is the control; its spoken name is the label, its state on / off. */
@Composable
private fun NoiseSuppressionSection(on: Boolean, onChange: (Boolean) -> Unit) {
    Text(
        t("call.settings.sound"),
        color = LakkoDark.textMuted,
        fontSize = 13.sp,
        fontWeight = FontWeight.SemiBold,
        modifier = Modifier
            .padding(top = 10.dp)
            .semantics { heading() },
    )
    val label = t("call.settings.noise_suppression")
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .heightIn(min = 48.dp)
            .toggleable(value = on, role = Role.Switch, onValueChange = onChange)
            .semantics { contentDescription = label },
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Column(
            modifier = Modifier
                .weight(1f)
                .clearAndSetSemantics { },
            verticalArrangement = Arrangement.spacedBy(2.dp),
        ) {
            Text(label, color = LakkoDark.text, fontSize = 15.sp)
            Text(
                t("call.settings.noise_suppression_hint"),
                color = LakkoDark.textMuted,
                fontSize = 13.sp,
            )
        }
        Switch(
            checked = on,
            // The row is the control; the switch only shows it.
            onCheckedChange = null,
            colors = SwitchDefaults.colors(
                checkedTrackColor = LakkoDark.accent,
                uncheckedThumbColor = LakkoDark.textMuted,
            ),
        )
    }
}

/**
 * `Your name in this call` (section 4.1): one field and `Save`, enabled only
 * when the typed name passes the display-name rule and differs from the name
 * already set. An empty field goes back to the profile name. Saved at once;
 * a screen reader hears the confirmation. The app's own controls, no system
 * alert.
 */
@Composable
private fun CallNameSection(control: CallNameControl, ownCallName: String, defaultName: String) {
    var typed by remember { mutableStateOf(ownCallName) }
    var spoken by remember { mutableStateOf("") }
    val strings = LocalStrings.current
    val check = remember(typed) { checkDisplayName(typed) }
    // The name to save: the trimmed text ("" for an empty field), or `null`
    // while the text fails the rule.
    val normalised: String? = (check as? DisplayNameCheck.Valid)?.let { it.normalized.orEmpty() }
    val canSave = normalised != null && normalised != ownCallName
    val save = {
        if (normalised != null && normalised != ownCallName) {
            control.save(normalised)
            typed = normalised
            spoken = if (normalised.isEmpty()) {
                strings["call.settings.call_name.a11y_cleared"]
            } else {
                strings.format("call.settings.call_name.a11y_saved", mapOf("name" to normalised))
            }
        }
    }
    val label = t("call.settings.call_name.label")
    Text(
        label,
        color = LakkoDark.textMuted,
        fontSize = 13.sp,
        fontWeight = FontWeight.SemiBold,
        modifier = Modifier
            .padding(top = 10.dp)
            .semantics { heading() },
    )
    Row(
        modifier = Modifier.fillMaxWidth(),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        OutlinedTextField(
            value = typed,
            onValueChange = { typed = it },
            placeholder = if (defaultName.isNotEmpty()) {
                { Text(defaultName, color = LakkoDark.textMuted) }
            } else {
                null
            },
            singleLine = true,
            isError = check is DisplayNameCheck.Invalid,
            keyboardOptions = KeyboardOptions(imeAction = ImeAction.Done),
            keyboardActions = KeyboardActions(onDone = { save() }),
            colors = OutlinedTextFieldDefaults.colors(
                focusedTextColor = LakkoDark.text,
                unfocusedTextColor = LakkoDark.text,
                focusedBorderColor = LakkoDark.accent,
                unfocusedBorderColor = LakkoDark.border,
                cursorColor = LakkoDark.accent,
                errorBorderColor = LakkoDark.danger,
            ),
            modifier = Modifier
                .weight(1f)
                .semantics { contentDescription = label },
        )
        Button(
            onClick = { save() },
            enabled = canSave,
            colors = ButtonDefaults.buttonColors(
                containerColor = LakkoDark.accent,
                contentColor = LakkoDark.accentFg,
            ),
            modifier = Modifier.heightIn(min = 48.dp),
        ) {
            Text(t("call.settings.call_name.save"))
        }
    }
    (check as? DisplayNameCheck.Invalid)?.let { invalid ->
        Text(invalid.problem.text.text(), color = LakkoDark.danger, fontSize = 13.sp)
    }
    Text(t("call.settings.call_name.hint"), color = LakkoDark.textMuted, fontSize = 13.sp)
    // The spoken confirmation: a polite live region that is not drawn.
    Box(
        modifier = Modifier
            .size(1.dp)
            .semantics {
                liveRegion = LiveRegionMode.Polite
                contentDescription = spoken
            },
    )
}
