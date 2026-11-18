// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.donate

import android.content.ActivityNotFoundException
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import android.graphics.BitmapFactory
import android.net.Uri
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import me.lakko.app.R
import me.lakko.app.donate.DonateCrypto
import me.lakko.app.donate.DonateKind
import me.lakko.app.donate.DonateMethod
import me.lakko.app.donate.DonationsClient
import me.lakko.app.donate.DonationsModel
import me.lakko.app.donate.DonationsSource
import me.lakko.app.donate.DonationsState
import me.lakko.app.donate.MASKED_PHONE
import me.lakko.app.donate.PhoneState
import me.lakko.app.l10n.t
import me.lakko.app.ui.components.LkIcon
import me.lakko.app.ui.devices.BrandedQrImage

/*
 * docs/ux/donations.md section 4 — Settings > About > Support Lakko: the ways
 * to donate, each verified against the key pinned into this build. Mirror of
 * the web `routes/Donations.tsx`. Logs nothing that names a method or value.
 */

private val QR_SIDE = 240.dp

/** The brand names of the three payment kinds; never translated. */
private fun methodName(kind: DonateKind): String = when (kind) {
    DonateKind.SBP -> "SBP"
    DonateKind.REVOLUT -> "Revolut"
    DonateKind.BMC -> "Buy Me a Coffee"
    DonateKind.CRYPTO -> ""
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
public fun DonationsScreen(
    onBack: () -> Unit,
    modifier: Modifier = Modifier,
    /** Tests pass a fake; production reads the three discovery routes. */
    source: (() -> DonationsSource)? = null,
) {
    val built = remember { source?.invoke() ?: DonationsClient() }
    androidx.compose.runtime.DisposableEffect(built) {
        onDispose { (built as? AutoCloseable)?.close() }
    }
    val model = remember(built) { DonationsModel(built) }
    val state by model.state.collectAsStateWithLifecycle()
    val phones by model.phones.collectAsStateWithLifecycle()
    val scope = rememberCoroutineScope()
    LaunchedEffect(model) { model.load() }

    Scaffold(
        modifier = modifier,
        topBar = {
            TopAppBar(
                title = { Text(t("donate.title")) },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        LkIcon(R.drawable.ic_lk_chevron_back, contentDescription = t("common.back"))
                    }
                },
            )
        },
    ) { padding ->
        Column(
            modifier = Modifier
                .padding(padding)
                .fillMaxSize()
                .verticalScroll(rememberScrollState())
                .padding(horizontal = 16.dp, vertical = 12.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Text(t("donate.lede"), style = MaterialTheme.typography.bodyLarge)
            when (val s = state) {
                DonationsState.Loading -> {
                    PlaceholderCard()
                    PlaceholderCard()
                }
                DonationsState.Error -> {
                    Text(
                        t("donate.error"),
                        color = MaterialTheme.colorScheme.error,
                        modifier = Modifier.semantics { liveRegion = LiveRegionMode.Polite },
                    )
                    Button(onClick = { scope.launch { model.load() } }) { Text(t("common.try_again")) }
                }
                DonationsState.None -> Text(t("donate.none"))
                is DonationsState.Loaded -> for (m in s.methods) {
                    androidx.compose.runtime.key(m.id) {
                        val crypto = m.crypto
                        if (crypto != null) {
                            CryptoCard(crypto)
                        } else {
                            PaymentCard(
                                method = m,
                                phone = phones[m.id] ?: PhoneState.Masked,
                                loadQr = { model.qr(m) },
                                onShowPhone = { scope.launch { model.revealPhone(m.id) } },
                            )
                        }
                    }
                }
            }
        }
    }
}

@Composable
private fun DonateCard(content: @Composable ColumnScope.() -> Unit) {
    Card(
        modifier = Modifier.fillMaxWidth(),
        colors = CardDefaults.cardColors(containerColor = MaterialTheme.colorScheme.surface),
        border = androidx.compose.foundation.BorderStroke(1.dp, MaterialTheme.colorScheme.outlineVariant),
    ) {
        Column(
            Modifier.padding(20.dp).fillMaxWidth(),
            verticalArrangement = Arrangement.spacedBy(10.dp),
        ) { content() }
    }
}

@Composable
private fun PlaceholderCard() {
    Box(
        Modifier
            .fillMaxWidth()
            .height(160.dp)
            .background(MaterialTheme.colorScheme.surfaceVariant, RoundedCornerShape(12.dp)),
    )
}

