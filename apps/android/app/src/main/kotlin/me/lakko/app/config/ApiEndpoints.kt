// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.config

import me.lakko.app.BuildConfig

/**
 * A Lakko HTTP service a native client talks to.
 *
 * [pathSegment] is the path the service lives under on the native API host.
 * The server exposes the same prefix-less shape in two places:
 *
 *   * production: `https://api.lakko.me/<segment>/...`
 *   * test stack: `https://test.lakko.me/native/<segment>/...`
 *
 * so one API base URL per build selects the whole deployment.
 *
 * [localDevBaseUrl] is the base used when the build carries no API base URL:
 * the service's local development bind behind the Android emulator's
 * host-loopback alias `10.0.2.2`. These are the values every client hardcoded
 * before the API base URL existed, so local development is unchanged.
 */
public enum class ApiService(public val pathSegment: String, public val localDevBaseUrl: String) {
    AUTH("auth", "http://10.0.2.2:8081"),
    DS("ds", "http://10.0.2.2:8082"),
    STORAGE("storage", "http://10.0.2.2:8083"),
    DISCOVERY("discovery", "http://10.0.2.2:8084"),
    PUSH_GW("push-gw", "http://10.0.2.2:8085"),
    CHANNELS("channels", "http://10.0.2.2:8086"),
    GIFS("gifs", "http://10.0.2.2:8091"),
    /** ADR-0098: reports to Lakko's moderators. */
    MODERATION("moderation", "http://10.0.2.2:8100"),
}

/**
 * The one build-time API base URL every native REST client derives its
 * service base URL from.
 *
 * Read from [BuildConfig.LAKKO_API_BASE_URL], which `app/build.gradle.kts`
 * fills per build type from the gradle property `lakko.api.url` (debug
 * defaults to the test stack, release to empty). Switching deployments is a
 * build flag, never a code change.
 *
 * When the value is empty every service falls back to its local development
 * bind ([ApiService.localDevBaseUrl]). A release build without a base URL
 * logs that once, so a misconfigured release is visible in logcat instead of
 * silently dialling the emulator alias.
 */
public object ApiEndpoints {

    /** The configured API base URL, normalised, or `null` when unset. */
    public val configuredApiBaseUrl: String? by lazy {
        val base = normalizedBase(BuildConfig.LAKKO_API_BASE_URL)
        if (base == null && !BuildConfig.DEBUG) {
            android.util.Log.w(
                "lakko-api",
                "LAKKO_API_BASE_URL is empty in a release build; every service " +
                    "falls back to its 10.0.2.2 development address",
            )
        }
        base
    }

    /** The base URL (no trailing slash) for [service] in this build. */
    public fun baseUrlFor(service: ApiService): String = resolve(configuredApiBaseUrl, service)

    /**
     * The web origin this build shares its links under (`/i/`, `/g/`, `/c/`),
     * read from [BuildConfig.LAKKO_WEB_ORIGIN]. It must be the host the same
     * build's servers live behind: a link minted here is opened by people on
     * that stack's web client and by phones whose app claims that host, so a
     * debug build against the test stack mints `https://test.lakko.me/...`,
     * never the production host. Empty -> the production origin.
     */
    public val webOrigin: String by lazy {
        normalizedBase(BuildConfig.LAKKO_WEB_ORIGIN) ?: PRODUCTION_WEB_ORIGIN
    }

    /** The production web origin, the fallback when a build sets none. */
    public const val PRODUCTION_WEB_ORIGIN: String = "https://lakko.me"

    /**
     * Pure resolution: `<apiBase>/<segment>` when [apiBase] is non-empty after
     * trimming whitespace and trailing slashes, else the service's local
     * development base URL.
     */
    public fun resolve(apiBase: String?, service: ApiService): String {
        val base = normalizedBase(apiBase) ?: return service.localDevBaseUrl
        return "$base/${service.pathSegment}"
    }

    /** Trim surrounding whitespace and every trailing `/`; `null` when nothing is left. */
    public fun normalizedBase(raw: String?): String? = raw?.trim()?.trimEnd('/')?.ifEmpty { null }
}
