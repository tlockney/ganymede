//! Notebooks through Y documents and rooms.

use ganymede_documents::schema::{self, Roots};
use ganymede_documents::yrs::sync::{Awareness, Message, SyncMessage};
use ganymede_documents::yrs::updates::decoder::Decode;
use ganymede_documents::yrs::updates::encoder::Encode;
use ganymede_documents::yrs::{
    Array, Doc, GetString, Map, Out, ReadTxn, StateVector, Text, Transact, Update,
};
use ganymede_documents::{Room, SaveStatus, Target};
use ganymede_nbformat::Notebook;
use std::path::PathBuf;

fn fixture(name: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../ganymede-nbformat/tests/fixtures")
        .join(name);
    std::fs::read_to_string(p).unwrap()
}

const BYTE_IDENTICAL: &[&str] = &[
    "code_outputs.ipynb",
    "markdown_attachments.ipynb",
    "empty.ipynb",
    "unsplit_source.expected.ipynb",
    "v3.expected.ipynb",
];

/// A second document, synced from the room's full state as a client would
/// be after sync step 2.
fn client_of(room: &Room) -> Doc {
    let doc = Doc::new();
    doc.transact_mut()
        .apply_update(Update::decode_v1(&room.encode_state()).unwrap())
        .unwrap();
    doc
}

#[test]
fn fixtures_roundtrip_byte_identical_through_a_room_and_a_client() {
    for name in BYTE_IDENTICAL {
        let text = fixture(name);
        let nb = Notebook::from_str(&text).unwrap();
        let room = Room::notebook_room(&nb, name);
        let back = room.notebook().unwrap().unwrap();
        if std::env::var("DUMP").is_ok() {
            std::fs::write(format!("/tmp/claude-0/{name}.out"), back.to_file_string()).unwrap();
        }
        assert_eq!(back.to_file_string(), text, "{name} via the room");

        let client = client_of(&room);
        // A client sees numbers as JavaScript would; the room puts back
        // the ones it could not hold.
        let roots = Roots::get(&client);
        let baseline = schema::notebook_value(&nb);
        let back = schema::get_restoring(&client.transact(), &roots, Some(&baseline)).unwrap();
        assert_eq!(back.to_file_string(), text, "{name} via a synced client");
    }
}

#[test]
fn numbers_keep_their_kind() {
    let text = r#"{
 "cells": [],
 "metadata": {
  "a": 1,
  "b": 1.0,
  "c": 1e-05,
  "d": 2.5,
  "e": -3,
  "f": 4000000000,
  "g": 1e+16,
  "h": 0.1
 },
 "nbformat": 4,
 "nbformat_minor": 5
}
"#;
    let nb = Notebook::from_str(text).unwrap();
    let room = Room::notebook_room(&nb, "n.ipynb");
    let client = client_of(&room);
    let roots = Roots::get(&client);
    let back = schema::get(&client.transact(), &roots).unwrap();
    assert_eq!(back.to_file_string(), text);
}

#[test]
fn schema_matches_jupyter_ydoc() {
    let nb = Notebook::from_str(&fixture("code_outputs.ipynb")).unwrap();
    let room = Room::notebook_room(&nb, "x.ipynb");
    let doc = room.doc();
    let roots = Roots::get(doc);
    let txn = doc.transact();
    assert!(matches!(
        roots.meta.get(&txn, "metadata"),
        Some(Out::YMap(_))
    ));
    assert!(roots.meta.get(&txn, "nbformat").is_some());
    let Some(Out::YMap(cell)) = roots.cells.get(&txn, 0) else {
        panic!("cell is a map")
    };
    assert!(matches!(cell.get(&txn, "source"), Some(Out::YText(_))));
    assert!(matches!(cell.get(&txn, "metadata"), Some(Out::YMap(_))));
    assert_eq!(
        cell.get(&txn, "execution_state").unwrap().to_string(&txn),
        "idle"
    );
    let Some(Out::YArray(outputs)) = cell.get(&txn, "outputs") else {
        panic!("outputs is an array")
    };
    let Some(Out::YMap(stream)) = outputs.get(&txn, 0) else {
        panic!("output is a map")
    };
    assert!(matches!(stream.get(&txn, "text"), Some(Out::YText(_))));
    assert_eq!(
        roots.state.get(&txn, "path").unwrap().to_string(&txn),
        "x.ipynb"
    );
}

