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
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
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
import androidx.compose.ui.draw.clip
import androidx.compose.ui.geometry.Rect
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.FilterQuality
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.heading
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
import me.lakko.app.donate.DonateIcon
import me.lakko.app.donate.DonationQr

/*
 * docs/ux/donations.md section 4 — Settings > About > Support Lakko: the ways
 * to donate, each verified against the key pinned into this build. Mirror of
 * the web `routes/Donations.tsx`. Logs nothing that names a method or value.
 */

/** Dark QR modules: the light theme's text colour (`text` in Theme.kt), on
 *  a white square in both themes so wallets and banking apps scan it. */
private val QR_INK = Color(0xFF1A1D2B)
private val QR_PAPER = Color(0xFFFFFFFF)

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
                is DonationsState.Loaded -> {
                    // Section 2: Bank and card first, then Crypto; a heading
                    // for every group that has cards; the server's order
                    // inside a group.
                    val payment = s.methods.filter { it.crypto == null }
                    val crypto = s.methods.filter { it.crypto != null }
                    if (payment.isNotEmpty()) {
                        GroupHeading(t("donate.section_payment"))
                        for (m in payment) {
                            androidx.compose.runtime.key(m.id) {
                                PaymentCard(
                                    method = m,
                                    phone = phones[m.id] ?: PhoneState.Masked,
                                    loadQr = { model.qr(m) },
                                    onShowPhone = { scope.launch { model.revealPhone(m.id) } },
                                )
                            }
                        }
                    }
                    if (crypto.isNotEmpty()) {
                        GroupHeading(t("donate.section_crypto"))
                        for (m in crypto) {
                            androidx.compose.runtime.key(m.id) { m.crypto?.let { CryptoCard(it) } }
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
private fun GroupHeading(text: String) {
    Text(
        text,
        style = MaterialTheme.typography.labelLarge,
        color = MaterialTheme.colorScheme.primary,
        modifier = Modifier
            .padding(top = 8.dp)
            .semantics { heading() },
    )
}

/** The square every QR of a card is drawn in: the card's width (section 4). */
private fun Modifier.qrSquare(): Modifier = fillMaxWidth().aspectRatio(1f)

@Composable
private fun CardTitle(title: String, sub: String? = null) {
    Text(title, style = MaterialTheme.typography.titleMedium, fontWeight = FontWeight.SemiBold)
    if (sub != null) {
        Text(sub, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
    }
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
        CardTitle(methodName(method.kind))
        if (method.qrSha256 != null) {
            // The card is shown at once; the QR fills in only once its bytes
            // hash to the signed SHA-256 and decode as an image.
            val image by produceState<ImageBitmap?>(null, method.id) {
                value = loadQr()?.let { bytes ->
                    runCatching { BitmapFactory.decodeByteArray(bytes, 0, bytes.size)?.asImageBitmap() }.getOrNull()
                }
            }
            // The operator's PNG exactly as served, in colour, scaled to the
            // card's QR square (section 2.4).
            image?.let {
                Image(
                    bitmap = it,
                    contentDescription = t("common.qr_code"),
                    contentScale = ContentScale.Fit,
                    filterQuality = FilterQuality.High,
                    modifier = Modifier.qrSquare().align(Alignment.CenterHorizontally),
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
    // The dots of the mask are not read out: TalkBack says the number is
    // hidden and, once shown, reads the number itself.
    val hiddenLabel = t("donate.phone_hidden")
    SelectionContainer {
        Text(
            shown ?: MASKED_PHONE,
            fontFamily = FontFamily.Monospace,
            style = MaterialTheme.typography.bodyLarge,
            modifier = Modifier
                .fillMaxWidth()
                .semantics { if (shown == null) contentDescription = hiddenLabel },
        )
    }
    if (phone is PhoneState.Failed) {
        Text(
            t("donate.phone_error"),
            color = MaterialTheme.colorScheme.error,
            style = MaterialTheme.typography.bodySmall,
            modifier = Modifier.semantics { liveRegion = LiveRegionMode.Polite },
        )
    }
    // Show, then Copy in the same place: a full-width button like Donate.
    if (shown != null) {
        CopyButton(shown, t("donate.copy"))
    } else {
        OutlinedButton(
            onClick = onShow,
            enabled = phone !is PhoneState.Busy,
            modifier = Modifier.fillMaxWidth(),
        ) { Text(t("donate.phone_show")) }
    }
}

@Composable
private fun CryptoCard(crypto: DonateCrypto) {
    DonateCard {
        CardTitle(crypto.coin, t("donate.crypto_network", mapOf("network" to crypto.network)))
        AddressQr(
            address = crypto.address,
            icon = crypto.icon,
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
        Text(
            t("donate.crypto_warning", mapOf("coin" to crypto.coin, "network" to crypto.network)),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        // At the bottom of the card, the same full-width button as Donate.
        CopyButton(crypto.address, t("donate.crypto_copy"))
    }
}

/** The drawable of a coin mark: apps/shared/donation-marks, same geometry. */
private fun markDrawable(icon: DonateIcon): Int? = when (icon) {
    DonateIcon.NONE -> null
    DonateIcon.BTC -> R.drawable.ic_donate_mark_btc
    DonateIcon.ETH -> R.drawable.ic_donate_mark_eth
    DonateIcon.TON -> R.drawable.ic_donate_mark_ton
}

/**
 * The QR of a crypto address (section 2.4): drawn here from the verified
 * address, level H, quiet zone 4, dark modules on white, the coin mark of the
 * signed icon in the cleared centre, or a plain code for `none`. Not the
 * device-pairing branded QR: no Lakko logo.
 */
@Composable
private fun AddressQr(
    address: String,
    icon: DonateIcon,
    contentDescription: String,
    modifier: Modifier = Modifier,
) {
    val mark = markDrawable(icon)
    val plan = remember(address, mark) { DonationQr.plan(address, withMark = mark != null) } ?: return
    Box(
        modifier
            .qrSquare()
            .clip(RoundedCornerShape(12.dp))
            .background(QR_PAPER)
            .semantics { this.contentDescription = contentDescription },
    ) {
        Canvas(Modifier.fillMaxSize()) {
            val cell = size.minDimension / plan.sideModules
            val modules = Path()
            for (y in 0 until plan.sideModules) {
                for (x in 0 until plan.sideModules) {
                    if (plan.isDark(x, y)) {
                        // A hair of overlap so no seam shows between modules.
                        modules.addRect(Rect(x * cell, y * cell, (x + 1) * cell + 0.5f, (y + 1) * cell + 0.5f))
                    }
                }
            }
            drawPath(modules, QR_INK)
        }
        val hole = plan.hole
        if (mark != null && hole != null) {
            // The mark fills the cleared square less one module of margin.
            val fraction = (hole.span - 1).toFloat() / plan.sideModules
            Image(
                painter = painterResource(mark),
                contentDescription = null,
                modifier = Modifier
                    .align(Alignment.Center)
                    .fillMaxSize(fraction),
            )
        }
    }
}

/** Copy: a full-width primary button like Donate; writes to the clipboard
 *  and reads "Copied" for two seconds. */
@Composable
private fun CopyButton(text: String, label: String) {
    val context = LocalContext.current
    var copied by remember(text) { mutableStateOf(false) }
    LaunchedEffect(copied) {
        if (copied) {
            delay(COPIED_MS)
            copied = false
        }
    }
    Button(
        onClick = {
            copyToClipboard(context, text)
            copied = true
        },
        modifier = Modifier.fillMaxWidth(),
    ) { Text(if (copied) t("donate.copied") else label) }
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
