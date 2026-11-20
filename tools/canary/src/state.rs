// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The canary's state file.
//!
//! It holds everything the two accounts need between runs: the service
//! bases, the credentials, the recovery phrases, the device seeds and
//! bindings, both MLS snapshots, B's journal cursor, the storage passes and
//! the step counters. It is a secret: written with mode `0600`, atomically
//! (temporary file, `fsync`, rename, `fsync` of the directory), so a crash
//! leaves either the old or the new file, never a torn one. Session tokens
//! are never written.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use serde::{Deserialize, Serialize};

use crate::endpoints::Endpoints;
use crate::error::{Error, Result};

/// Format version of the state file. Bumped on any incompatible change.
pub(crate) const STATE_VERSION: u32 = 1;

/// One canary account.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct AccountState {
    /// Canonical username (`canary_<10 hex>`); its login handle signs in.
    pub(crate) username: String,
    /// The account password.
    pub(crate) password: String,
    /// The 12-word recovery phrase: the account identity seed (ADR-0017).
    pub(crate) phrase: String,
    /// The device's 32-byte seed, base64.
    pub(crate) device_seed_b64: String,
    /// The device binding to the account key, base64 (ADR-0050).
    pub(crate) device_binding_b64: String,
    /// The device's MLS provider snapshot, base64.
    pub(crate) mls_snapshot_b64: String,
    /// When the account was created (client clock), unix seconds.
    pub(crate) created_at_unix: u64,
    /// The storage upload pass, when one is held.
    #[serde(default)]
    pub(crate) upload_pass: Option<Pass>,
    /// The storage read pass, when one is held.
    #[serde(default)]
    pub(crate) read_pass: Option<Pass>,
}

/// A storage pass and when it expires.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Pass {
    /// The opaque pass value (`p1.`...).
    pub(crate) value: String,
    /// Expiry, unix seconds.
    pub(crate) expires_at_unix: u64,
}

/// Renew a pass this many seconds before it expires.
pub(crate) const PASS_RENEW_MARGIN_SECS: u64 = 120;

impl Pass {
    /// Whether the pass can still be used at `now` (more than
    /// [`PASS_RENEW_MARGIN_SECS`] left).
    #[must_use]
    pub(crate) const fn usable_at(&self, now_unix: u64) -> bool {
        self.expires_at_unix > now_unix.saturating_add(PASS_RENEW_MARGIN_SECS)
    }
}

/// Per-step counters that survive a restart.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct StepCounters {
    /// Consecutive failures of each step, by step name.
    #[serde(default)]
    pub(crate) consecutive_failures: std::collections::BTreeMap<String, u64>,
}

/// The whole state file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct State {
    /// [`STATE_VERSION`].
    pub(crate) version: u32,
    /// Where requests go.
    pub(crate) endpoints: Endpoints,
    /// The one-to-one group id, base64.
    pub(crate) group_id_b64: String,
    /// Account A: sends the message, uploads, publishes call audio.
    pub(crate) a: AccountState,
    /// Account B: receives, downloads, subscribes.
    pub(crate) b: AccountState,
    /// B's journal cursor: the id of the last row B processed.
    pub(crate) b_cursor: u64,
    /// Completed runs, used for the call cadence.
    #[serde(default)]
    pub(crate) runs: u64,
    /// Step counters.
    #[serde(default)]
    pub(crate) counters: StepCounters,
}

impl State {
    /// The group id bytes.
    ///
    /// # Errors
    ///
    /// [`Error::State`] when the stored value is not 32 bytes of base64.
    pub(crate) fn group_id(&self) -> Result<[u8; 32]> {
        decode_fixed(&self.group_id_b64)
    }

    /// Read and parse a state file.
    ///
    /// # Errors
    ///
    /// [`Error::State`] when it cannot be read or parsed, or has another
    /// version.
    pub(crate) fn load(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path).map_err(|e| Error::State(format!("read: {}", e.kind())))?;
        let state: Self =
            serde_json::from_slice(&bytes).map_err(|_| Error::State("parse".to_owned()))?;
        if state.version != STATE_VERSION {
            return Err(Error::State("unknown version".to_owned()));
        }
        Ok(state)
    }

    /// Write the state atomically with mode `0600`.
    ///
    /// # Errors
    ///
    /// [`Error::State`] on any I/O failure.
    pub(crate) fn save(&self, path: &Path) -> Result<()> {
        let json =
            serde_json::to_vec_pretty(self).map_err(|_| Error::State("serialize".to_owned()))?;
        write_atomic(path, &json)
    }

    /// Write the state to a path that must not exist yet.
    ///
    /// # Errors
    ///
    /// [`Error::State`] when the file exists or on any I/O failure.
    pub(crate) fn create(&self, path: &Path) -> Result<()> {
        if path.exists() {
            return Err(Error::State("state file already exists".to_owned()));
        }
        self.save(path)
    }
}

