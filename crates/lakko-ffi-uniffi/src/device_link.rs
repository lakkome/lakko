// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! UniFFI surface for `lakko-device-link` — the camera-less add-device
//! confirmation of ADR-0049 section 2 (amended 2026-09-28). Mirror of
//! `crates/lakko-ffi-wasm/src/device_link.rs`.
//!
//! Two objects, one per device:
//!
//! * [`DeviceLinkResponder`] — the signed-in (old) device. Constructed when a
//!   request shows up (`GET /device/requests`); it chooses the code to show
//!   and the SPAKE2 message to post (`POST /device/requests/{id}/pake`).
//!   [`DeviceLinkResponder::accept`] runs once, on the new device's values,
//!   and returns the pairing payload plus `confirm_b` to post as the
//!   `accepted` result, or [`FfiError::DeviceLinkWrongCode`].
//! * [`DeviceLinkInitiator`] — the new device. Constructed with the request
//!   id `POST /device/request` returned and the handle of the typed name;
//!   [`DeviceLinkInitiator::finish`] runs once, when the person has typed the
//!   code and `pake_b` has arrived; [`DeviceLinkInitiator::verify_confirmation`]
//!   checks the old device's `confirm_b` from the `accepted` status.
//!
//! Both objects are single-use and hold their secrets in zeroizing storage
//! (the `spake2` crate's own copy of its scalar excepted; see the crate docs).

use std::sync::Mutex;

use lakko_device_link::{
    initiate, ConfirmationCheck, LinkCode, LinkContext, LinkError, Responder, Zeroizing,
};

use crate::FfiError;

impl From<LinkError> for FfiError {
    fn from(e: LinkError) -> Self {
        match e {
            LinkError::WrongCode => Self::DeviceLinkWrongCode,
            other => Self::DeviceLink(format!("{}: {other}", other.code())),
        }
    }
}

fn already_used() -> FfiError {
    FfiError::DeviceLink("already-used: this device-link exchange already ran".into())
}

fn poisoned() -> FfiError {
    FfiError::DeviceLink("internal: device-link lock poisoned".into())
}

/// What the old device gets from a verified exchange.
#[derive(uniffi::Record, Clone)]
pub struct DeviceLinkAccepted {
    /// The ADR-0056 pairing payload; pass it to `parsePairingQr` with the
    /// account's own login handle, exactly as a scanned QR.
    pub pairing_payload: Vec<u8>,
    /// The old device's key confirmation (32 bytes); post it as
    /// `confirm_b_b64` with outcome `accepted`.
    pub confirm_b: Vec<u8>,
}

/// What the new device posts to `POST /device/request/payload`.
#[derive(uniffi::Record, Clone)]
pub struct DeviceLinkSealed {
    /// SPAKE2 side-A message (33 bytes).
    pub pake_a: Vec<u8>,
    /// The pairing payload under the agreed key.
    pub sealed_payload: Vec<u8>,
    /// The new device's key confirmation (32 bytes).
    pub confirm_a: Vec<u8>,
}

/// The signed-in device's side of one device-link exchange.
#[derive(uniffi::Object)]
pub struct DeviceLinkResponder {
    inner: Mutex<Option<Responder>>,
    code: Zeroizing<String>,
    pake_b: Vec<u8>,
}

#[uniffi::export]
impl DeviceLinkResponder {
    /// Choose a fresh code and start the exchange for `request_id` (16 bytes,
    /// the decoded `request_id_b64`) under `login_handle` (32 bytes, the
    /// handle of this account's own username).
    ///
    /// # Errors
    ///
    /// [`FfiError::DeviceLink`] `bad-input: ...` on a wrong length.
    #[uniffi::constructor]
    pub fn new(request_id: Vec<u8>, login_handle: Vec<u8>) -> Result<Self, FfiError> {
        let context = LinkContext::new(&request_id, &login_handle)?;
        let (responder, code) = Responder::start(context, &mut lakko_crypto::random::rng());
        let pake_b = responder.pake_message().to_vec();
        Ok(Self {
            inner: Mutex::new(Some(responder)),
            code: Zeroizing::new(code.as_str().to_string()),
            pake_b,
        })
    }

