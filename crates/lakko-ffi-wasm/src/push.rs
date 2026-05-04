// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Mirror of `lakko-push`: `PushPayload` JSON wire format. JS code
//! receives push notifications from the OS pipeline, hands the raw
//! bytes here for parsing.

use lakko_push::{PushError, PushPayload as RustPushPayload, PSEUDONYM_LEN};
use wasm_bindgen::prelude::*;

fn push_err(e: PushError) -> JsError {
    JsError::new(&format!("{e}"))
}

/// Mirror of [`lakko_push::PushPayload`]. Three discriminated variants
/// addressed via `kind`:
///
/// - `"GroupMessage"` — `groupPseudonym` + `envelopePointer` set
/// - `"MailboxMessage"` — only `envelopePointer` set
/// - `"Wakeup"` — neither set
#[wasm_bindgen]
pub struct PushPayload {
    inner: RustPushPayload,
}

#[wasm_bindgen]
impl PushPayload {
    /// Construct a `GroupMessage` payload.
    #[wasm_bindgen(js_name = groupMessage)]
    pub fn group_message(
        group_pseudonym: &[u8],
        envelope_pointer: u64,
    ) -> Result<PushPayload, JsError> {
        if group_pseudonym.len() != PSEUDONYM_LEN {
            return Err(JsError::new(&format!(
                "groupPseudonym must be {PSEUDONYM_LEN} bytes",
            )));
        }
        let mut id = [0u8; PSEUDONYM_LEN];
        id.copy_from_slice(group_pseudonym);
        Ok(Self {
            inner: RustPushPayload::GroupMessage {
                group_pseudonym: id,
                envelope_pointer,
            },
        })
    }

    /// Construct a `MailboxMessage` payload (recipient knows its own
    /// device pseudonym).
    #[wasm_bindgen(js_name = mailboxMessage)]
    pub fn mailbox_message(envelope_pointer: u64) -> PushPayload {
        Self {
            inner: RustPushPayload::MailboxMessage { envelope_pointer },
        }
    }

    /// Construct a generic `Wakeup` payload.
    #[wasm_bindgen]
    pub fn wakeup() -> PushPayload {
        Self {
            inner: RustPushPayload::Wakeup,
        }
    }

    /// Decode JSON received from the OS push pipeline.
    #[wasm_bindgen(js_name = decodeJson)]
    pub fn decode_json(bytes: &[u8]) -> Result<PushPayload, JsError> {
        let inner = RustPushPayload::decode_json(bytes).map_err(push_err)?;
        Ok(Self { inner })
    }

    /// Encode as compact JSON ready to ship through APNs / FCM.
    #[wasm_bindgen(js_name = encodeJson)]
    pub fn encode_json(&self) -> Vec<u8> {
        self.inner.encode_json()
    }

    /// Discriminator. `"GroupMessage" | "MailboxMessage" | "Wakeup"`.
    #[wasm_bindgen(getter)]
    pub fn kind(&self) -> String {
        match &self.inner {
            RustPushPayload::GroupMessage { .. } => "GroupMessage".into(),
            RustPushPayload::MailboxMessage { .. } => "MailboxMessage".into(),
            RustPushPayload::Wakeup => "Wakeup".into(),
        }
    }

    /// Group pseudonym bytes — present iff `kind == "GroupMessage"`.
    #[wasm_bindgen(getter, js_name = groupPseudonym)]
    pub fn group_pseudonym(&self) -> Option<Vec<u8>> {
        match &self.inner {
            RustPushPayload::GroupMessage {
                group_pseudonym, ..
            } => Some(group_pseudonym.to_vec()),
            _ => None,
        }
    }

    /// Envelope pointer — present iff `kind != "Wakeup"`.
    #[wasm_bindgen(getter, js_name = envelopePointer)]
    pub fn envelope_pointer(&self) -> Option<u64> {
        match &self.inner {
            RustPushPayload::GroupMessage {
                envelope_pointer, ..
            }
            | RustPushPayload::MailboxMessage { envelope_pointer } => Some(*envelope_pointer),
            RustPushPayload::Wakeup => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_message_roundtrip() {
        let p = PushPayload::group_message(&[0xAA; 32], 1234).unwrap();
        assert_eq!(p.kind(), "GroupMessage");
        assert_eq!(p.envelope_pointer(), Some(1234));
        assert_eq!(p.group_pseudonym().as_deref(), Some(&[0xAA; 32][..]));

        let bytes = p.encode_json();
        let decoded = PushPayload::decode_json(&bytes).unwrap();
        assert_eq!(decoded.kind(), "GroupMessage");
        assert_eq!(decoded.envelope_pointer(), Some(1234));
        assert_eq!(decoded.group_pseudonym().as_deref(), Some(&[0xAA; 32][..]));
    }

    #[test]
    fn mailbox_message_roundtrip() {
        let p = PushPayload::mailbox_message(99);
        let decoded = PushPayload::decode_json(&p.encode_json()).unwrap();
        assert_eq!(decoded.kind(), "MailboxMessage");
        assert_eq!(decoded.envelope_pointer(), Some(99));
        assert!(decoded.group_pseudonym().is_none());
    }

    #[test]
    fn wakeup_roundtrip() {
        let decoded = PushPayload::decode_json(&PushPayload::wakeup().encode_json()).unwrap();
        assert_eq!(decoded.kind(), "Wakeup");
        assert!(decoded.envelope_pointer().is_none());
        assert!(decoded.group_pseudonym().is_none());
    }

    // Error-path tests live in `lakko-push` itself — invoking
    // `JsError::new` from a host `cargo test` panics on the
    // wasm-bindgen import stub. The roundtrip tests above are the
    // shape-regression sentinel.
}
