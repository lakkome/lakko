// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.components

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.Text
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.disabled
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

/** One row of a [LakkoMenu]. */
public data class LakkoMenuRow(
    val label: String,
    /** A Lakko icon drawable (`R.drawable.ic_lk_*`), or `null`. */
    @androidx.annotation.DrawableRes val icon: Int?,
    /** Rows with different sections are separated by a hairline. */
    val section: Int = 0,
    /** A disabled row says why (design system: the reason is read out). */
    val disabledReason: String? = null,
    val danger: Boolean = false,
    val testTag: String? = null,
    val onClick: () -> Unit,
)

/**
 * The design system's action sheet (`.claude/skills/ux-ui/design-system.md`,
 * Action sheet): an in-app bottom sheet of 52dp rows, icon and label, an
 * optional heading, no Cancel row (dismissing is Cancel). Never the stock
 * Material `DropdownMenu`.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
public fun LakkoMenu(
    rows: List<LakkoMenuRow>,
    onDismissRequest: () -> Unit,
    heading: String? = null,
) {
    val sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)
    val scheme = MaterialTheme.colorScheme
    ModalBottomSheet(onDismissRequest = onDismissRequest, sheetState = sheetState) {
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .verticalScroll(rememberScrollState())
                .navigationBarsPadding()
                .padding(bottom = 8.dp),
        ) {
            if (heading != null) {
                Text(
                    heading,
                    fontSize = 13.sp,
                    color = scheme.onSurfaceVariant,
                    modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp),
                )
            }
            var previousSection: Int? = null
            for (row in rows) {
                if (previousSection != null && previousSection != row.section) {
                    HorizontalDivider(color = scheme.outlineVariant)
                }
                previousSection = row.section
                val enabled = row.disabledReason == null
                val tint = when {
                    !enabled -> scheme.onSurfaceVariant.copy(alpha = 0.6f)
                    row.danger -> scheme.error
                    else -> scheme.onSurface
                }
                Row(
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(16.dp),
                    modifier = Modifier
                        .fillMaxWidth()
                        .heightIn(min = 52.dp)
                        .then(if (row.testTag != null) Modifier.testTag(row.testTag) else Modifier)
                        .semantics {
                            if (!enabled) {
                                disabled()
                                contentDescription = "${row.label}, ${row.disabledReason}"
                            }
                        }
                        .clickable(enabled = enabled, role = Role.Button) {
                            onDismissRequest()
                            row.onClick()
                        }
                        .padding(horizontal = 16.dp, vertical = 6.dp),
                ) {
                    if (row.icon != null) {
                        LkIcon(row.icon, contentDescription = null, tint = tint, size = LkIconSize.Toolbar)
                    }
                    Column {
                        Text(row.label, fontSize = 16.sp, color = tint)
                        if (row.disabledReason != null) {
                            Text(row.disabledReason, fontSize = 13.sp, color = scheme.onSurfaceVariant)
                        }
                    }
                }
            }
        }
    }
}
