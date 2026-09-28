// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! wasm-bindgen exposure of `lakko-device-link` — the camera-less add-device
//! confirmation of ADR-0049 section 2 (amended 2026-09-28). Mirror of
//! `lakko-ffi-uniffi/src/device_link.rs`; see there for the exchange.
//!
//! JS shape:
//!
//! * old device: `new DeviceLinkResponder(requestId, loginHandle)`, `.code`
//!   (six digits), `.pakeMessage` (post as `pake_b_b64`), then once
//!   `.accept(pakeA, sealedPayload, confirmA)` -> `{ pairingPayload, confirmB }`;
//! * new device: `new DeviceLinkInitiator(requestId, loginHandle)`, once
//!   `.finish(pakeB, typedCode, pairingPayload)` -> `{ pakeA, sealedPayload,
//!   confirmA }`, then `.verifyConfirmation(confirmB)`;
//! * `deviceLinkNormalizeCode(input)`, `deviceLinkFormatCode(code)`.
//!
//! A refusal throws an `Error` whose message is `"<code>: <reason>"` with code
//! `wrong-code`, `invalid-code`, `malformed`, `bad-input`, `already-used`,
//! `not-ready` or `internal` — the same text the UniFFI `DeviceLink` variant
//! carries (UniFFI raises `DeviceLinkWrongCode` for `wrong-code`).

use std::sync::Mutex;

use lakko_device_link::{
    initiate, ConfirmationCheck, LinkCode, LinkContext, LinkError, Responder, Zeroizing,
};
use wasm_bindgen::prelude::*;

fn message(e: LinkError) -> String {
    format!("{}: {e}", e.code())
}

fn js(e: LinkError) -> JsError {
    JsError::new(&message(e))
}

const ALREADY_USED: &str = "already-used: this device-link exchange already ran";
const POISONED: &str = "internal: device-link lock poisoned";

/// What the old device gets from a verified exchange.
#[wasm_bindgen]
#[derive(Debug)]
pub struct DeviceLinkAccepted {
    pairing_payload: Vec<u8>,
    confirm_b: Vec<u8>,
}

#[wasm_bindgen]
impl DeviceLinkAccepted {
    /// The ADR-0056 pairing payload; `parsePairingQr` it with the account's
    /// own login handle, exactly as a scanned QR.
    #[wasm_bindgen(getter, js_name = pairingPayload)]
    pub fn pairing_payload(&self) -> Vec<u8> {
        self.pairing_payload.clone()
    }

    /// The old device's key confirmation (32 bytes); post it as
    /// `confirm_b_b64` with outcome `accepted`.
    #[wasm_bindgen(getter, js_name = confirmB)]
    pub fn confirm_b(&self) -> Vec<u8> {
        self.confirm_b.clone()
    }
}

/// What the new device posts to `POST /device/request/payload`.
#[wasm_bindgen]
#[derive(Debug)]
pub struct DeviceLinkSealed {
    pake_a: Vec<u8>,
    sealed_payload: Vec<u8>,
    confirm_a: Vec<u8>,
}

#[wasm_bindgen]
impl DeviceLinkSealed {
    /// SPAKE2 side-A message (33 bytes).
    #[wasm_bindgen(getter, js_name = pakeA)]
    pub fn pake_a(&self) -> Vec<u8> {
        self.pake_a.clone()
    }

    /// The pairing payload under the agreed key.
    #[wasm_bindgen(getter, js_name = sealedPayload)]
    pub fn sealed_payload(&self) -> Vec<u8> {
        self.sealed_payload.clone()
    }

    /// The new device's key confirmation (32 bytes).
    #[wasm_bindgen(getter, js_name = confirmA)]
    pub fn confirm_a(&self) -> Vec<u8> {
        self.confirm_a.clone()
    }
}

/// The signed-in device's side of one device-link exchange.
#[wasm_bindgen]
pub struct DeviceLinkResponder {
    inner: Mutex<Option<Responder>>,
    code: Zeroizing<String>,
    pake_b: Vec<u8>,
}

