// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! A whole directory, over an in-memory stand-in for the Delivery Service
//! that holds what the real one holds: the head, the edges, the chain, the
//! filter and the rows, as bytes.

#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;

use lakko_circle::{
    filter_header, member_epoch_key, open_filter, open_row, path_nodes, row_epoch, CircleError,
    DirectoryHead, DirectoryWrite, DirectoryWriter, EpochKey, FilterEntry, MemberGrant, Wrap,
    MAX_ROWS_PER_WRITE, ROW_LEN, WRAP_LEN,
};
use lakko_crypto::hash::blake3;
use proptest::prelude::*;

#[derive(Default)]
struct Server {
    version: u64,
    epoch: u64,
    read_pk: [u8; 32],
    head: Vec<u8>,
    edges: BTreeMap<u32, Wrap>,
    chain: BTreeMap<u64, Wrap>,
    filter: Option<Vec<u8>>,
    rows: BTreeMap<u32, Vec<u8>>,
}

impl Server {
    fn apply(&mut self, write: DirectoryWrite) {
        assert_eq!(write.version, self.version + 1);
        self.version = write.version;
        self.epoch = write.epoch;
        self.read_pk = write.read_pk;
        self.head = write.head.to_vec();
        if let Some(chain) = write.chain {
            self.chain.insert(write.epoch, chain);
        }
        for node in write.deletes {
            self.edges.remove(&node);
        }
        self.edges.extend(write.puts);
        if let Some(filter) = write.filter {
            self.filter = Some(filter);
        }
        for index in write.row_deletes {
            self.rows.remove(&index);
        }
        self.rows.extend(write.rows);
    }

    fn head(&self) -> DirectoryHead {
        DirectoryHead::decode(&self.head).unwrap()
    }

    /// What a member reads to reach the current key: the head and its path.
    fn path(&self, grant: &MemberGrant) -> Vec<(u32, Wrap)> {
        path_nodes(grant.leaf, self.head().capacity_log)
            .unwrap()
            .into_iter()
            .filter_map(|n| self.edges.get(&n).map(|w| (n, *w)))
            .collect()
    }

    fn current_key(&self, grant: &MemberGrant) -> Result<EpochKey, CircleError> {
        member_epoch_key(grant, &self.head(), &self.path(grant))
    }

    /// The key of an older epoch, walking down the chain.
    fn key_at(&self, from: &EpochKey, epoch: u64) -> EpochKey {
        let mut key = from.clone();
        while key.epoch() > epoch {
            key = key.previous(&self.chain[&key.epoch()]).unwrap();
        }
        key
    }

    /// A search: the filter, then the row.
    fn search(&self, grant: &MemberGrant, term: &[u8; 32]) -> Vec<Vec<u8>> {
        let current = self.current_key(grant).unwrap();
        let wire = self.filter.as_ref().unwrap();
        let filter_key = self.key_at(&current, filter_header(wire).unwrap().epoch);
        let filter = open_filter(&filter_key, wire).unwrap();
        filter
            .lookup(term)
            .into_iter()
            .map(|index| {
                let row = &self.rows[&index];
                let row_key = self.key_at(&current, row_epoch(row).unwrap());
                open_row(&row_key, index, row).unwrap()
            })
            .collect()
    }
}

fn term(name: &str) -> [u8; 32] {
    blake3(name.as_bytes())
}

fn setup() -> (DirectoryWriter, Server) {
    let mut writer = DirectoryWriter::create(&[0x51; 32]).unwrap();
    let mut server = Server::default();
    server.apply(writer.take_write().unwrap().unwrap());
    (writer, server)
}

fn flush(writer: &mut DirectoryWriter, server: &mut Server) {
    while let Some(write) = writer.take_write().unwrap() {
        server.apply(write);
    }
}

#[test]
fn the_worked_example() {
    // Bob and Katya are contacts, Katya and Anna are contacts. Bob finds Anna
    // through Katya's directory.
    let (mut katya, mut server) = setup();
    let bob = katya.add_member().unwrap();
    let anna_card = b"anna's card".to_vec();
    katya.put_row(0, anna_card.clone()).unwrap();
    katya
        .set_filter(
            vec![FilterEntry {
                tag: term("ann"),
                row: 0,
            }],
            1,
        )
        .unwrap();
    flush(&mut katya, &mut server);

    assert_eq!(server.search(&bob, &term("ann")), vec![anna_card]);
    assert!(server.search(&bob, &term("zed")).is_empty());
}