#[test]
fn identical_notebooks_load_to_identical_updates() {
    let nb = Notebook::from_str(&fixture("code_outputs.ipynb")).unwrap();
    // Two rooms built from one file share history, so a client of the
    // first that meets the second (after a server restart) merges without
    // duplicating cells.
    let doc = Doc::new();
    for _ in 0..2 {
        let u = schema::deterministic_update(&nb);
        doc.transact_mut()
            .apply_update(Update::decode_v1(&u).unwrap())
            .unwrap();
    }
    let roots = Roots::get(&doc);
    let baseline = schema::notebook_value(&nb);
    let back = schema::get_restoring(&doc.transact(), &roots, Some(&baseline)).unwrap();
    assert_eq!(back.to_file_string(), nb.to_file_string());
}

/// Run a sync handshake between a room and a client doc, returning frames
/// the room sent to others.
fn sync(room: &mut Room, conn: u64, client: &Doc) {
    let fx = room.connect(conn);
    for (_, frame) in fx.send {
        if let Ok(Message::Sync(SyncMessage::SyncStep1(sv))) = Message::decode_v1(&frame) {
            let update = client.transact().encode_state_as_update_v1(&sv);
            room.receive(
                conn,
                &Message::Sync(SyncMessage::SyncStep2(update)).encode_v1(),
            )
            .unwrap();
        }
    }
    let sv = client.transact().state_vector();
    let fx = room
        .receive(conn, &Message::Sync(SyncMessage::SyncStep1(sv)).encode_v1())
        .unwrap();
    for (to, frame) in fx.send {
        assert_eq!(to, Target::One(conn));
        apply_frame(client, &frame);
    }
}

fn apply_frame(client: &Doc, frame: &[u8]) {
    match Message::decode_v1(frame).unwrap() {
        Message::Sync(SyncMessage::SyncStep2(u)) | Message::Sync(SyncMessage::Update(u)) => {
            client
                .transact_mut()
                .apply_update(Update::decode_v1(&u).unwrap())
                .unwrap();
        }
        _ => {}
    }
}

#[test]
fn edits_from_one_client_reach_the_other_and_the_file() {
    let nb = Notebook::from_str(&fixture("code_outputs.ipynb")).unwrap();
    let mut room = Room::notebook_room(&nb, "x.ipynb");
    let a = Doc::new();
    let b = Doc::new();
    sync(&mut room, 1, &a);
    sync(&mut room, 2, &b);
    assert!(!room.is_dirty());

    // Client a types at the start of the first cell.
    let before = a.transact().state_vector();
    {
        let roots = Roots::get(&a);
        let mut txn = a.transact_mut();
        let Some(Out::YMap(cell)) = roots.cells.get(&txn, 0) else {
            panic!()
        };
        let Some(Out::YText(src)) = cell.get(&txn, "source") else {
            panic!()
        };
        src.insert(&mut txn, 0, "# hi\n");
    }
    let update = a.transact().encode_state_as_update_v1(&before);
    let fx = room
        .receive(1, &Message::Sync(SyncMessage::Update(update)).encode_v1())
        .unwrap();
    assert!(fx.changed);
    assert!(room.is_dirty());
    for (to, frame) in fx.send {
        assert_eq!(to, Target::Others(1));
        apply_frame(&b, &frame);
    }
    let read = |d: &Doc| {
        let roots = Roots::get(d);
        let txn = d.transact();
        let Some(Out::YMap(cell)) = roots.cells.get(&txn, 0) else {
            panic!()
        };
        let Some(Out::YText(src)) = cell.get(&txn, "source") else {
            panic!()
        };
        src.get_string(&txn)
    };
    assert!(read(&b).starts_with("# hi\nimport sys"));
    let saved = room.notebook().unwrap().unwrap();
    assert!(saved
        .to_file_string()
        .contains("\"# hi\\n\",\n    \"import sys\\n\""));

    let generation = room.generation();
    let fx = room.saved(generation, "abc");
    assert!(!room.is_dirty());
    for (to, frame) in fx.send {
        assert_eq!(to, Target::All);
        apply_frame(&b, &frame);
    }
    let roots = Roots::get(&b);
    let txn = b.transact();
    assert_eq!(
        roots.state.get(&txn, "hash").unwrap().to_string(&txn),
        "abc"
    );
    assert_eq!(
        roots.state.get(&txn, "dirty").unwrap().to_string(&txn),
        "false"
    );
}