impl DeviceLinkResponder {
    fn new_inner(request_id: &[u8], login_handle: &[u8]) -> Result<Self, LinkError> {
        let context = LinkContext::new(request_id, login_handle)?;
        let (responder, code) = Responder::start(context, &mut lakko_crypto::random::rng());
        let pake_b = responder.pake_message().to_vec();
        Ok(Self {
            inner: Mutex::new(Some(responder)),
            code: Zeroizing::new(code.as_str().to_string()),
            pake_b,
        })
    }

    fn accept_inner(
        &self,
        pake_a: &[u8],
        sealed_payload: &[u8],
        confirm_a: &[u8],
    ) -> Result<DeviceLinkAccepted, String> {
        let responder = self
            .inner
            .lock()
            .map_err(|_| POISONED.to_string())?
            .take()
            .ok_or_else(|| ALREADY_USED.to_string())?;
        let accepted = responder
            .accept(pake_a, sealed_payload, confirm_a)
            .map_err(message)?;
        Ok(DeviceLinkAccepted {
            pairing_payload: accepted.payload,
            confirm_b: accepted.confirm_b.to_vec(),
        })
    }
}

#[wasm_bindgen]
impl DeviceLinkResponder {
    /// Choose a fresh code and start the exchange for `requestId` (16 bytes)
    /// under `loginHandle` (32 bytes, this account's own handle). Throws
    /// `bad-input: ...` on a wrong length.
    #[wasm_bindgen(constructor)]
    pub fn new(request_id: &[u8], login_handle: &[u8]) -> Result<Self, JsError> {
        Self::new_inner(request_id, login_handle).map_err(js)
    }

    /// The six digits to show, e.g. `"471903"`.
    #[wasm_bindgen(getter)]
    pub fn code(&self) -> String {
        self.code.as_str().to_string()
    }

    /// The SPAKE2 message to post (33 bytes), as `pake_b_b64`.
    #[wasm_bindgen(getter, js_name = pakeMessage)]
    pub fn pake_message(&self) -> Vec<u8> {
        self.pake_b.clone()
    }

    /// Verify the new device's values and open its pairing payload. Runs
    /// once. Throws `wrong-code: ...` (post `wrong_code`), `malformed: ...`
    /// (post `wrong_code`), or `already-used: ...`. Pair nothing on a throw.
    pub fn accept(
        &self,
        pake_a: &[u8],
        sealed_payload: &[u8],
        confirm_a: &[u8],
    ) -> Result<DeviceLinkAccepted, JsError> {
        self.accept_inner(pake_a, sealed_payload, confirm_a)
            .map_err(|m| JsError::new(&m))
    }
}

/// The new device's side of one device-link exchange.
#[wasm_bindgen]
pub struct DeviceLinkInitiator {
    context: LinkContext,
    check: Mutex<Option<ConfirmationCheck>>,
    finished: Mutex<bool>,
}

impl DeviceLinkInitiator {
    fn finish_inner(
        &self,
        pake_b: &[u8],
        typed_code: &str,
        pairing_payload: &[u8],
    ) -> Result<DeviceLinkSealed, String> {
        let code = LinkCode::parse(typed_code).map_err(message)?;
        let mut finished = self.finished.lock().map_err(|_| POISONED.to_string())?;
        if *finished {
            return Err(ALREADY_USED.to_string());
        }
        let (sealed, check) = initiate(
            &self.context,
            &code,
            pake_b,
            pairing_payload,
            &mut lakko_crypto::random::rng(),
        )
        .map_err(message)?;
        *self.check.lock().map_err(|_| POISONED.to_string())? = Some(check);
        *finished = true;
        Ok(DeviceLinkSealed {
            pake_a: sealed.pake_a,
            sealed_payload: sealed.sealed_payload,
            confirm_a: sealed.confirm_a.to_vec(),
        })
    }