#[test]
fn a_removal_moves_nothing_but_the_path_and_the_key() {
    let (mut katya, mut server) = setup();
    let bob = katya.add_member().unwrap();
    let carol = katya.add_member().unwrap();
    katya.put_row(0, b"before".to_vec()).unwrap();
    katya
        .set_filter(
            vec![FilterEntry {
                tag: term("old"),
                row: 0,
            }],
            1,
        )
        .unwrap();
    flush(&mut katya, &mut server);
    let old_filter = server.filter.clone();
    let old_row = server.rows[&0].clone();
    let bob_key = server.current_key(&bob).unwrap();

    katya.remove_members(&[bob.leaf]).unwrap();
    let write = katya.take_write().unwrap().unwrap();
    assert!(write.chain.is_some());
    assert!(write.filter.is_none() && write.rows.is_empty());
    assert_eq!(write.puts.len(), usize::from(katya.capacity_log()));
    server.apply(write);

    // Nothing stored moved.
    assert_eq!(server.filter, old_filter);
    assert_eq!(server.rows[&0], old_row);
    // Bob's key is gone, Carol's is not.
    assert!(server.current_key(&bob).is_err());
    assert_eq!(
        server.search(&carol, &term("old")),
        vec![b"before".to_vec()]
    );
    assert_ne!(bob_key.read_public_key(), server.read_pk);

    // A row written after the removal is under a key Bob does not have, even
    // with the bytes in his hands.
    katya.put_row(1, b"after".to_vec()).unwrap();
    katya
        .set_filter(
            vec![
                FilterEntry {
                    tag: term("old"),
                    row: 0,
                },
                FilterEntry {
                    tag: term("new"),
                    row: 1,
                },
            ],
            2,
        )
        .unwrap();
    flush(&mut katya, &mut server);
    assert_eq!(
        open_row(&bob_key, 1, &server.rows[&1]).map(|_| ()),
        Err(CircleError::WrongEpoch)
    );
    assert_eq!(
        open_filter(&bob_key, server.filter.as_ref().unwrap()).map(|_| ()),
        Err(CircleError::WrongEpoch)
    );
    assert_eq!(server.search(&carol, &term("new")), vec![b"after".to_vec()]);
}

#[test]
fn a_contact_who_joined_later_opens_older_rows_through_the_chain() {
    let (mut katya, mut server) = setup();
    let bob = katya.add_member().unwrap();
    katya.put_row(0, b"from epoch one".to_vec()).unwrap();
    flush(&mut katya, &mut server);
    katya.remove_members(&[bob.leaf]).unwrap();
    flush(&mut katya, &mut server);
    let dora = katya.add_member().unwrap();
    katya.remove_members(&[]).unwrap_err();
    let erin = katya.add_member().unwrap();
    katya.remove_members(&[dora.leaf]).unwrap();
    flush(&mut katya, &mut server);
    assert_eq!(server.epoch, 3);

    let frank = katya.add_member().unwrap();
    flush(&mut katya, &mut server);
    assert_eq!(row_epoch(&server.rows[&0]).unwrap(), 1);
    for joined_later in [&erin, &frank] {
        let current = server.current_key(joined_later).unwrap();
        let old_key = server.key_at(&current, 1);
        assert_eq!(
            open_row(&old_key, 0, &server.rows[&0]).unwrap(),
            b"from epoch one"
        );
    }
}

#[test]
fn a_write_seals_under_the_epoch_it_ends_on() {
    // A card queued before a removal in the same write does not stay readable
    // by the person removed.
    let (mut katya, mut server) = setup();
    let bob = katya.add_member().unwrap();
    flush(&mut katya, &mut server);
    let bob_key = server.current_key(&bob).unwrap();

    katya.put_row(0, b"late".to_vec()).unwrap();
    katya.remove_members(&[bob.leaf]).unwrap();
    flush(&mut katya, &mut server);
    assert_eq!(row_epoch(&server.rows[&0]).unwrap(), 2);
    assert_eq!(
        open_row(&bob_key, 0, &server.rows[&0]).map(|_| ()),
        Err(CircleError::WrongEpoch)
    );
}

#[test]
fn one_write_moves_the_epoch_once() {
    let (mut katya, _server) = setup();
    let a = katya.add_member().unwrap();
    let b = katya.add_member().unwrap();
    katya.remove_members(&[a.leaf]).unwrap();
    assert_eq!(
        katya.remove_members(&[b.leaf]),
        Err(CircleError::WritePending)
    );
}

