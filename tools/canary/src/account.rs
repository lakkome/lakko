// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The live form of one canary account: its account identity, its device
//! identity bound to it, and its copy of the one-to-one group, rebuilt from
//! the state file at the start of a run.

use lakko_ds_proof::{DsProofSigner, ACCESS_KEY_LABEL};
use lakko_mls::{Group, Identity};
use lakko_mnemonic::Mnemonic;

use crate::error::{Error, Result};
use crate::state::{b64, decode_fixed, unb64, AccountState};

/// One account at run time.
pub(crate) struct Account {
    /// The account identity (phrase-seeded, ADR-0017).
    pub(crate) identity: Identity,
    /// The device identity, bound to the account (ADR-0050).
    pub(crate) device: Identity,
    /// The device's copy of the one-to-one group.
    pub(crate) group: Group,
}

/// The account identity a recovery phrase seeds.
///
/// # Errors
///
/// [`Error::State`] when the phrase does not parse; [`Error::Mls`] when the
/// identity cannot be built.
pub(crate) fn account_identity(phrase: &str) -> Result<Identity> {
    let m = Mnemonic::from_phrase(phrase).map_err(|_| Error::State("phrase".to_owned()))?;
    let seed = m.to_seed();
    Identity::from_seed(seed.as_ref()).map_err(|_| Error::Mls("identity"))
}

impl Account {
    /// Rebuild from the state: identities from their seeds, the MLS provider
    /// from the snapshot, the group from the provider.
    ///
    /// # Errors
    ///
    /// [`Error::State`] / [`Error::Mls`] when anything stored does not load.
    pub(crate) fn load(st: &AccountState, group_id: &[u8]) -> Result<Self> {
        let account = account_identity(&st.phrase)?;
        let device_seed: [u8; 32] = decode_fixed(&st.device_seed_b64)?;
        let binding = unb64(&st.device_binding_b64)?;
        let device = Identity::from_seed_with_binding(&device_seed, &binding)
            .map_err(|_| Error::Mls("device-identity"))?;
        device
            .restore_storage(&unb64(&st.mls_snapshot_b64)?)
            .map_err(|_| Error::Mls("restore"))?;
        let group = Group::load(&device, group_id)
            .map_err(|_| Error::Mls("group-load"))?
            .ok_or(Error::Mls("group-missing"))?;
        Ok(Self {
            identity: account,
            device,
            group,
        })
    }

    /// The MLS snapshot as stored in the state file.
    ///
    /// # Errors
    ///
    /// [`Error::Mls`] when the provider cannot be read.
    pub(crate) fn snapshot_b64(&self) -> Result<String> {
        self.device
            .snapshot_storage()
            .map(|s| b64(&s))
            .map_err(|_| Error::Mls("snapshot"))
    }

    /// The DS access signer at the group's current epoch: the exporter
    /// derivation every client uses (`lakko-ffi-*` `dsAccessSigner`).
    ///
    /// # Errors
    ///
    /// [`Error::Mls`] when the exporter fails.
    pub(crate) fn ds_signer(&self) -> Result<DsProofSigner> {
        ds_signer(&self.group, &self.device)
    }
}

/// The DS access signer of `group` at its current epoch.
///
/// # Errors
///
/// [`Error::Mls`] when the exporter fails.
pub(crate) fn ds_signer(group: &Group, device: &Identity) -> Result<DsProofSigner> {
    let gid = group.group_id();
    let secret = group
        .export_secret(device, ACCESS_KEY_LABEL, &gid, 32)
        .map_err(|_| Error::Mls("exporter"))?;
    let seed: [u8; 32] = secret
        .as_slice()
        .try_into()
        .map_err(|_| Error::Mls("exporter-len"))?;
    Ok(DsProofSigner::for_group(group.epoch(), &seed))
}