    /// The six digits to show, e.g. `"471903"`; render with
    /// `deviceLinkFormatCode` as `"47 19 03"`.
    pub fn code(&self) -> String {
        self.code.as_str().to_string()
    }

    /// The SPAKE2 message to post (33 bytes), as `pake_b_b64`.
    pub fn pake_message(&self) -> Vec<u8> {
        self.pake_b.clone()
    }

    /// Verify the new device's values and open its pairing payload. Runs
    /// once: a second call is refused, so a request gets one guess.
    ///
    /// # Errors
    ///
    /// [`FfiError::DeviceLinkWrongCode`] when the confirmation does not
    /// verify (post outcome `wrong_code`); [`FfiError::DeviceLink`]
    /// `malformed: ...` for a value of impossible shape (also post
    /// `wrong_code`), `already-used: ...` on a second call. Pair nothing on
    /// any error.
    pub fn accept(
        &self,
        pake_a: Vec<u8>,
        sealed_payload: Vec<u8>,
        confirm_a: Vec<u8>,
    ) -> Result<DeviceLinkAccepted, FfiError> {
        let responder = self
            .inner
            .lock()
            .map_err(|_| poisoned())?
            .take()
            .ok_or_else(already_used)?;
        let accepted = responder.accept(&pake_a, &sealed_payload, &confirm_a)?;
        Ok(DeviceLinkAccepted {
            pairing_payload: accepted.payload,
            confirm_b: accepted.confirm_b.to_vec(),
        })
    }
}

/// The new device's side of one device-link exchange.
#[derive(uniffi::Object)]
pub struct DeviceLinkInitiator {
    context: LinkContext,
    /// `None` before `finish`; the expected `confirm_b` after.
    check: Mutex<Option<ConfirmationCheck>>,
    finished: Mutex<bool>,
}

#[uniffi::export]
impl DeviceLinkInitiator {
    /// Bind to `request_id` (16 bytes, from `POST /device/request`) and
    /// `login_handle` (32 bytes, the handle of the name the person typed).
    ///
    /// # Errors
    ///
    /// [`FfiError::DeviceLink`] `bad-input: ...` on a wrong length.
    #[uniffi::constructor]
    pub fn new(request_id: Vec<u8>, login_handle: Vec<u8>) -> Result<Self, FfiError> {
        Ok(Self {
            context: LinkContext::new(&request_id, &login_handle)?,
            check: Mutex::new(None),
            finished: Mutex::new(false),
        })
    }

    /// Run the exchange with the old device's `pake_b` and the code the person
    /// typed (six digits; spaces and `-` ignored), sealing `pairing_payload`
    /// (the `buildPairingQr` bytes). Runs once.
    ///
    /// # Errors
    ///
    /// [`FfiError::DeviceLink`] with `invalid-code: ...` (not six digits;
    /// nothing is spent, the person may retype), `malformed: ...` (a `pake_b`
    /// of impossible shape), `bad-input: ...` (empty or over 2048-byte
    /// payload), or `already-used: ...` on a second successful call.
    pub fn finish(
        &self,
        pake_b: Vec<u8>,
        typed_code: String,
        pairing_payload: Vec<u8>,
    ) -> Result<DeviceLinkSealed, FfiError> {
        let typed_code = Zeroizing::new(typed_code);
        let code = LinkCode::parse(&typed_code)?;
        let mut finished = self.finished.lock().map_err(|_| poisoned())?;
        if *finished {
            return Err(already_used());
        }
        let (sealed, check) = initiate(
            &self.context,
            &code,
            &pake_b,
            &pairing_payload,
            &mut lakko_crypto::random::rng(),
        )?;
        *self.check.lock().map_err(|_| poisoned())? = Some(check);
        *finished = true;
        Ok(DeviceLinkSealed {
            pake_a: sealed.pake_a,
            sealed_payload: sealed.sealed_payload,
            confirm_a: sealed.confirm_a.to_vec(),
        })
    }

    /// Check the old device's `confirm_b` (from the `accepted` status) before
    /// trusting the Welcome that follows.
    ///
    /// # Errors
    ///
    /// [`FfiError::DeviceLinkWrongCode`] on a mismatch (whoever answered did
    /// not hold the code); [`FfiError::DeviceLink`] `malformed: ...` on a
    /// wrong length or `not-ready: ...` before `finish`.
    pub fn verify_confirmation(&self, confirm_b: Vec<u8>) -> Result<(), FfiError> {
        let guard = self.check.lock().map_err(|_| poisoned())?;
        let check = guard
            .as_ref()
            .ok_or_else(|| FfiError::DeviceLink("not-ready: finish has not run".into()))?;
        Ok(check.verify(&confirm_b)?)
    }
}