#[test]
fn a_large_change_goes_over_several_writes_and_the_filter_goes_last() {
    let (mut katya, mut server) = setup();
    let bob = katya.add_member().unwrap();
    let rows = u32::try_from(MAX_ROWS_PER_WRITE).unwrap() + 3;
    let mut entries = Vec::new();
    for row in 0..rows {
        katya.put_row(row, vec![0x10; 4]).unwrap();
        entries.push(FilterEntry {
            tag: blake3(&row.to_be_bytes()),
            row,
        });
    }
    katya.set_filter(entries, rows).unwrap();
    let first = katya.take_write().unwrap().unwrap();
    assert_eq!(first.rows.len(), MAX_ROWS_PER_WRITE);
    assert!(first.filter.is_none());
    server.apply(first);
    let second = katya.take_write().unwrap().unwrap();
    assert_eq!(second.rows.len(), 3);
    assert!(second.filter.is_some());
    server.apply(second);
    assert_eq!(katya.take_write().unwrap(), None);
    assert_eq!(server.head().filter_version, 1);
    assert_eq!(server.search(&bob, &blake3(&0u32.to_be_bytes())).len(), 1);
}

#[test]
fn a_restored_writer_continues() {
    let (mut katya, mut server) = setup();
    let bob = katya.add_member().unwrap();
    let carol = katya.add_member().unwrap();
    katya.remove_members(&[bob.leaf]).unwrap();
    flush(&mut katya, &mut server);
    let state = katya.encode_state();
    let mut again = DirectoryWriter::restore(&[0x51; 32], &state).unwrap();
    assert_eq!(again.encode_state(), state);
    assert_eq!(again.epoch_key(), katya.epoch_key());
    assert_eq!(again.members(), vec![carol.leaf]);
    assert_eq!(again.version(), server.version);
    let dora = again.add_member().unwrap();
    again.put_row(0, b"x".to_vec()).unwrap();
    flush(&mut again, &mut server);
    assert!(server.current_key(&dora).is_ok());
    assert!(server.current_key(&carol).is_ok());
    // Another seed restores nothing that works.
    let other = DirectoryWriter::restore(&[0x52; 32], &state).unwrap();
    assert_ne!(other.directory_id(), katya.directory_id());
}

#[test]
fn the_sizes_of_a_removal_measured() {
    // Run with --nocapture to read the numbers.
    for members in [150u32, 10_000] {
        let mut katya = DirectoryWriter::create(&[0x53; 32]).unwrap();
        let mut grants = Vec::new();
        let claim = katya.take_write().unwrap().unwrap();
        let mut stored_edges = claim.puts.len();
        for _ in 0..members {
            grants.push(katya.add_member().unwrap());
        }
        let growth = katya.take_write().unwrap().unwrap();
        stored_edges += growth.puts.len();
        let growth_body = growth.body_json().len();
        katya.remove_members(&[grants[3].leaf]).unwrap();
        let write = katya.take_write().unwrap().unwrap();
        let raw = write.puts.len() * (4 + WRAP_LEN) + WRAP_LEN + write.head.len() + 32;
        let path = usize::from(katya.capacity_log()) * (4 + WRAP_LEN) + write.head.len();
        println!(
            "{members} members: capacity 2^{}, {stored_edges} edges stored ({} bytes), \
             growth body {growth_body} bytes, removal {} wraps, {raw} bytes of payload, \
             {} bytes of JSON body; a member's path read {path} bytes of payload; \
             a row {ROW_LEN} bytes",
            katya.capacity_log(),
            stored_edges * WRAP_LEN,
            write.puts.len(),
            write.body_json().len(),
        );
        assert_eq!(write.puts.len(), usize::from(katya.capacity_log()));
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(24))]

    /// A body of JSON carries exactly the fields set, and stays valid JSON
    /// shape: the only strings in it are base64.
    #[test]
    fn a_body_names_its_version_and_epoch(rows in 0u32..8) {
        let (mut katya, mut server) = setup();
        for row in 0..rows {
            katya.put_row(row, vec![1, 2, 3]).unwrap();
        }
        if let Some(write) = katya.take_write().unwrap() {
            let body = write.body_json();
            let opening = format!("{{\"version\":{},", write.version);
            prop_assert!(body.starts_with(&opening));
            let quote_count = body.chars().filter(|c| *c == '"').count();
            prop_assert_eq!(quote_count % 2, 0);
            server.apply(write);
        }
    }
}
