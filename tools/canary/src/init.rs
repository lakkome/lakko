// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! `lakko-canary init`: create the two canary accounts and their one-to-one.
//!
//! Each account is created the way the web client creates one (proof of
//! work, OPAQUE registration of the password and of the recovery seed), its
//! login handle bound at once (an account with no handle and no passkey is
//! deleted after 24 h, `services/auth/src/unclaimed.rs`), and given one
//! device identity bound to its phrase-seeded account identity. A founds
//! one one-to-one group and adds B; the founding commit is never posted,
//! as on the web, and B joins from the Welcome in memory. A then claims the
//! DS journal with its root statement (ADR-0057), naming the access key of
//! the epoch the group stands at.

use std::path::Path;

use lakko_ds_proof::statement::{StatementBody, StatementSigner};
use lakko_mls::{group::encode_message, Group, GroupKind, Identity};
use lakko_mnemonic::Mnemonic;
use rand::RngCore as _;
use tracing::info;

use crate::account::{account_identity, ds_signer};
use crate::api::{nonce16, now_ms, now_secs, Api};
use crate::endpoints::{ds_group_path, Endpoints};
use crate::error::{Error, Result};
use crate::state::{b64, AccountState, State, StepCounters, STATE_VERSION};

struct NewAccount {
    state: AccountState,
    device: Identity,
    account: Identity,
}

fn random_bytes<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    rand::thread_rng().fill_bytes(&mut b);
    b
}

/// A UUIDv7-shaped account id: 48 bits of unix milliseconds, version 7,
/// variant `10`, the rest random.
fn account_id() -> [u8; 16] {
    let mut id = random_bytes::<16>();
    let ms = now_ms().to_be_bytes();
    id[..6].copy_from_slice(&ms[2..8]);
    id[6] = (id[6] & 0x0F) | 0x70;
    id[8] = (id[8] & 0x3F) | 0x80;
    id
}

async fn create_account(api: &Api) -> Result<NewAccount> {
    let username = format!("canary_{}", hex::encode(random_bytes::<5>()));
    let handle = lakko_contacts::username::login_handle(&username)
        .map_err(|_| Error::Usage("username".to_owned()))?;
    let password = b64(&random_bytes::<24>());
    let mnemonic = Mnemonic::generate();
    let phrase = mnemonic.phrase();
    let seed = mnemonic.to_seed();

    let session = api
        .signup(&account_id(), password.as_bytes(), seed.as_ref())
        .await?;
    api.bind_handle(&session, &handle).await?;

    let account = account_identity(&phrase)?;
    let device_seed = random_bytes::<32>();
    let device = Identity::from_seed_bound(&device_seed, &account.ed25519_seed_bytes(), now_secs())
        .map_err(|_| Error::Mls("device-identity"))?;
    let binding = device
        .device_binding()
        .ok_or(Error::Mls("binding"))?
        .to_vec();
    Ok(NewAccount {
        state: AccountState {
            username,
            password,
            phrase,
            device_seed_b64: b64(&device_seed),
            device_binding_b64: b64(&binding),
            mls_snapshot_b64: String::new(),
            created_at_unix: now_secs(),
            upload_pass: None,
            read_pass: None,
        },
        device,
        account,
    })
}

/// Run `init`: refuse an existing state file, create everything, write it.
///
/// # Errors
///
/// Any failure; nothing is written unless every step succeeded.
pub(crate) async fn run(endpoints: Endpoints, state_path: &Path) -> Result<()> {
    if state_path.exists() {
        return Err(Error::State("state file already exists".to_owned()));
    }
    let api = Api::new(endpoints.clone())?;
    let mut a = create_account(&api).await?;
    let mut b = create_account(&api).await?;
    info!("canary accounts created");

    let key_package = b
        .device
        .create_key_package()
        .map_err(|_| Error::Mls("key-package"))?;
    let mut group =
        Group::create(&a.device, GroupKind::OneToOne).map_err(|_| Error::Mls("group-create"))?;
    let added = group
        .add_members(&a.device, vec![key_package])
        .map_err(|_| Error::Mls("add-member"))?;
    let welcome = encode_message(&added.welcome).map_err(|_| Error::Mls("welcome-encode"))?;
    let group_b = Group::join_from_welcome(&welcome, &b.device).map_err(|_| Error::Mls("join"))?;
    let gid = group.group_id();
    if group_b.group_id() != gid || group_b.epoch() != group.epoch() {
        return Err(Error::Mls("join-mismatch"));
    }
    let gid32: [u8; 32] = gid
        .as_slice()
        .try_into()
        .map_err(|_| Error::Mls("group-id-len"))?;

    // ADR-0057: the root claims the journal at the epoch the group stands at.
    let founding = ds_signer(&group, &a.device)?;
    let root = StatementSigner::root(&a.account.ed25519_seed_bytes(), &gid32);
    let claim = root
        .sign_at(
            &gid32,
            StatementBody::Claim {
                epoch: founding.epoch(),
                epoch_key: founding.verifying_key().to_bytes(),
            },
            now_ms(),
            nonce16(),
        )
        .map_err(|_| Error::Crypto("claim-sign"))?;
    api.ds_claim(&ds_group_path(&gid, "claim"), claim.body_json())
        .await?;
    info!("canary journal claimed");

    a.state.mls_snapshot_b64 = b64(&a
        .device
        .snapshot_storage()
        .map_err(|_| Error::Mls("snapshot"))?);
    b.state.mls_snapshot_b64 = b64(&b
        .device
        .snapshot_storage()
        .map_err(|_| Error::Mls("snapshot"))?);
    let state = State {
        version: STATE_VERSION,
        endpoints,
        group_id_b64: b64(&gid),
        a: a.state,
        b: b.state,
        b_cursor: 0,
        runs: 0,
        counters: StepCounters::default(),
    };
    state.create(state_path)?;
    info!("canary state written");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_id_is_uuid_v7_shaped() {
        let id = account_id();
        assert_eq!(id[6] >> 4, 7);
        assert_eq!(id[8] >> 6, 0b10);
    }

    #[test]
    fn generated_usernames_are_canonical() {
        let name = format!("canary_{}", hex::encode(random_bytes::<5>()));
        assert_eq!(name.len(), 17);
        assert!(lakko_contacts::username::login_handle(&name).is_ok());
    }
}