@Composable
private fun CardTitle(title: String, sub: String) {
    Text(title, style = MaterialTheme.typography.titleMedium, fontWeight = FontWeight.SemiBold)
    Text(sub, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
}

@Composable
private fun PaymentCard(
    method: DonateMethod,
    phone: PhoneState,
    loadQr: suspend () -> ByteArray?,
    onShowPhone: () -> Unit,
) {
    val context = LocalContext.current
    DonateCard {
        CardTitle(
            methodName(method.kind),
            if (method.kind == DonateKind.SBP) t("donate.for_russia") else t("donate.worldwide"),
        )
        if (method.qrSha256 != null) {
            // The card is shown at once; the QR fills in only once its bytes
            // hash to the signed SHA-256 and decode as an image.
            val image by produceState<ImageBitmap?>(null, method.id) {
                value = loadQr()?.let { bytes ->
                    runCatching { BitmapFactory.decodeByteArray(bytes, 0, bytes.size)?.asImageBitmap() }.getOrNull()
                }
            }
            image?.let {
                Image(
                    bitmap = it,
                    contentDescription = t("common.qr_code"),
                    modifier = Modifier.size(QR_SIDE).align(Alignment.CenterHorizontally),
                )
            }
        }
        method.link?.let { link ->
            Button(onClick = { openPayment(context, link) }, modifier = Modifier.fillMaxWidth()) {
                Text(t("donate.button"))
            }
        }
        if (method.hasPhone) PhoneRow(phone, onShowPhone)
    }
}

@Composable
private fun PhoneRow(phone: PhoneState, onShow: () -> Unit) {
    val shown = (phone as? PhoneState.Shown)?.number
    Row(verticalAlignment = Alignment.CenterVertically) {
        // The dots of the mask are not read out: TalkBack says the number is
        // hidden and, once shown, reads the number itself.
        val hiddenLabel = t("donate.phone_hidden")
        Text(
            shown ?: MASKED_PHONE,
            fontFamily = FontFamily.Monospace,
            modifier = Modifier
                .weight(1f)
                .semantics { if (shown == null) contentDescription = hiddenLabel },
        )
        if (shown != null) {
            CopyButton(shown, t("donate.copy"))
        } else {
            TextButton(onClick = onShow, enabled = phone !is PhoneState.Busy) { Text(t("donate.phone_show")) }
        }
    }
    if (phone is PhoneState.Failed) {
        Text(
            t("donate.phone_error"),
            color = MaterialTheme.colorScheme.error,
            style = MaterialTheme.typography.bodySmall,
            modifier = Modifier.semantics { liveRegion = LiveRegionMode.Polite },
        )
    }
}

@Composable
private fun CryptoCard(crypto: DonateCrypto) {
    DonateCard {
        CardTitle(crypto.coin, t("donate.crypto_network", mapOf("network" to crypto.network)))
        BrandedQrImage(
            text = crypto.address,
            size = QR_SIDE,
            contentDescription = t("common.qr_code"),
            modifier = Modifier.align(Alignment.CenterHorizontally),
        )
        SelectionContainer {
            Text(
                crypto.address,
                fontFamily = FontFamily.Monospace,
                style = MaterialTheme.typography.bodyMedium,
                modifier = Modifier
                    .fillMaxWidth()
                    .background(MaterialTheme.colorScheme.surfaceVariant, RoundedCornerShape(8.dp))
                    .padding(10.dp),
            )
        }
        CopyButton(crypto.address, t("donate.crypto_copy"), outlined = true)
        Text(
            t("donate.crypto_warning", mapOf("coin" to crypto.coin, "network" to crypto.network)),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.widthIn(max = 560.dp),
        )
    }
}

/** Copy: writes to the clipboard and reads "Copied" for two seconds. */
@Composable
private fun CopyButton(text: String, label: String, outlined: Boolean = false) {
    val context = LocalContext.current
    var copied by remember(text) { mutableStateOf(false) }
    LaunchedEffect(copied) {
        if (copied) {
            delay(COPIED_MS)
            copied = false
        }
    }
    val onClick = {
        copyToClipboard(context, text)
        copied = true
    }
    val shown = if (copied) t("donate.copied") else label
    if (outlined) {
        OutlinedButton(onClick = onClick) { Text(shown) }
    } else {
        TextButton(onClick = onClick) { Text(shown) }
    }
}

private const val COPIED_MS = 2_000L

private fun copyToClipboard(context: Context, text: String) {
    val clipboard = context.getSystemService(Context.CLIPBOARD_SERVICE) as? ClipboardManager ?: return
    clipboard.setPrimaryClip(ClipData.newPlainText(me.lakko.app.l10n.Localization.strings.value["donate.title"], text))
}

/** Donate: the system browser, `https://` only (checked again here). */
private fun openPayment(context: Context, link: String) {
    if (!link.startsWith("https://")) return
    val intent = Intent(Intent.ACTION_VIEW, Uri.parse(link)).addCategory(Intent.CATEGORY_BROWSABLE)
    try {
        context.startActivity(intent)
    } catch (_: ActivityNotFoundException) {
        // No browser on the device; the button does nothing.
    }
}
