// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/*
 * `app` — installable Android APK (Phase 1.15c).
 *
 * Compose UI on top of the `lakko-core` AAR (1.15b). For 1.15c
 * the only screen is "Hello core" — a smoke surface that
 * constructs a fresh `Identity` via the UniFFI-generated
 * Kotlin bindings, reads `signaturePublicKey()`, renders the
 * hex. Same shape as `apps/web`'s 1.16a `DevProbe.tsx`:
 * proves the JNA cdylib bridge works on a real device before
 * 1.15d builds the OPAQUE-PAKE auth surface on top.
 *
 * No app icon override (default mipmap from AGP), no
 * launcher splash, no localisation — all polish lands later
 * once the real flows take shape.
 */

plugins {
    alias(libs.plugins.android.application)
    alias(libs.plugins.kotlin.android)
    alias(libs.plugins.kotlin.compose)
    // 1.15d2 — kotlinx-serialization compiler plugin. Generates
    // the `Serializer` implementations for the `@Serializable`
    // JSON data classes under `me.lakko.app.auth.wire`.
    alias(libs.plugins.kotlin.serialization)
}

android {
    namespace = "me.lakko.app"

    compileSdk = libs.versions.android.compile.sdk.get().toInt()

    defaultConfig {
        // `me.lakko.dev` keeps debug installs separate from a
        // future production `me.lakko` package on the same
        // device — handy when smoke-testing alongside a
        // released build. Override per-buildType when prod
        // signing lands in a later sprint.
        applicationId = "me.lakko.dev"

        minSdk    = libs.versions.android.min.sdk.get().toInt()
        targetSdk = libs.versions.android.target.sdk.get().toInt()

        versionCode = 1
        versionName = "0.1.0-dev"

        // Same ABI matrix as `lakko-core` — keep both in
        // lock-step so the APK packager never asks for a .so
        // we didn't ship.
        ndk {
            abiFilters += setOf("arm64-v8a", "x86_64")
        }

        // 1.20d — Firebase Messaging configuration. We deliberately
        // do NOT apply the `com.google.gms:google-services` Gradle
        // plugin (which would require a checked-in
        // `google-services.json` at build time). Instead Firebase
        // is initialised programmatically at runtime via
        // `FirebaseOptions.Builder()` populated from these
        // BuildConfig fields. If every field is empty the runtime
        // init is skipped + the PushSubscriber returns null — the
        // app keeps working without push (right posture for any
        // contributor not actively iterating on push).
        //
        // Source the values from gradle properties (set via
        // `apps/android/local.properties` or `-P` flags) so they
        // never end up in version control. The `?:` fallbacks
        // produce empty strings when unset.
        buildConfigField(
            "String",
            "FCM_PROJECT_ID",
            "\"${project.findProperty("lakko.fcm.projectId") as String? ?: ""}\"",
        )
        buildConfigField(
            "String",
            "FCM_APPLICATION_ID",
            "\"${project.findProperty("lakko.fcm.applicationId") as String? ?: ""}\"",
        )
        buildConfigField(
            "String",
            "FCM_API_KEY",
            "\"${project.findProperty("lakko.fcm.apiKey") as String? ?: ""}\"",
        )
        buildConfigField(
            "String",
            "FCM_SENDER_ID",
            "\"${project.findProperty("lakko.fcm.senderId") as String? ?: ""}\"",
        )

        // The deployment endpoints (`LAKKO_API_BASE_URL`, `LAKKO_SFU_URL`)
        // are per build type — see `buildTypes` below.

        // G-13 (ADR-0015) — optional build-time pin of the device-attestation
        // verifying key (base64url of the 32-byte Ed25519 key). When set,
        // `DeviceAttestKey` never consults the JWKS endpoint, so a compromised
        // JWKS-serving tier can't substitute a key it controls. Sourced from a
        // gradle property (`apps/android/local.properties` or
        // `-P lakko.device.attest.key=...`); empty when unset (JWKS fallback).
        buildConfigField(
            "String",
            "LAKKO_DEVICE_ATTEST_KEY",
            "\"${project.findProperty("lakko.device.attest.key") as String? ?: ""}\"",
        )

        // ADR-0078 section 3 - optional build-time pin of push-gw's X25519
        // wake key (base64url of 32 bytes). When set, `PushWakeKey` never
        // fetches `/push/wake-key`. Sourced from the gradle property
        // `lakko.push.wake.key`; empty when unset (fetched once per process).
        buildConfigField(
            "String",
            "LAKKO_PUSH_WAKE_KEY",
            "\"${project.findProperty("lakko.push.wake.key") as String? ?: ""}\"",
        )

        // ADR-0060 section 5 - optional build-time pin of the discovery
        // username-certificate verifying key (base64url of the 32-byte Ed25519
        // key). When set, a claimed username is verified against it and the
        // `/.well-known/lakko-username-cert-key` endpoint is never consulted;
        // empty when unset (fetched once per process).
        buildConfigField(
            "String",
            "LAKKO_USERNAME_CERT_KEY",
            "\"${project.findProperty("lakko.username.cert.key") as String? ?: ""}\"",
        )

        // ADR-0037 (G-11) — backup-head staleness window in SECONDS (the Android
        // mirror of the web `VITE_LAKKO_BACKUP_HEAD_STALENESS_SECS`). A restore
        // freshness gate for a fresh-install (no floor) restore: a head whose
        // `signed_at` is older than this is flagged stale (user-acknowledged
        // override). Empty when unset → `backupHeadStalenessSecs()` falls back to
        // the 30-day default. Sourced from a gradle property
        // (`-P lakko.backup.head.staleness.secs=...` or `local.properties`).
        buildConfigField(
            "String",
            "LAKKO_BACKUP_HEAD_STALENESS_SECS",
            "\"${project.findProperty("lakko.backup.head.staleness.secs") as String? ?: ""}\"",
        )

        // KT-log S3e — transparency-log verification config (the Android
        // mirror of the web `VITE_LAKKO_TRANSPARENCY_*` env vars). Both
        // sourced from gradle properties (set via
        // `apps/android/local.properties` or `-P lakko.transparency.url=...`)
        // so they never land in version control; empty when unset.
        //
        // `LAKKO_TRANSPARENCY_URL` — the transparency service base URL (no
        // trailing path). Blank → the whole log check is OFF.
        // `LAKKO_TRANSPARENCY_OPERATOR_KEY` — base64 (standard alphabet) of
        // the operator's 32-byte Ed25519 verifying key, pinned out-of-band.
        // The client NEVER trusts `/transparency/operator-key` for
        // verification. Blank / malformed → the log check is OFF.
        //
        // The skip path is fail-OPEN: the log is an equivocation-detection
        // layer on top of the fail-closed supersession/pin gate (ADR-0016),
        // so an unprovisioned build runs without the extra check rather
        // than refusing to start.
        buildConfigField(
            "String",
            "LAKKO_TRANSPARENCY_URL",
            "\"${project.findProperty("lakko.transparency.url") as String? ?: ""}\"",
        )
        buildConfigField(
            "String",
            "LAKKO_TRANSPARENCY_OPERATOR_KEY",
            "\"${project.findProperty("lakko.transparency.operator.key") as String? ?: ""}\"",
        )
        // `LAKKO_TRANSPARENCY_WITNESS_KEY` (ADR-0020 split-view defence) —
        // base64 (standard alphabet) of the witness's 32-byte Ed25519
        // verifying key, pinned out-of-band like the operator key. When
        // set, the client REQUIRES a valid witness co-signature over every
        // fetched root (fail-closed); blank → the witness gate is OFF and
        // the client keeps the operator-only posture (transition fallback).
        // Retained as the BACKWARD-COMPAT source for the K-of-N key SET
        // below (folded into a one-element set at threshold 1).
        buildConfigField(
            "String",
            "LAKKO_TRANSPARENCY_WITNESS_KEY",
            "\"${project.findProperty("lakko.transparency.witness.key") as String? ?: ""}\"",
        )
        // `LAKKO_TRANSPARENCY_WITNESS_KEYS` (ADR-0039 K-of-N witness QUORUM,
        // G-30 finding-1) — a COMMA-SEPARATED list of base64 (url-safe or
        // standard alphabet, padding optional) 32-byte Ed25519 witness
        // verifying keys, all pinned out-of-band. When non-empty, the client
        // requires a K-of-N QUORUM: at least `..._WITNESS_THRESHOLD` DISTINCT
        // pinned witnesses must each have co-signed the served root, or the
        // gate fails closed. Blank / all-malformed → falls back to the
        // singular key above, else the witness gate is OFF (transition
        // posture). Sourced from a gradle property so it never lands in
        // version control; empty when unset.
        buildConfigField(
            "String",
            "LAKKO_TRANSPARENCY_WITNESS_KEYS",
            "\"${project.findProperty("lakko.transparency.witness.keys") as String? ?: ""}\"",
        )
        // `LAKKO_TRANSPARENCY_WITNESS_THRESHOLD` (ADR-0039) — the K-of-N
        // quorum threshold (the minimum number of DISTINCT pinned witnesses
        // that must have co-signed). Defaults to "1" (unset / unparseable /
        // non-positive). Only consulted when the key SET above is non-empty;
        // a threshold LARGER than the pinned-key count makes the quorum
        // unsatisfiable → the gate fails closed (the correct posture).
        buildConfigField(
            "String",
            "LAKKO_TRANSPARENCY_WITNESS_THRESHOLD",
            "\"${project.findProperty("lakko.transparency.witness.threshold") as String? ?: "1"}\"",
        )

        // ADR-0026 Phase 4 — anti-censorship bootstrap pin. All three
        // sourced from gradle properties (`apps/android/local.properties`
        // or `-P lakko.bootstrap.*=...`) so they never land in version
        // control; empty when unset. There is no insecure default —
        // `BootstrapPin` returns null on a blank/malformed anchor and
        // `AntiCensorshipManager` stays disarmed, so an unprovisioned
        // build cannot arm the tunnel.
        //
        // `LAKKO_BOOTSTRAP_TRUST_ANCHOR` — base64 (standard alphabet) of
        // the 32-byte Ed25519 verifying key the signed bootstrap config is
        // checked against, pinned out-of-band at build time.
        // `LAKKO_BOOTSTRAP_DOH_DOMAIN` — the rendezvous domain resolved
        // over JSON-DoH (e.g. `rendezvous.lakko.me`).
        // `LAKKO_BOOTSTRAP_CONFIG_PATH` — the signed-config path on that
        // domain (e.g. `/bootstrap.json`).
        buildConfigField(
            "String",
            "LAKKO_BOOTSTRAP_TRUST_ANCHOR",
            "\"${project.findProperty("lakko.bootstrap.trust.anchor") as String? ?: ""}\"",
        )
        buildConfigField(
            "String",
            "LAKKO_BOOTSTRAP_DOH_DOMAIN",
            "\"${project.findProperty("lakko.bootstrap.doh.domain") as String? ?: ""}\"",
        )
        buildConfigField(
            "String",
            "LAKKO_BOOTSTRAP_CONFIG_PATH",
            "\"${project.findProperty("lakko.bootstrap.config.path") as String? ?: ""}\"",
        )

        // ADR-0038 (S4) — signed "update available" notice config. All sourced
        // from gradle properties (`apps/android/local.properties` or
        // `-P lakko.release.*=...`) so they never land in version control.
        //
        // `LAKKO_RELEASE_KEY` — base64url (no-pad) of the 32-byte Ed25519
        // publisher release verifying key (the dedicated, OFFLINE publisher key,
        // ADR-0038 §3). Empty when unset → the whole feature is DORMANT (no
        // fetch, no verify, no banner). Pin-only: there is NO serving-tier
        // fallback (unlike `DeviceAttestKey`'s JWKS), so an unpinned build simply
        // has the notice off — fail-safe-toward-silence.
        buildConfigField(
            "String",
            "LAKKO_RELEASE_KEY",
            "\"${project.findProperty("lakko.release.key") as String? ?: ""}\"",
        )
        // `LAKKO_RELEASE_MANIFEST_URL` — the signed-manifest URL (a raw 89-byte
        // Ed25519 blob, not JSON — it carries a 64-byte signature). Default is the
        // pinned static serve path on the `lakko.me` vhost; the Ed25519 signature
        // (not the URL) is the security boundary.
        buildConfigField(
            "String",
            "LAKKO_RELEASE_MANIFEST_URL",
            "\"${project.findProperty("lakko.release.manifest.url") as String? ?: "https://lakko.me/.well-known/lakko-release"}\"",
        )
        // `LAKKO_PLAY_STORE_URL` — the outbound Play deep-link the Update button
        // opens. DEFAULT EMPTY because no production Play listing exists yet; when
        // empty the Update button is hidden and (per the S4 gate) the banner stays
        // dormant unless `LAKKO_RELEASE_CHECK_DEV_OVERRIDE` is set.
        buildConfigField(
            "String",
            "LAKKO_PLAY_STORE_URL",
            "\"${project.findProperty("lakko.play.store.url") as String? ?: ""}\"",
        )
        // `LAKKO_CURRENT_RELEASE` — this build's monotone release integer, compared
        // as an integer against the manifest's `latest_release` /
        // `min_supported_release`. Default 1 (the first release). Decoupled from
        // the cosmetic `versionName` / `versionCode` (ADR-0038 §2).
        buildConfigField(
            "String",
            "LAKKO_CURRENT_RELEASE",
            "\"${project.findProperty("lakko.current.release") as String? ?: "1"}\"",
        )
        // `LAKKO_RELEASE_CHECK_DEV_OVERRIDE` — force the banner to show even
        // without a Play store URL, for pre-launch dogfood testing (UX §5 optional
        // hardening). Default false. A text-only informational notice (no Update
        // button) then appears once per NEW version.
        buildConfigField(
            "boolean",
            "LAKKO_RELEASE_CHECK_DEV_OVERRIDE",
            "${(project.findProperty("lakko.release.check.dev.override") as String?)?.toBoolean() ?: false}",
        )

        // ADR-0061 (S6) — signed translation updates.
        //
        // `LAKKO_TRANSLATION_KEY` — base64url (no-pad) of the 32-byte Ed25519
        // translation-publisher verifying key (a dedicated offline key, not the
        // release key). Empty when unset → the feature is DORMANT: no fetch, the
        // built-in translations only.
        buildConfigField(
            "String",
            "LAKKO_TRANSLATION_KEY",
            "\"${project.findProperty("lakko.translation.key") as String? ?: ""}\"",
        )
        // `LAKKO_TRANSLATION_BUNDLE_URL` — the signed bundle carrying every
        // language (the same bytes for every device, so the fetch reveals no
        // language). The signature, not the URL, is the security boundary.
        buildConfigField(
            "String",
            "LAKKO_TRANSLATION_BUNDLE_URL",
            "\"${project.findProperty("lakko.translation.url") as String? ?: "https://lakko.me/.well-known/lakko-translations"}\"",
        )

        // G3-4-mobile — wire androidx.test runner for the
        // `app/src/androidTest/` instrumented test source set.
        // `./gradlew :app:connectedDebugAndroidTest` boots a
        // device / emulator + installs `app-debug-androidTest.apk`
        // + invokes AndroidJUnitRunner against the @Test methods
        // there. Without this line the test APK packages but
        // never starts.
        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"
    }

    testOptions {
        unitTests {
            // The first UI tests on Android. `createComposeRule()` under
            // Robolectric hosts the composition in a real Activity, so the
            // resource table has to be the real one: MaterialTheme resolves the
            // `Theme.Material3.*` parent from `res/values/themes.xml`, and the
            // layout is measured at the density and font scale that table
            // states. Without this every UI test fails before it can assert.
            isIncludeAndroidResources = true
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    kotlinOptions {
        jvmTarget = "17"
    }

    buildFeatures {
        compose = true
        // 1.20d — re-enabled so the four `FCM_*` BuildConfig
        // string fields from `defaultConfig` make it onto the
        // runtime classpath. PushSubscriber reads them at app
        // startup to decide whether to initialise Firebase
        // programmatically.
        buildConfig = true
    }

    buildTypes {
        // Deployment endpoints, per build type. A gradle property overrides
        // the default for any build (`-P lakko.api.url=...` /
        // `-P lakko.sfu.url=...`, or `apps/android/local.properties`); set it
        // to an empty string to get the local-development fallbacks.
        //
        // `LAKKO_API_BASE_URL` — the native API base every REST client derives
        // its service base from (`ApiEndpoints`): `<base>/auth`, `/ds`,
        // `/storage`, `/discovery`, `/push-gw`, `/channels`, `/gifs`. Empty ->
        // each client keeps its `http://10.0.2.2:<port>` emulator dev bind (a
        // release build logs a warning).
        //   debug:   https://test.lakko.me/native (the test stack)
        //   release: empty until a production release sets it
        //
        // `LAKKO_SFU_URL` — the SFU WebSocket origin (`ws://` / `wss://`, no
        // trailing path); `SfuConfig` trims it and `SfuSignalingClient`
        // appends `/v1/connect`. Empty -> `SfuConfig.baseUrl` is null and the
        // group-call trigger refuses with a clear log line (1x1 P2P calls are
        // unaffected).
        //   debug:   wss://test.lakko.me/sfu
        //   release: empty
        //
        // `LAKKO_WEB_ORIGIN` — the web origin this build mints its `/i/`, `/g/`
        // and `/c/` links under (`ApiEndpoints.webOrigin`). It must be the host
        // the same stack's web client and app-link association live on, or a
        // link made on the test stack sends people to production. Empty ->
        // `https://lakko.me`.
        //   debug:   https://test.lakko.me
        //   release: https://lakko.me
        fun com.android.build.api.dsl.ApplicationBuildType.endpointFields(
            apiDefault: String,
            sfuDefault: String,
            webOriginDefault: String,
        ) {
            buildConfigField(
                "String",
                "LAKKO_API_BASE_URL",
                "\"${project.findProperty("lakko.api.url") as String? ?: apiDefault}\"",
            )
            buildConfigField(
                "String",
                "LAKKO_SFU_URL",
                "\"${project.findProperty("lakko.sfu.url") as String? ?: sfuDefault}\"",
            )
            buildConfigField(
                "String",
                "LAKKO_WEB_ORIGIN",
                "\"${project.findProperty("lakko.web.origin") as String? ?: webOriginDefault}\"",
            )
        }
        debug {
            endpointFields(
                apiDefault = "https://test.lakko.me/native",
                sfuDefault = "wss://test.lakko.me/sfu",
                webOriginDefault = "https://test.lakko.me",
            )
        }
        release {
            endpointFields(apiDefault = "", sfuDefault = "", webOriginDefault = "https://lakko.me")
            // Smoke build only — release shrinking is wired
            // properly in 1.15d when there's enough code to
            // shrink. The lakko-core consumer-rules.pro keeps
            // UniFFI / JNA from being renamed when shrinking
            // does land.
            isMinifyEnabled = false
        }
    }

    packaging {
        // JNA ships its own `libjnidispatch.so` per ABI inside
        // the `@aar` artefact. AGP merges that with our Rust
        // `.so` from lakko-core; no extra resources to pick.
        // The `pickFirsts` line below would handle conflicts
        // if any other dep shipped a colliding .so name —
        // left commented as a hint, not enabled today.
        // jniLibs {
        //     pickFirsts += listOf("**/libc++_shared.so")
        // }
    }
}

// ADR-0047 §3 — the emoji picker's data. ONE dataset for all three clients:
// the file the web client bundles, generated offline by
// `apps/web/scripts/gen-emoji-data.mjs` from pinned Unicode 15.1 + CLDR 45
// sources (annotated `Unicode-3.0` in the repo's REUSE.toml at its source
// path). It is copied into this app's assets at build time rather than
// checked in a second time, so the Android set cannot drift from the web set,
// and nothing fetches emoji data at runtime.
val emojiDataSource: File =
    rootProject.layout.projectDirectory.file("../web/src/media/emoji/emoji-data.json").asFile
val copyEmojiData = tasks.register<Sync>("copyEmojiData") {
    from(emojiDataSource)
    into(layout.buildDirectory.dir("generated/lakko-emoji-assets/emoji"))
}
android.sourceSets.getByName("main").assets.srcDir(
    layout.buildDirectory.dir("generated/lakko-emoji-assets"),
)
tasks.named("preBuild") { dependsOn(copyEmojiData) }
android.testOptions.unitTests.all {
    // The emoji index tests parse the real dataset, from its source path.
    it.systemProperty("lakko.emojiDataPath", emojiDataSource.absolutePath)
    // Where the Roborazzi baselines live, and when a run is allowed to WRITE
    // them. Recording is opt-in per run — `-Plakko.roborazzi.record=true` — so
    // an ordinary verification run can only ever compare: a run that silently
    // re-recorded would make every baseline agree with whatever the code does
    // today, which is the one thing a baseline must not do.
    it.systemProperty("roborazzi.output.dir", file("src/test/screenshots").absolutePath)
    val recording = (project.findProperty("lakko.roborazzi.record") as String?).toBoolean()
    it.systemProperty("roborazzi.test.record", recording.toString())
    it.systemProperty("roborazzi.test.verify", (!recording).toString())
    // Test hygiene. `lakko.test.networkLog`: every request a unit test sent
    // without a stubbed engine (the test source set replaces the app's base
    // engine with one that refuses and records, see
    // `src/test/kotlin/me/lakko/app/testing/ForbiddenNetworkEngine.kt`).
    // `lakko.test.leakLog`: every exception a coroutine could deliver to
    // nobody, which `runTest` otherwise reports in whichever test starts
    // next. A run that leaves either file non-empty FAILS, and says what
    // was recorded. Paths are plain strings so the configuration cache
    // holds no project reference.
    val networkLogPath = layout.buildDirectory.file("test-hygiene/unstubbed-network.log").get().asFile.absolutePath
    val leakLogPath = layout.buildDirectory.file("test-hygiene/leaked-coroutine-exceptions.log").get().asFile.absolutePath
    it.systemProperty("lakko.test.networkLog", networkLogPath)
    it.systemProperty("lakko.test.leakLog", leakLogPath)
    it.doFirst {
        for (path in listOf(networkLogPath, leakLogPath)) {
            val file = File(path)
            file.parentFile.mkdirs()
            file.delete()
        }
    }
    it.doLast {
        val problems = listOf(
            "unit tests reached the network without a stubbed engine" to networkLogPath,
            "coroutines leaked exceptions nobody caught" to leakLogPath,
        ).filter { (_, path) -> File(path).let { f -> f.exists() && f.length() > 0L } }
        if (problems.isNotEmpty()) {
            throw GradleException(
                problems.joinToString("\n") { (what, path) ->
                    what + ": see " + path + "\n" +
                        File(path).readLines().filter { l -> l.startsWith("----") }.distinct().take(40).joinToString("\n")
                },
            )
        }
    }
}

dependencies {
    // The local AAR — Rust cdylib + UniFFI Kotlin bindings.
    implementation(project(":lakko-core"))

    // G3-sframe-native N-5 — local AAR with the SFrame JNI
    // bridge. Bundles libsframe_jni.so (one per shipped ABI)
    // plus the LakkoFrameEncryptor / Decryptor Kotlin classes
    // that org.webrtc.RtpSender.setFrameEncryptor accepts.
    implementation(project(":lakko-sframe"))

    // Compose stack via BOM — one version pin covers every
    // androidx.compose.* artefact.
    val composeBom = platform(libs.compose.bom)
    implementation(composeBom)
    implementation(libs.compose.ui)
    implementation(libs.compose.ui.graphics)
    implementation(libs.compose.material3)
    // Extended Compose icon set — base material3 ships only a
    // tiny glyph table; the in-call UI (3c6-android) needs Mic /
    // Videocam / CallEnd / ScreenShare / PanTool / PushPin /
    // FlipCameraAndroid / Group icons.
    implementation(libs.compose.material.icons.extended)
    // Google Material XML library — for the
    // `Theme.Material3.DayNight.NoActionBar` style the
    // Activity's manifest theme inherits. Separate from
    // `compose.material3` which is the Compose UI toolkit.
    implementation(libs.material.android)
    implementation(libs.activity.compose)
    // Client redesign Phase A — Navigation Compose drives the five-tab shell,
    // the pushed chat / settings destinations and predictive back.
    implementation(libs.androidx.navigation.compose)

    // Compose tooling (Layout Inspector previews etc.) — debug
    // only so they don't bloat release APKs.
    debugImplementation(libs.compose.ui.tooling)
    implementation(libs.compose.ui.tooling.preview)

    // 1.15d4 — ViewModel + StateFlow ↔ Compose glue.
    implementation(libs.lifecycle.viewmodel.compose)
    implementation(libs.lifecycle.runtime.compose)

    // 1.15d2 — networking + JSON for the OPAQUE auth client.
    //
    // Ktor's `client-core` is the suspending HTTP surface;
    // `client-okhttp` is the engine that actually opens
    // sockets on Android (production-grade HTTP/2 + connection
    // pooling); `client-content-negotiation` is the plug that
    // auto-serializes / -deserializes JSON request bodies; the
    // `serialization-kotlinx-json` artefact is the codec
    // implementation that plug uses.
    implementation(libs.ktor.client.core)
    implementation(libs.ktor.client.okhttp)
    implementation(libs.ktor.client.content.negotiation)
    implementation(libs.ktor.serialization.kotlinx.json)
    // SFU group-call signaling (SfuSignalingClient.kt) rides Ktor's
    // WebSocket plugin over the OkHttp engine above. (Also an
    // androidTest dep for the SFU instrumented test.)
    implementation(libs.ktor.client.websockets)

    // kotlinx-coroutines on Android. Ktor pulls -core in
    // transitively, but the -android dispatcher (`Dispatchers.Main`
    // backed by Looper) is Android-specific and needs to be
    // declared explicitly. Coroutines drive both Ktor's
    // suspending HTTP and our `Dispatchers.Default` PoW solve.
    implementation(libs.kotlinx.coroutines.core)
    implementation(libs.kotlinx.coroutines.android)

    // kotlinx-serialization runtime — required by the `@Serializable`
    // data classes (Ktor pulls the JSON-side via
    // `serialization-kotlinx-json`, but the core annotation
    // package needs to be on the runtime classpath of any
    // module that declares `@Serializable` types).
    implementation(libs.kotlinx.serialization.json)

    // 1.20d — Firebase Messaging via the BoM. We do NOT apply
    // the `com.google.gms:google-services` plugin (would require
    // a checked-in `google-services.json` at build time); Firebase
    // is initialised programmatically at runtime — see
    // `me.lakko.app.push.PushSubscriber`.
    implementation(platform(libs.firebase.bom))
    implementation(libs.firebase.messaging)
    // 1.20d — `Task<T>.await()` extension that converts the
    // Google Play Services callback-based `Task` into a `suspend`
    // shape. Required because `FirebaseMessaging.getInstance().token`
    // returns `Task<String>`.
    implementation(libs.kotlinx.coroutines.play.services)
    // 1.20d — EncryptedSharedPreferences for the device push
    // pseudonym (mirror of iOS Keychain posture).
    implementation(libs.androidx.security.crypto)

    // 2a2e — Jetpack Credential Manager for WebAuthn passkey
    // enrolment + assertion. `credentials` is the unified
    // public API; `credentials-play-services-auth` bridges to
    // Google Play Services on devices that ship it (everywhere
    // outside CN/RU appstore-only builds).
    implementation(libs.androidx.credentials)
    implementation(libs.androidx.credentials.play.services.auth)

    // 2a3b4 — add-device QR pipeline. CameraX drives the live
    // preview + frame analysis, ML Kit decodes QR codes inside
    // those frames, ZXing-core encodes the pairing payload into
    // the QR matrix the joining device renders. All three together
    // cover the iOS CoreImage + AVCaptureMetadataOutput pair and
    // the web `qrcode + jsqr` pair on a single AAR surface.
    implementation(libs.androidx.camera.core)
    implementation(libs.androidx.camera.camera2)
    implementation(libs.androidx.camera.lifecycle)
    implementation(libs.androidx.camera.view)
    // ADR-0047 — video notes: CameraX `Recorder` / `VideoCapture` write the
    // H.264 + AAC MP4 every client plays. CameraX core, camera2, lifecycle
    // and view were already here for the QR scanner; this is the one module
    // that records.
    implementation(libs.androidx.camera.video)
    implementation(libs.mlkit.barcode.scanning)
    implementation(libs.zxing.core)

    // G3-2 — WebRTC pod. The artifact ships the Java/Kotlin
    // surface under the `org.webrtc` package the upstream
    // libwebrtc has used since 2018; only the Maven
    // coordinate changed from the abandoned
    // `org.webrtc:google-webrtc` to the community-maintained
    // `io.github.webrtc-sdk:android`. Consumer code under
    // `me.lakko.app.calls.WebRtcPeerConnection` imports
    // `org.webrtc.PeerConnection`, `org.webrtc.SessionDescription`,
    // and friends — unchanged from the abandoned artifact's
    // surface.
    implementation(libs.webrtc.sdk.android)

    // 1.15d2 — JVM-side unit tests. `testDebugUnitTest` runs
    // these on the host JDK without any emulator; perfect for
    // the AuthClient flow tests that drive the high-level
    // signup/login/recover against a Ktor MockEngine.
    testImplementation(libs.junit)
    testImplementation(libs.kotlinx.coroutines.test)
    testImplementation(libs.ktor.client.mock)
    testImplementation(libs.kotlinx.serialization.json)

    // The first UI tests on Android. Robolectric runs the framework in-process,
    // which is what lets `createComposeRule()` measure real layout and dispatch
    // real gestures under `testDebugUnitTest` — no emulator, no adb.
    //
    // `ui-test-junit4` comes through the same Compose BOM the app draws with, so
    // the test surface can never drift from the Compose under test.
    // `ui-test-manifest` is the empty activity the rule hosts the composition
    // in; it is a DEBUG dep rather than a test one because the unit tests run
    // against the debug variant's merged manifest, and the activity has to be
    // declared there for the rule to launch it.
    testImplementation(platform(libs.compose.bom))
    testImplementation(libs.compose.ui.test.junit4)
    debugImplementation(libs.compose.ui.test.manifest)
    testImplementation(libs.robolectric)
    // Roborazzi — a composition to a PNG, for the two things semantics cannot
    // answer: whether the waveform draws bars of differing heights, and whether
    // the bubble still looks like the baseline.
    testImplementation(libs.roborazzi)
    testImplementation(libs.roborazzi.compose)
    testImplementation(libs.roborazzi.junit.rule)

    // G3-4-mobile — instrumented test stack under
    // `app/src/androidTest/`. The first-pass SfuHealth
    // instrumented test only needs JUnit4 + the
    // AndroidJUnitRunner; Espresso (view-matching) and
    // mockito-android come later, when call UI grows
    // assertions worth pinning against a live emulator.
    androidTestImplementation(libs.androidx.test.runner)
    androidTestImplementation(libs.androidx.test.ext.junit)
    androidTestImplementation(libs.junit)
    // G3-4-mobile-call iter1 — the SfuSignalingInstrumentedTest
    // opens a real WebSocket to the dockerised SFU. Ktor's
    // OkHttp engine is already a production dep
    // (`libs.ktor.client.okhttp` above); pulling it into the
    // instrumented-test classpath gives us the same
    // `HttpClient { install(WebSockets) }` shape callers use
    // in production without dragging in a second WS stack.
    androidTestImplementation(libs.ktor.client.okhttp)
    androidTestImplementation(libs.ktor.client.websockets)
    androidTestImplementation(libs.kotlinx.coroutines.test)
    androidTestImplementation(libs.kotlinx.serialization.json)
}
