// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import android.Manifest
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.location.Location
import android.location.LocationListener
import android.location.LocationManager
import android.net.Uri
import android.os.Looper
import android.provider.Settings
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.core.content.ContextCompat
import kotlinx.coroutines.delay
import me.lakko.app.l10n.t
import uniffi.lakko_ffi_uniffi.SharedLocation

// ---------------------------------------------------------------- location

/** UX 9.4 — a fix older than this or worse than [WEAK_ACCURACY_M] is shown in warning colour. */
internal const val STALE_FIX_MS: Long = 120_000L
internal const val WEAK_ACCURACY_M: Float = 1_000f
internal const val NO_FIX_TIMEOUT_MS: Long = 30_000L

/** UX 9.2 — the coordinates in the person's locale, `52.37022° N, 4.89517° E`. */
internal fun formatCoordinates(latE7: Int, lonE7: Int, locale: java.util.Locale = java.util.Locale.getDefault()): String {
    val lat = latE7 / 1e7
    val lon = lonE7 / 1e7
    val ns = if (lat >= 0) "N" else "S"
    val ew = if (lon >= 0) "E" else "W"
    return String.format(locale, "%.5f° %s, %.5f° %s", kotlin.math.abs(lat), ns, kotlin.math.abs(lon), ew)
}

/** `Accurate to 12 m` / `1.4 km`, formatted for the locale. */
internal fun formatDistance(meters: Long, locale: java.util.Locale = java.util.Locale.getDefault()): String =
    if (meters < 1000) "$meters m" else String.format(locale, "%.1f km", meters / 1000.0)

/** A platform fix as the wire carries it (1e-7 fixed point). */
internal fun sharedLocationOf(loc: Location, name: String?): SharedLocation = SharedLocation(
    latE7 = Math.round(loc.latitude * 1e7).toInt(),
    lonE7 = Math.round(loc.longitude * 1e7).toInt(),
    accuracyM = loc.accuracy.coerceAtLeast(0f).toLong().coerceIn(0, Int.MAX_VALUE.toLong()).toUInt(),
    fixAtMs = loc.time,
    name = name?.trim()?.takeIf { it.isNotEmpty() },
)

/** UX 9.3 — the `geo:` link a tap on a location opens (Android row). */
internal fun geoUriOf(loc: SharedLocation): String {
    val lat = "%.7f".format(java.util.Locale.ROOT, loc.latE7 / 1e7)
    val lon = "%.7f".format(java.util.Locale.ROOT, loc.lonE7 / 1e7)
    val label = loc.name?.let { "(" + Uri.encode(it) + ")" } ?: ""
    return "geo:$lat,$lon?q=$lat,$lon$label"
}

private fun hasLocationPermission(context: Context): Boolean =
    ContextCompat.checkSelfPermission(context, Manifest.permission.ACCESS_FINE_LOCATION) ==
        PackageManager.PERMISSION_GRANTED ||
        ContextCompat.checkSelfPermission(context, Manifest.permission.ACCESS_COARSE_LOCATION) ==
        PackageManager.PERMISSION_GRANTED

/**
 * UX 9 — Lakko's location screen. Nothing leaves the device before Send; the
 * fix comes from the platform `LocationManager` (no Play services), updated
 * while the screen is open. No map.
 */