/// Base64 of bytes.
#[must_use]
pub(crate) fn b64(bytes: &[u8]) -> String {
    B64.encode(bytes)
}

/// Decode base64 into a vector.
///
/// # Errors
///
/// [`Error::State`] when it is not base64.
pub(crate) fn unb64(s: &str) -> Result<Vec<u8>> {
    B64.decode(s).map_err(|_| Error::State("base64".to_owned()))
}

/// Decode base64 into a fixed-size array.
///
/// # Errors
///
/// [`Error::State`] when it is not base64 or has another length.
pub(crate) fn decode_fixed<const N: usize>(s: &str) -> Result<[u8; N]> {
    let v = unb64(s)?;
    <[u8; N]>::try_from(v.as_slice()).map_err(|_| Error::State("length".to_owned()))
}

fn temp_path(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .map(std::ffi::OsStr::to_os_string)
        .unwrap_or_default();
    name.push(".tmp");
    path.with_file_name(name)
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let io = |what: &str, e: &std::io::Error| Error::State(format!("{what}: {}", e.kind()));
    let tmp = temp_path(path);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(&tmp).map_err(|e| io("open", &e))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(|e| io("chmod", &e))?;
    }
    file.write_all(bytes).map_err(|e| io("write", &e))?;
    file.sync_all().map_err(|e| io("fsync", &e))?;
    drop(file);
    std::fs::rename(&tmp, path).map_err(|e| io("rename", &e))?;
    if let Some(dir) = path.parent() {
        let dir = if dir.as_os_str().is_empty() {
            Path::new(".")
        } else {
            dir
        };
        if let Ok(d) = std::fs::File::open(dir) {
            let _ = d.sync_all();
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account(name: &str) -> AccountState {
        AccountState {
            username: name.to_owned(),
            password: "pw".to_owned(),
            phrase: "a b c".to_owned(),
            device_seed_b64: b64(&[1; 32]),
            device_binding_b64: b64(&[2; 10]),
            mls_snapshot_b64: b64(&[3; 5]),
            created_at_unix: 1_700_000_000,
            upload_pass: Some(Pass {
                value: "p1.x".to_owned(),
                expires_at_unix: 10,
            }),
            read_pass: None,
        }
    }

    fn sample() -> State {
        let mut counters = StepCounters::default();
        counters.consecutive_failures.insert("message".into(), 2);
        State {
            version: STATE_VERSION,
            endpoints: Endpoints {
                auth: "http://a".into(),
                ds: "http://d".into(),
                storage: "http://s".into(),
                sfu: "ws://f".into(),
            },
            group_id_b64: b64(&[9; 32]),
            a: account("canary_a"),
            b: account("canary_b"),
            b_cursor: 42,
            runs: 7,
            counters,
        }
    }

    fn scratch(name: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("lakko-canary-test-{}-{name}", std::process::id()));
        p
    }

    #[test]
    fn state_round_trips_through_the_file() {
        let path = scratch("rt.json");
        let _ = std::fs::remove_file(&path);
        let s = sample();
        s.create(&path).unwrap();
        assert_eq!(State::load(&path).unwrap(), s);
        assert_eq!(s.group_id().unwrap(), [9; 32]);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        // create refuses to overwrite; save replaces.
        assert!(s.create(&path).is_err());
        let mut t = s;
        t.b_cursor = 43;
        t.save(&path).unwrap();
        assert_eq!(State::load(&path).unwrap().b_cursor, 43);
        assert!(!temp_path(&path).exists());
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn another_version_is_refused() {
        let path = scratch("ver.json");
        let mut s = sample();
        s.version = 99;
        s.save(&path).unwrap();
        assert!(State::load(&path).is_err());
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn a_pass_is_renewed_inside_the_margin() {
        let p = Pass {
            value: "p1".into(),
            expires_at_unix: 1_000,
        };
        assert!(p.usable_at(0));
        assert!(p.usable_at(879));
        assert!(!p.usable_at(880));
        assert!(!p.usable_at(2_000));
        assert!(!p.usable_at(u64::MAX));
    }
}
