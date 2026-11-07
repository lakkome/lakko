// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.contacts

import android.content.Intent
import androidx.test.core.app.ApplicationProvider
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner

/**
 * `docs/ux/desktop.md` section 7.1 — "Open in the app" on a link's page in a
 * phone browser opens an `intent:` address the web client composes
 * (`apps/web/src/platform/lakko-link.ts` `androidIntentUrl`; the strings here
 * are the ones its tests pin). Android must read it as this app's own `https`
 * link with the secret intact, and the manifest must take it.
 */
@RunWith(RobolectricTestRunner::class)
class WebIntentLinkTest {
    private val id = "AbCd_-12"
    private val secret = "0123456789abcdef"
    private val https = "https://test.lakko.me/c/$id#$secret"
    private val webIntent =
        "intent://test.lakko.me/c/$id#$secret#Intent;scheme=https;package=me.lakko.dev;" +
            "S.browser_fallback_url=https%3A%2F%2Ftest.lakko.me%2Fc%2F$id%23$secret;end"

    @Test
    fun theWebIntentIsTheHttpsLinkForThisPackage() {
        val intent = Intent.parseUri(webIntent, Intent.URI_INTENT_SCHEME)
        assertEquals(Intent.ACTION_VIEW, intent.action)
        assertEquals("me.lakko.dev", intent.`package`)
        assertEquals(https, intent.dataString)
        assertEquals(https, intent.getStringExtra("browser_fallback_url"))
        val uri = intent.data!!
        assertEquals(LakkoLink.Call(id, secret), lakkoLinkFromPath(uri.path!!, uri.fragment))
    }

    @Test
    fun aSharedFeedKeepsItsRule() {
        val intent = Intent.parseUri(
            "intent://test.lakko.me/feed/import#AbC_-9#Intent;scheme=https;package=me.lakko.dev;" +
                "S.browser_fallback_url=https%3A%2F%2Ftest.lakko.me%2Ffeed%2Fimport%23AbC_-9;end",
            Intent.URI_INTENT_SCHEME,
        )
        assertEquals("/feed/import", intent.data!!.path)
        assertEquals("AbC_-9", intent.data!!.encodedFragment)
    }

    @Test
    fun theManifestTakesIt() {
        val intent = Intent.parseUri(webIntent, Intent.URI_INTENT_SCHEME)
            .addCategory(Intent.CATEGORY_BROWSABLE)
        val context = ApplicationProvider.getApplicationContext<android.content.Context>()
        val matches = context.packageManager.queryIntentActivities(intent, 0)
        assertTrue(
            "no activity of ${context.packageName} takes $https",
            matches.any { it.activityInfo.name == "me.lakko.app.MainActivity" },
        )
    }
}