    fn verify_inner(&self, confirm_b: &[u8]) -> Result<(), String> {
        let guard = self.check.lock().map_err(|_| POISONED.to_string())?;
        let check = guard
            .as_ref()
            .ok_or_else(|| "not-ready: finish has not run".to_string())?;
        check.verify(confirm_b).map_err(message)
    }
}

#[wasm_bindgen]
impl DeviceLinkInitiator {
    /// Bind to `requestId` (16 bytes, from `POST /device/request`) and
    /// `loginHandle` (32 bytes, the handle of the typed name). Throws
    /// `bad-input: ...` on a wrong length.
    #[wasm_bindgen(constructor)]
    pub fn new(request_id: &[u8], login_handle: &[u8]) -> Result<Self, JsError> {
        Ok(Self {
            context: LinkContext::new(request_id, login_handle).map_err(js)?,
            check: Mutex::new(None),
            finished: Mutex::new(false),
        })
    }

    /// Run the exchange with `pakeB` and the typed code, sealing
    /// `pairingPayload`. Runs once. Throws `invalid-code: ...` (nothing
    /// spent; the person may retype), `malformed: ...`, `bad-input: ...` or
    /// `already-used: ...`.
    pub fn finish(
        &self,
        pake_b: &[u8],
        typed_code: &str,
        pairing_payload: &[u8],
    ) -> Result<DeviceLinkSealed, JsError> {
        self.finish_inner(pake_b, typed_code, pairing_payload)
            .map_err(|m| JsError::new(&m))
    }

    /// Check the old device's `confirmB` from the `accepted` status before
    /// trusting the Welcome. Throws `wrong-code: ...`, `malformed: ...` or
    /// `not-ready: ...`.
    #[wasm_bindgen(js_name = verifyConfirmation)]
    pub fn verify_confirmation(&self, confirm_b: &[u8]) -> Result<(), JsError> {
        self.verify_inner(confirm_b).map_err(|m| JsError::new(&m))
    }
}

/// Normalise a typed code to its six digits (`"47 19 03"` -> `"471903"`).
/// Throws `invalid-code: ...`.
#[wasm_bindgen(js_name = deviceLinkNormalizeCode)]
pub fn device_link_normalize_code(input: &str) -> Result<String, JsError> {
    Ok(LinkCode::parse(input).map_err(js)?.as_str().to_string())
}

/// Render a code as three pairs, `"47 19 03"`. Throws `invalid-code: ...`.
#[wasm_bindgen(js_name = deviceLinkFormatCode)]
pub fn device_link_format_code(code: &str) -> Result<String, JsError> {
    Ok(LinkCode::parse(code).map_err(js)?.grouped())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_two_objects_link_and_refuse_a_second_use() {
        let r = DeviceLinkResponder::new_inner(&[1; 16], &[2; 32]).unwrap();
        let i = DeviceLinkInitiator {
            context: LinkContext::new(&[1; 16], &[2; 32]).unwrap(),
            check: Mutex::new(None),
            finished: Mutex::new(false),
        };
        assert!(i
            .finish_inner(&r.pake_b, "12", b"p")
            .unwrap_err()
            .starts_with("invalid-code: "));
        let sealed = i.finish_inner(&r.pake_b, &r.code, b"payload").unwrap();
        let accepted = r
            .accept_inner(&sealed.pake_a, &sealed.sealed_payload, &sealed.confirm_a)
            .unwrap();
        assert_eq!(accepted.pairing_payload, b"payload");
        i.verify_inner(&accepted.confirm_b).unwrap();
        assert!(i
            .verify_inner(&[0; 32])
            .unwrap_err()
            .starts_with("wrong-code: "));
        assert_eq!(
            r.accept_inner(&sealed.pake_a, &sealed.sealed_payload, &sealed.confirm_a)
                .unwrap_err(),
            ALREADY_USED
        );
        assert_eq!(
            i.finish_inner(&r.pake_b, &r.code, b"p").unwrap_err(),
            ALREADY_USED
        );
    }
}
