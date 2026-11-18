// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! A shared location (ADR-0092 section 6): a point in fixed point at 1e-7
//! degree, its accuracy, the fix time and an optional place name. No
//! floating point on the wire, no tiles, no geocoding.
//!
//! ```text
//! location = { "lat_e7": i32 in -900_000_000..=900_000_000,
//!              "lon_e7": i32 in -1_800_000_000..=1_800_000_000,
//!              "accuracy_m": u32 in 1..=1_000_000,
//!              "fix_at_ms": i64,
//!              "name": optional 1..=255 bytes of display text }
//! ```
//!
//! The JSON object is strict: an unknown field, a value of the wrong type or
//! out of its range refuses the whole location.

use serde::{Deserialize, Serialize};

use crate::content::{check_display_text, ContentError};

/// Largest absolute latitude, 1e-7 degree.
pub const MAX_LAT_E7: i32 = 900_000_000;
/// Largest absolute longitude, 1e-7 degree.
pub const MAX_LON_E7: i32 = 1_800_000_000;
/// Largest accuracy radius, metres (at least 1).
pub const MAX_ACCURACY_M: u32 = 1_000_000;
/// Longest place name, bytes (at least 1 when present).
pub const MAX_LOCATION_NAME_BYTES: usize = 255;

/// A shared location.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Location {
    /// Latitude, 1e-7 degree.
    pub lat_e7: i32,
    /// Longitude, 1e-7 degree.
    pub lon_e7: i32,
    /// Accuracy radius, metres.
    pub accuracy_m: u32,
    /// When the platform took the fix, milliseconds since the epoch.
    pub fix_at_ms: i64,
    /// The place name the person typed, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl Location {
    /// Check every bound.
    ///
    /// # Errors
    ///
    /// The first [`ContentError`] that applies.
    pub fn validate(&self) -> Result<(), ContentError> {
        if !(-MAX_LAT_E7..=MAX_LAT_E7).contains(&self.lat_e7) {
            return Err(ContentError::OutOfRange { field: "latitude" });
        }
        if !(-MAX_LON_E7..=MAX_LON_E7).contains(&self.lon_e7) {
            return Err(ContentError::OutOfRange { field: "longitude" });
        }
        if !(1..=MAX_ACCURACY_M).contains(&self.accuracy_m) {
            return Err(ContentError::OutOfRange { field: "accuracy" });
        }
        if let Some(name) = &self.name {
            check_display_text(name, 1, MAX_LOCATION_NAME_BYTES, "placeName")?;
        }
        Ok(())
    }

    /// Decode from the envelope's JSON value.
    ///
    /// # Errors
    ///
    /// [`ContentError::Malformed`] for a wrong shape, or a bound.
    pub fn from_json(value: serde_json::Value) -> Result<Self, ContentError> {
        let loc: Self = serde_json::from_value(value)
            .map_err(|_| ContentError::Malformed { what: "location" })?;
        loc.validate()?;
        Ok(loc)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn round_trip(
            lat in -MAX_LAT_E7..=MAX_LAT_E7,
            lon in -MAX_LON_E7..=MAX_LON_E7,
            acc in 1u32..=MAX_ACCURACY_M,
            at in any::<i64>(),
            name in proptest::option::of("[a-zA-Z é]{1,40}"),
        ) {
            let l = Location { lat_e7: lat, lon_e7: lon, accuracy_m: acc, fix_at_ms: at, name };
            l.validate().unwrap();
            let v = serde_json::to_value(&l).unwrap();
            prop_assert_eq!(Location::from_json(v).unwrap(), l);
        }

        #[test]
        fn out_of_range_is_refused(lat in any::<i32>(), lon in any::<i32>(), acc in any::<u32>()) {
            let l = Location { lat_e7: lat, lon_e7: lon, accuracy_m: acc, fix_at_ms: 0, name: None };
            let ok = (-MAX_LAT_E7..=MAX_LAT_E7).contains(&lat)
                && (-MAX_LON_E7..=MAX_LON_E7).contains(&lon)
                && (1..=MAX_ACCURACY_M).contains(&acc);
            prop_assert_eq!(l.validate().is_ok(), ok);
            let v = serde_json::to_value(&l).unwrap();
            prop_assert_eq!(Location::from_json(v).is_ok(), ok);
        }
    }

    #[test]
    fn strict_json_shape() {
        let ok = serde_json::json!({"lat_e7": 1, "lon_e7": 2, "accuracy_m": 3, "fix_at_ms": 4});
        assert!(Location::from_json(ok).is_ok());
        let extra =
            serde_json::json!({"lat_e7": 1, "lon_e7": 2, "accuracy_m": 3, "fix_at_ms": 4, "x": 1});
        assert!(Location::from_json(extra).is_err());
        let float =
            serde_json::json!({"lat_e7": 1.5, "lon_e7": 2, "accuracy_m": 3, "fix_at_ms": 4});
        assert!(Location::from_json(float).is_err());
        let empty_name = serde_json::json!({"lat_e7": 1, "lon_e7": 2, "accuracy_m": 3, "fix_at_ms": 4, "name": ""});
        assert!(Location::from_json(empty_name).is_err());
        let zero_acc =
            serde_json::json!({"lat_e7": 1, "lon_e7": 2, "accuracy_m": 0, "fix_at_ms": 4});
        assert!(Location::from_json(zero_acc).is_err());
    }
}