/// Normalise what the person typed to the six digits (`"47 19 03"` ->
/// `"471903"`), for enabling Continue.
///
/// # Errors
///
/// [`FfiError::DeviceLink`] `invalid-code: ...`.
#[uniffi::export]
pub fn device_link_normalize_code(input: String) -> Result<String, FfiError> {
    let input = Zeroizing::new(input);
    Ok(LinkCode::parse(&input)?.as_str().to_string())
}

/// Render a code as it is shown: three pairs, `"47 19 03"`.
///
/// # Errors
///
/// [`FfiError::DeviceLink`] `invalid-code: ...`.
#[uniffi::export]
pub fn device_link_format_code(code: String) -> Result<String, FfiError> {
    let code = Zeroizing::new(code);
    Ok(LinkCode::parse(&code)?.grouped())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_two_objects_link_under_the_right_code() {
        let responder = DeviceLinkResponder::new(vec![1; 16], vec![2; 32]).unwrap();
        let initiator = DeviceLinkInitiator::new(vec![1; 16], vec![2; 32]).unwrap();
        let typed = device_link_format_code(responder.code()).unwrap();
        let sealed = initiator
            .finish(responder.pake_message(), typed, b"payload".to_vec())
            .unwrap();
        let accepted = responder
            .accept(sealed.pake_a, sealed.sealed_payload, sealed.confirm_a)
            .unwrap();
        assert_eq!(accepted.pairing_payload, b"payload");
        initiator.verify_confirmation(accepted.confirm_b).unwrap();
    }

    #[test]
    fn a_wrong_code_is_typed_and_the_responder_is_spent() {
        let responder = DeviceLinkResponder::new(vec![1; 16], vec![2; 32]).unwrap();
        let initiator = DeviceLinkInitiator::new(vec![1; 16], vec![2; 32]).unwrap();
        let wrong = if responder.code() == "000000" {
            "000001"
        } else {
            "000000"
        };
        let sealed = initiator
            .finish(responder.pake_message(), wrong.into(), b"p".to_vec())
            .unwrap();
        assert!(matches!(
            responder.accept(
                sealed.pake_a.clone(),
                sealed.sealed_payload.clone(),
                sealed.confirm_a.clone()
            ),
            Err(FfiError::DeviceLinkWrongCode)
        ));
        assert!(matches!(
            responder.accept(sealed.pake_a, sealed.sealed_payload, sealed.confirm_a),
            Err(FfiError::DeviceLink(m)) if m.starts_with("already-used: ")
        ));
        assert!(matches!(
            initiator.finish(responder.pake_message(), "123456".into(), b"p".to_vec()),
            Err(FfiError::DeviceLink(m)) if m.starts_with("already-used: ")
        ));
        assert!(matches!(
            initiator.verify_confirmation(vec![0; 32]),
            Err(FfiError::DeviceLinkWrongCode)
        ));
    }

    #[test]
    fn a_mistyped_code_spends_nothing() {
        let responder = DeviceLinkResponder::new(vec![1; 16], vec![2; 32]).unwrap();
        let initiator = DeviceLinkInitiator::new(vec![1; 16], vec![2; 32]).unwrap();
        assert!(matches!(
            initiator.finish(responder.pake_message(), "12 34".into(), b"p".to_vec()),
            Err(FfiError::DeviceLink(m)) if m.starts_with("invalid-code: ")
        ));
        assert!(matches!(
            initiator.verify_confirmation(vec![0; 32]),
            Err(FfiError::DeviceLink(m)) if m.starts_with("not-ready: ")
        ));
        assert!(initiator
            .finish(responder.pake_message(), responder.code(), b"p".to_vec())
            .is_ok());
        assert_eq!(
            device_link_normalize_code("47-19 03".into()).unwrap(),
            "471903"
        );
        assert!(DeviceLinkInitiator::new(vec![1; 15], vec![2; 32]).is_err());
    }
}
