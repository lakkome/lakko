// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.config

import me.lakko.app.BuildConfig
import me.lakko.app.auth.AuthClient
import me.lakko.app.calls.CallJwtClient
import me.lakko.app.channels.ChannelsClient
import me.lakko.app.discovery.DiscoveryClient
import me.lakko.app.ds.DsClient
import me.lakko.app.push.PushClient
import me.lakko.app.storage.StorageClient
import me.lakko.app.webauthn.WebauthnClient
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/**
 * The build-time API base URL (`LAKKO_API_BASE_URL`) every native REST
 * client derives its service base URL from.
 */
class ApiEndpointsTest {

    @Test
    fun everyServiceHangsOffTheBase() {
        val base = "https://test.lakko.me/native"
        assertEquals("https://test.lakko.me/native/auth", ApiEndpoints.resolve(base, ApiService.AUTH))
        assertEquals("https://test.lakko.me/native/ds", ApiEndpoints.resolve(base, ApiService.DS))
        assertEquals("https://test.lakko.me/native/storage", ApiEndpoints.resolve(base, ApiService.STORAGE))
        assertEquals("https://test.lakko.me/native/discovery", ApiEndpoints.resolve(base, ApiService.DISCOVERY))
        assertEquals("https://test.lakko.me/native/push-gw", ApiEndpoints.resolve(base, ApiService.PUSH_GW))
        assertEquals("https://test.lakko.me/native/channels", ApiEndpoints.resolve(base, ApiService.CHANNELS))
        assertEquals("https://test.lakko.me/native/gifs", ApiEndpoints.resolve(base, ApiService.GIFS))
    }

    @Test
    fun trailingSlashesAndWhitespaceAreTrimmed() {
        assertEquals("https://api.lakko.me/auth", ApiEndpoints.resolve("  https://api.lakko.me///\n", ApiService.AUTH))
    }

    @Test
    fun emptyOrMissingBaseFallsBackToLocalDevelopment() {
        for (empty in listOf(null, "", "   ", "/", " // ")) {
            assertNull(ApiEndpoints.normalizedBase(empty))
            assertEquals("http://10.0.2.2:8081", ApiEndpoints.resolve(empty, ApiService.AUTH))
            assertEquals("http://10.0.2.2:8082", ApiEndpoints.resolve(empty, ApiService.DS))
            assertEquals("http://10.0.2.2:8083", ApiEndpoints.resolve(empty, ApiService.STORAGE))
            assertEquals("http://10.0.2.2:8084", ApiEndpoints.resolve(empty, ApiService.DISCOVERY))
            assertEquals("http://10.0.2.2:8085", ApiEndpoints.resolve(empty, ApiService.PUSH_GW))
            assertEquals("http://10.0.2.2:8086", ApiEndpoints.resolve(empty, ApiService.CHANNELS))
            assertEquals("http://10.0.2.2:8091", ApiEndpoints.resolve(empty, ApiService.GIFS))
        }
    }

    @Test
    fun clientDefaultsFollowTheBuildConfiguration() {
        val base = BuildConfig.LAKKO_API_BASE_URL
        assertEquals(ApiEndpoints.resolve(base, ApiService.AUTH), AuthClient.DEFAULT_BASE_URL)
        assertEquals(AuthClient.DEFAULT_BASE_URL, WebauthnClient.DEFAULT_BASE_URL)
        assertEquals(AuthClient.DEFAULT_BASE_URL, CallJwtClient.DEFAULT_BASE_URL)
        assertEquals(ApiEndpoints.resolve(base, ApiService.DS), DsClient.DEFAULT_BASE_URL)
        assertEquals(ApiEndpoints.resolve(base, ApiService.STORAGE), StorageClient.DEFAULT_BASE_URL)
        assertEquals(ApiEndpoints.resolve(base, ApiService.DISCOVERY), DiscoveryClient.DEFAULT_BASE_URL)
        assertEquals(ApiEndpoints.resolve(base, ApiService.PUSH_GW), PushClient.DEFAULT_BASE_URL)
        assertEquals(ApiEndpoints.resolve(base, ApiService.CHANNELS), ChannelsClient.DEFAULT_BASE_URL)
    }
}
