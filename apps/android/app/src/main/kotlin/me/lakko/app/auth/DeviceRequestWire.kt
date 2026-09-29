// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import kotlinx.serialization.Serializable

/*
 * ADR-0049 section 2 (the device link, amended 2026-09-28) — the wire shapes
 * of `services/auth/src/device_request.rs`. Every opaque value is standard
 * base64, except the request id, which is base64url without padding (it also
 * rides in a path).
 */

/** `POST /device/request` body. */
@Serializable
internal data class DeviceRequestCreateBody(
    /** The 32-byte login handle, base64url without padding. */
    val handle: String,
)

/** `POST /device/request` response. */
@Serializable
internal data class DeviceRequestCreateResponse(
    val request_id_b64: String,
    val expires_in_secs: Long,
)

/** `POST /device/request/status` body. */
@Serializable
internal data class DeviceRequestStatusBody(
    val request_id_b64: String,
)

/** `POST /device/request/status` response. */
@Serializable
internal data class DeviceRequestStatusResponse(
    val pending: Boolean,
    val pake_b_b64: String? = null,
    val outcome: String? = null,
    val confirm_b_b64: String? = null,
)

/** `POST /device/request/payload` body. */
@Serializable
internal data class DeviceRequestPayloadBody(
    val request_id_b64: String,
    val pake_a_b64: String,
    val sealed_payload_b64: String,
    val confirm_a_b64: String,
)

/** One open request as `GET /device/requests` lists it. */
@Serializable
internal data class DeviceRequestPendingWire(
    val request_id_b64: String,
    val created_at_ms: Long,
    val pake_b_b64: String? = null,
    val pake_a_b64: String? = null,
    val sealed_payload_b64: String? = null,
    val confirm_a_b64: String? = null,
    val outcome: String? = null,
)

/** `GET /device/requests` response. */
@Serializable
internal data class DeviceRequestListResponse(
    val request: DeviceRequestPendingWire? = null,
)

/** `POST /device/requests/{id}/pake` body. */
@Serializable
internal data class DeviceRequestAnswerBody(
    val pake_b_b64: String,
)

/** `POST /device/requests/{id}/result` body. */
@Serializable
internal data class DeviceRequestResultBody(
    val outcome: String,
    val confirm_b_b64: String? = null,
)