#[test]
fn reload_keeps_unchanged_cells_and_edits_changed_source() {
    let text = fixture("code_outputs.ipynb");
    let nb = Notebook::from_str(&text).unwrap();
    let mut room = Room::notebook_room(&nb, "x.ipynb");
    let client = client_of(&room);

    let mut changed = nb.clone();
    let ganymede_nbformat::Cell::Code(c) = &mut changed.cells[1] else {
        panic!()
    };
    *c.source.as_mut_string() = c.source.as_str().replace("range(3)", "range(4)");
    let fx = room.reload(&changed, "h2");
    assert!(!room.is_dirty());
    // Only a small update: no cell was replaced.
    let total: usize = fx.send.iter().map(|(_, f)| f.len()).sum();
    assert!(total < 200, "reload sent {total} bytes");
    for (_, frame) in &fx.send {
        apply_frame(&client, frame);
    }
    let back = room.notebook().unwrap().unwrap();
    assert_eq!(back.to_file_string(), changed.to_file_string());
    let roots = Roots::get(&client);
    let seen = schema::get(&client.transact(), &roots).unwrap();
    assert_eq!(
        schema::through_yjs(&schema::notebook_value(&seen)),
        schema::through_yjs(&schema::notebook_value(&back))
    );

    // Dropping, adding and reordering cells.
    let mut reordered = changed.clone();
    reordered.cells.reverse();
    reordered.cells.pop();
    let fx = room.reload(&reordered, "h3");
    for (_, frame) in &fx.send {
        apply_frame(&client, frame);
    }
    let back = room.notebook().unwrap().unwrap();
    assert_eq!(back.to_file_string(), reordered.to_file_string());
    let roots = Roots::get(&client);
    let seen = schema::get(&client.transact(), &roots).unwrap();
    assert_eq!(
        schema::through_yjs(&schema::notebook_value(&seen)),
        schema::through_yjs(&schema::notebook_value(&back))
    );
}

#[test]
fn set_text_makes_a_minimal_edit() {
    let doc = Doc::new();
    let text = doc.get_or_insert_text("t");
    let mut txn = doc.transact_mut();
    text.insert(&mut txn, 0, "héllo wörld");
    schema::set_text(&mut txn, &text, "héllo, wörld!");
    assert_eq!(text.get_string(&txn), "héllo, wörld!");
    schema::set_text(&mut txn, &text, "wörld");
    assert_eq!(text.get_string(&txn), "wörld");
    schema::set_text(&mut txn, &text, "");
    assert_eq!(text.get_string(&txn), "");
}

#[test]
fn save_message_is_parsed_and_answered() {
    use ganymede_documents::yrs::encoding::write::Write;
    use ganymede_documents::yrs::updates::encoder::{Encoder, EncoderV1};
    let nb = Notebook::new();
    let mut room = Room::notebook_room(&nb, "x.ipynb");
    let mut enc = EncoderV1::new();
    enc.write_var(2u32);
    enc.write_string("save");
    enc.write_var(7u64);
    let fx = room.receive(3, &enc.to_vec()).unwrap();
    assert_eq!(fx.save, Some((3, 7)));
    let reply = Room::save_reply(7, SaveStatus::Success);
    assert_eq!(reply[0], 2);
    let json: serde_json::Value = serde_json::from_slice(&reply[2..]).unwrap();
    assert_eq!(
        json,
        serde_json::json!({"type": "save", "responseTo": 7, "status": "success"})
    );
}

#[test]
fn awareness_is_relayed_and_cleared_on_disconnect() {
    let mut room = Room::transient();
    let peer = Awareness::new(Doc::new());
    peer.doc(); // keep the type used
    let mut peer = peer;
    peer.set_local_state_raw(r#"{"user":{"name":"a"}}"#);
    let update = peer.update().unwrap();
    room.connect(1);
    room.connect(2);
    let fx = room
        .receive(1, &Message::Awareness(update).encode_v1())
        .unwrap();
    assert_eq!(fx.send.len(), 1);
    assert_eq!(fx.send[0].0, Target::Others(1));
    let fx = room.disconnect(1);
    assert_eq!(fx.send.len(), 1);
    let Message::Awareness(gone) = Message::decode_v1(&fx.send[0].1).unwrap() else {
        panic!()
    };
    let entry = gone.clients.get(&peer.client_id()).unwrap();
    assert_eq!(&*entry.json, "null");
    // A transient room has no content to save.
    assert!(room.notebook().is_none());
    let _ = StateVector::default();
}
