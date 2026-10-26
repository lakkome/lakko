// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.contacts

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.FilterChip
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import me.lakko.app.l10n.LocalStrings
import me.lakko.app.l10n.t

/**
 * The Invite links sheet's "Create link" form (the web `InviteLinksPanel`
 * form): Label, Expires, Uses, then Create and Cancel. Choices are Lakko
 * chips inside the sheet, never a system dialog. The validation and the
 * mapping to the repository's parameters live in [InviteCreateForm].
 */
@OptIn(ExperimentalLayoutApi::class)
@Composable
internal fun InviteCreateFormView(
    form: InviteCreateForm,
    onChange: (InviteCreateForm) -> Unit,
    busy: Boolean,
    onCreate: (InviteCreateParams) -> Unit,
    onCancel: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val language = LocalStrings.current.language
    val maxUsesText = remember(language) {
        java.text.NumberFormat.getIntegerInstance(java.util.Locale.forLanguageTag(language))
            .format(INVITE_CUSTOM_MAX_USES)
    }
    val params = form.toParams()
    Column(
        modifier = modifier
            .fillMaxWidth()
            .verticalScroll(rememberScrollState()),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        OutlinedTextField(
            value = form.label,
            onValueChange = { onChange(form.copy(label = clampInviteLabel(it))) },
            label = { Text(t("invite.links.label_field")) },
            placeholder = { Text(t("invite.links.label_placeholder")) },
            singleLine = true,
            enabled = !busy,
            modifier = Modifier.fillMaxWidth(),
        )

        FieldTitle(t("invite.links.expires_field"))
        FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            Choice(form.expiry == InviteExpiryChoice.OneDay, days(1), busy) {
                onChange(form.copy(expiry = InviteExpiryChoice.OneDay))
            }
            Choice(form.expiry == InviteExpiryChoice.SevenDays, days(7), busy) {
                onChange(form.copy(expiry = InviteExpiryChoice.SevenDays))
            }
            Choice(form.expiry == InviteExpiryChoice.ThirtyDays, days(30), busy) {
                onChange(form.copy(expiry = InviteExpiryChoice.ThirtyDays))
            }
            Choice(form.expiry == InviteExpiryChoice.Never, t("invite.links.expires_option_never"), busy) {
                onChange(form.copy(expiry = InviteExpiryChoice.Never))
            }
            Choice(form.expiry == InviteExpiryChoice.Custom, t("invite.links.expires_option_custom"), busy) {
                onChange(form.copy(expiry = InviteExpiryChoice.Custom))
            }
        }
        if (form.expiry == InviteExpiryChoice.Custom) {
            NumberField(
                value = form.customExpiryAmount,
                label = t("invite.links.custom_expiry_amount"),
                enabled = !busy,
                isError = form.showsCustomExpiryError(),
                onValueChange = { onChange(form.copy(customExpiryAmount = it)) },
            )
            FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Choice(form.customExpiryUnit == InviteExpiryUnit.Hours, t("invite.links.unit_hours"), busy) {
                    onChange(form.copy(customExpiryUnit = InviteExpiryUnit.Hours))
                }
                Choice(form.customExpiryUnit == InviteExpiryUnit.Days, t("invite.links.unit_days"), busy) {
                    onChange(form.copy(customExpiryUnit = InviteExpiryUnit.Days))
                }
            }
            if (form.showsCustomExpiryError()) ErrorLine(t("invite.links.error.custom_expiry"))
        }

        FieldTitle(t("invite.links.uses_field"))
        FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            Choice(form.uses == InviteUsesChoice.One, t("invite.links.uses_option_one"), busy) {
                onChange(form.copy(uses = InviteUsesChoice.One))
            }
            Choice(
                form.uses == InviteUsesChoice.UpToTen,
                t("invite.links.uses_option_up_to", emptyMap(), mapOf("count" to 10uL)),
                busy,
            ) {
                onChange(form.copy(uses = InviteUsesChoice.UpToTen))
            }
            Choice(form.uses == InviteUsesChoice.Unlimited, t("invite.links.uses_option_unlimited"), busy) {
                onChange(form.copy(uses = InviteUsesChoice.Unlimited))
            }
            Choice(form.uses == InviteUsesChoice.Custom, t("invite.links.uses_option_custom"), busy) {
                onChange(form.copy(uses = InviteUsesChoice.Custom))
            }
        }
        if (form.uses == InviteUsesChoice.Custom) {
            NumberField(
                value = form.customUses,
                label = t("invite.links.custom_uses_count"),
                enabled = !busy,
                isError = form.showsCustomUsesError(),
                onValueChange = { onChange(form.copy(customUses = it)) },
            )
            if (form.showsCustomUsesError()) {
                ErrorLine(t("invite.links.error.custom_uses", mapOf("max" to maxUsesText)))
            }
        }

        Button(
            onClick = { params?.let(onCreate) },
            enabled = params != null && !busy,
            modifier = Modifier.fillMaxWidth(),
        ) {
            if (busy) {
                CircularProgressIndicator(
                    modifier = Modifier.size(20.dp),
                    strokeWidth = 2.dp,
                    color = MaterialTheme.colorScheme.onPrimary,
                )
                Spacer(Modifier.width(8.dp))
                Text(t("common.creating"))
            } else {
                Text(t("invite.links.create"))
            }
        }
        TextButton(onClick = onCancel, enabled = !busy, modifier = Modifier.fillMaxWidth()) {
            Text(t("common.cancel"))
        }
    }
}

@Composable
private fun days(count: Int): String =
    t("invite.links.expires_option_days", emptyMap(), mapOf("count" to count.toULong()))

@Composable
private fun FieldTitle(text: String) {
    Text(text, style = MaterialTheme.typography.labelLarge)
}

@Composable
private fun ErrorLine(text: String) {
    Text(text, color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.bodySmall)
}

@Composable
private fun Choice(selected: Boolean, label: String, busy: Boolean, onClick: () -> Unit) {
    FilterChip(
        selected = selected,
        onClick = onClick,
        enabled = !busy,
        label = { Text(label) },
    )
}

@Composable
private fun NumberField(
    value: String,
    label: String,
    enabled: Boolean,
    isError: Boolean,
    onValueChange: (String) -> Unit,
) {
    OutlinedTextField(
        value = value,
        // Digits only, and never longer than the parser reads.
        onValueChange = { onValueChange(it.filter { c -> c in '0'..'9' }.take(9)) },
        label = { Text(label) },
        singleLine = true,
        enabled = enabled,
        isError = isError,
        keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number),
        modifier = Modifier.fillMaxWidth(),
    )
}