@Composable
internal fun LocationScreen(onSend: (SharedLocation) -> Unit, onClose: () -> Unit) {
    val context = LocalContext.current
    var permitted by remember { mutableStateOf(hasLocationPermission(context)) }
    var denied by remember { mutableStateOf(false) }
    var fix by remember { mutableStateOf<Location?>(null) }
    var name by remember { mutableStateOf("") }
    var attempt by remember { mutableIntStateOf(0) }
    var timedOut by remember { mutableStateOf(false) }
    val launcher = rememberLauncherForActivityResult(ActivityResultContracts.RequestMultiplePermissions()) { r ->
        permitted = r.values.any { it }
        denied = !permitted
    }
    LaunchedEffect(Unit) {
        if (!permitted) {
            launcher.launch(arrayOf(Manifest.permission.ACCESS_FINE_LOCATION, Manifest.permission.ACCESS_COARSE_LOCATION))
        }
    }
    if (permitted) {
        DisposableEffect(attempt) {
            val manager = context.getSystemService(Context.LOCATION_SERVICE) as? LocationManager
            val listener = LocationListener { loc -> if (fix == null || loc.accuracy <= (fix?.accuracy ?: Float.MAX_VALUE) || loc.time > (fix?.time ?: 0)) fix = loc }
            try {
                manager?.allProviders?.filter { it != LocationManager.PASSIVE_PROVIDER }?.forEach { provider ->
                    manager.getLastKnownLocation(provider)?.let { if (fix == null) fix = it }
                    manager.requestLocationUpdates(provider, 2_000L, 0f, listener, Looper.getMainLooper())
                }
            } catch (_: SecurityException) {
                denied = true
            } catch (_: Throwable) {
                // A provider that is off leaves the wait running; the timeout says so.
            }
            onDispose { runCatching { manager?.removeUpdates(listener) } }
        }
        LaunchedEffect(attempt) {
            timedOut = false
            delay(NO_FIX_TIMEOUT_MS)
            if (fix == null) timedOut = true
        }
    }
    val current = fix
    ContentEditorScaffold(
        title = t("location.title"),
        actionLabel = t("location.send"),
        actionEnabled = permitted && current != null,
        onAction = { current?.let { onSend(sharedLocationOf(it, name)) } },
        onClose = onClose,
    ) {
        Box(Modifier.fillMaxWidth().padding(vertical = 24.dp), contentAlignment = Alignment.Center) {
            me.lakko.app.ui.components.LkIcon(
                me.lakko.app.R.drawable.ic_lk_location,
                contentDescription = null,
                tint = MaterialTheme.colorScheme.primary,
                modifier = Modifier.size(72.dp),
            )
        }
        when {
            denied || !permitted -> {
                Text(t("location.denied"), fontSize = 15.sp)
                OutlinedButton(onClick = {
                    context.startActivity(
                        Intent(Settings.ACTION_APPLICATION_DETAILS_SETTINGS, Uri.fromParts("package", context.packageName, null))
                            .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK),
                    )
                }) { Text(t("chat.record.open_settings")) }
            }
            current == null && timedOut -> {
                Text(t("location.not_found"), fontSize = 15.sp)
                OutlinedButton(onClick = { attempt += 1 }) { Text(t("common.try_again")) }
            }
            current == null -> Text(t("location.finding"), fontSize = 15.sp)
            else -> {
                val shared = sharedLocationOf(current, null)
                Text(formatCoordinates(shared.latE7, shared.lonE7), fontSize = 17.sp)
                val weak = current.accuracy > WEAK_ACCURACY_M ||
                    System.currentTimeMillis() - current.time > STALE_FIX_MS
                val approximate = ContextCompat.checkSelfPermission(context, Manifest.permission.ACCESS_FINE_LOCATION) !=
                    PackageManager.PERMISSION_GRANTED
                Text(
                    if (approximate) {
                        t("location.approximate")
                    } else {
                        t("location.accuracy", mapOf("distance" to formatDistance(current.accuracy.toLong())))
                    },
                    fontSize = 13.sp,
                    color = if (weak) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
        OutlinedTextField(
            value = name,
            onValueChange = { name = it.take(100) },
            label = { Text(t("location.name_place")) },
            singleLine = true,
            modifier = Modifier.fillMaxWidth(),
        )
    }
}

// ---------------------------------------------------------------- contact

/** One contact the picker offers (UX 10.1). */
internal data class ContactPickRow(
    val name: String,
    val username: String?,
    val accountKey: ByteArray,
    /** ADR-0092 section 7 — `false` when the newest profile this device holds
     *  from the contact says their card may not be shared. */
    val shareable: Boolean = true,
) {
    /** Whether the row can be picked: a username to share, and the person
     *  lets others share their card. */
    val pickable: Boolean get() = shareable && !username.isNullOrEmpty()
}

/**
 * The picker rows for [accepted] (UX 10.1). The username and the sharing flag
 * come from the NEWEST profile this device holds from the contact
 * ([storedProfile], keyed by the signature key in standard base64), not the
 * profile their contact request carried; with nothing stored, the request's
 * profile is the newest there is. A person who never sent a profile counts as
 * shareable.
 */
internal fun contactPickRows(
    accepted: List<me.lakko.app.contacts.AcceptedContact>,
    storedProfile: (String) -> me.lakko.app.profile.PeerProfile?,
): List<ContactPickRow> = accepted.map { c ->
    val keyB64 = java.util.Base64.getEncoder().encodeToString(c.request.fromPubkey)
    val stored = storedProfile(keyB64)
    val request = c.request.profile
    val username = if (stored != null) stored.username else request?.username
    val name = c.displayName ?: stored?.displayName ?: request?.displayName ?: username ?: ""
    ContactPickRow(
        name = name,
        username = username?.takeIf { it.isNotEmpty() },
        accountKey = c.request.fromPubkey,
        shareable = stored?.cardShareable ?: true,
    )
}

/** UX 10.1 — the contact picker: the contacts, searchable, one choice. */
@Composable
internal fun ContactPickerScreen(
    contacts: List<ContactPickRow>,
    onSend: (ContactPickRow) -> Unit,
    onClose: () -> Unit,
) {
    var query by remember { mutableStateOf("") }
    var picked by remember { mutableStateOf<ContactPickRow?>(null) }
    val shown = contacts.filter {
        query.isBlank() || it.name.contains(query, ignoreCase = true) ||
            (it.username?.contains(query.removePrefix("@"), ignoreCase = true) == true)
    }
    ContentEditorScaffold(
        title = t("contact.picker.title"),
        actionLabel = t("contact.send"),
        actionEnabled = picked != null,
        onAction = { picked?.let(onSend) },
        onClose = onClose,
    ) {
        OutlinedTextField(
            value = query,
            onValueChange = { query = it },
            placeholder = { Text(t("contact.picker.search")) },
            singleLine = true,
            modifier = Modifier.fillMaxWidth(),
        )
        for (c in shown) {
            val enabled = c.pickable
            val selected = picked === c
            Row(
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(12.dp),
                modifier = Modifier
                    .fillMaxWidth()
                    .heightIn(min = 56.dp)
                    .clickable(enabled = enabled) { picked = c },
            ) {
                me.lakko.app.ui.components.LakkoAvatar(name = c.name, size = 40.dp)
                Column(Modifier.weight(1f)) {
                    Text(
                        c.name,
                        fontSize = 15.sp,
                        color = if (enabled) MaterialTheme.colorScheme.onSurface else MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                    Text(
                        when {
                            !c.shareable -> t("contact.not_shareable", mapOf("name" to c.name))
                            enabled -> "@${c.username}"
                            else -> t("contact.no_username")
                        },
                        fontSize = 13.sp,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
                if (selected) Text("✓", color = MaterialTheme.colorScheme.primary, fontSize = 18.sp)
            }
        }
    }
}
