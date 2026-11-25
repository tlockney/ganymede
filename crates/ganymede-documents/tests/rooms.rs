//! Notebooks through Y documents and rooms.

use ganymede_documents::schema::{self, Roots};
use ganymede_documents::yrs::sync::{Awareness, Message, SyncMessage};
use ganymede_documents::yrs::updates::decoder::Decode;
use ganymede_documents::yrs::updates::encoder::Encode;
use ganymede_documents::yrs::{
    Array, ArrayPrelim, Doc, GetString, Map, MapPrelim, MapRef, Out, ReadTxn, StateVector, Text,
    Transact, TransactionMut, Update,
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

/// The file changed on disk while a client's edit was unsaved: both
/// changes end up in the room, which stays dirty so the host saves the
/// merge.
#[test]
fn a_disk_change_merges_with_unsaved_edits() {
    let nb = Notebook::from_str(&fixture("code_outputs.ipynb")).unwrap();
    assert!(nb.cells.len() >= 2);
    let mut room = Room::notebook_room(&nb, "x.ipynb");
    let client = Doc::new();
    sync(&mut room, 1, &client);

    // The client edits the first cell.
    let before = client.transact().state_vector();
    {
        let roots = Roots::get(&client);
        let mut txn = client.transact_mut();
        let Some(Out::YMap(cell)) = roots.cells.get(&txn, 0) else {
            panic!()
        };
        let Some(Out::YText(src)) = cell.get(&txn, "source") else {
            panic!()
        };
        src.insert(&mut txn, 0, "# room\n");
    }
    let update = client.transact().encode_state_as_update_v1(&before);
    room.receive(1, &Message::Sync(SyncMessage::Update(update)).encode_v1())
        .unwrap();
    assert!(room.is_dirty());

    // Meanwhile another program changes the second cell on disk.
    let mut disk = nb.clone();
    let ganymede_nbformat::Cell::Code(c) = &mut disk.cells[1] else {
        panic!()
    };
    *c.source.as_mut_string() = format!("# disk\n{}", c.source.as_str());
    let (fx, conflicts) = room.merge_disk(&nb, &disk, "h2");
    assert!(conflicts.is_empty(), "{conflicts:?}");
    assert!(fx.changed);
    assert!(room.is_dirty(), "the merge still needs saving");
    for (_, frame) in &fx.send {
        apply_frame(&client, frame);
    }
    let merged = room.notebook().unwrap().unwrap();
    assert!(merged.cells[0]
        .source()
        .unwrap()
        .as_str()
        .starts_with("# room\n"));
    assert!(merged.cells[1]
        .source()
        .unwrap()
        .as_str()
        .starts_with("# disk\n"));
    let roots = Roots::get(&client);
    let seen = schema::get(&client.transact(), &roots).unwrap();
    assert_eq!(
        schema::through_yjs(&schema::notebook_value(&seen)),
        schema::through_yjs(&schema::notebook_value(&merged))
    );

    // Both sides change the same cell: the disk wins it.
    let mut disk2 = merged.clone();
    let ganymede_nbformat::Cell::Code(c) = &mut disk2.cells[0] else {
        panic!()
    };
    *c.source.as_mut_string() = "disk wins".into();
    let (_, conflicts) = room.merge_disk(&disk, &disk2, "h3");
    assert_eq!(conflicts.len(), 1, "{conflicts:?}");
    assert!(conflicts[0].starts_with("cell "));
    let after = room.notebook().unwrap().unwrap();
    assert_eq!(after.cells[0].source().unwrap().as_str(), "disk wins");

    // A clean room just reloads.
    let generation = room.generation();
    room.saved(generation, "h4");
    let (_, conflicts) = room.merge_disk(&disk2, &nb, "h5");
    assert!(conflicts.is_empty());
    assert!(!room.is_dirty());
    assert_eq!(
        room.notebook().unwrap().unwrap().to_file_string(),
        nb.to_file_string()
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

/// Outputs written by the server reach a client with small updates: a
/// growing stream is appended to, kept outputs are not resent.
#[test]
fn set_outputs_appends_to_a_growing_stream() {
    let nb = Notebook::from_str(&fixture("code_outputs.ipynb")).unwrap();
    let mut room = Room::notebook_room(&nb, "x.ipynb");
    let client = client_of(&room);
    let id = nb.cells[1].id().unwrap().to_string();
    let stream =
        |text: &str| serde_json::json!({"name": "stdout", "output_type": "stream", "text": text});

    let (_, fx) = room.edit(|txn, roots| {
        let cell = schema::find_cell(txn, roots, &id).unwrap();
        schema::set_outputs(txn, &cell, vec![stream("one\n")]);
    });
    assert!(fx.changed);
    for (_, f) in &fx.send {
        apply_frame(&client, f);
    }
    let big = "x".repeat(2000);
    let (_, fx) = room.edit(|txn, roots| {
        let cell = schema::find_cell(txn, roots, &id).unwrap();
        schema::set_outputs(txn, &cell, vec![stream(&format!("one\n{big}"))]);
    });
    let total: usize = fx.send.iter().map(|(_, f)| f.len()).sum();
    assert!(total < big.len() + 200, "sent {total} bytes");
    for (_, f) in &fx.send {
        apply_frame(&client, f);
    }
    let result = serde_json::json!({"data": {"text/plain": "2"}, "execution_count": 1, "metadata": {}, "output_type": "execute_result"});
    let (_, fx) = room.edit(|txn, roots| {
        let cell = schema::find_cell(txn, roots, &id).unwrap();
        schema::set_outputs(
            txn,
            &cell,
            vec![stream(&format!("one\n{big}")), result.clone()],
        );
    });
    let total: usize = fx.send.iter().map(|(_, f)| f.len()).sum();
    assert!(total < 500, "sent {total} bytes");
    for (_, f) in &fx.send {
        apply_frame(&client, f);
    }
    let roots = Roots::get(&client);
    let seen = schema::get(&client.transact(), &roots).unwrap();
    let ganymede_nbformat::Cell::Code(c) = &seen.cells[1] else {
        panic!()
    };
    assert_eq!(c.outputs.len(), 2);
    let v = serde_json::to_value(&c.outputs).unwrap();
    assert_eq!(v[0]["text"], format!("one\n{big}"));
    assert_eq!(v[1]["data"]["text/plain"], "2");

    // Cleared.
    let (_, fx) = room.edit(|txn, roots| {
        let cell = schema::find_cell(txn, roots, &id).unwrap();
        schema::set_outputs(txn, &cell, vec![]);
    });
    for (_, f) in &fx.send {
        apply_frame(&client, f);
    }
    let seen = schema::get(&client.transact(), &roots).unwrap();
    let ganymede_nbformat::Cell::Code(c) = &seen.cells[1] else {
        panic!()
    };
    assert!(c.outputs.is_empty());
}

/// `append_outputs` merges a stream into the stream before it, keeps
/// other outputs separate, and its cost doesn't grow with what the cell
/// already shows.
#[test]
fn append_outputs_merges_streams_and_stays_small() {
    let nb = Notebook::from_str(&fixture("code_outputs.ipynb")).unwrap();
    let mut room = Room::notebook_room(&nb, "x.ipynb");
    let client = client_of(&room);
    let id = nb.cells[1].id().unwrap().to_string();
    let out = |name: &str, text: &str| serde_json::json!({"name": name, "output_type": "stream", "text": text});
    let edit = |room: &mut Room, outputs: Vec<serde_json::Value>| {
        let (_, fx) = room.edit(|txn, roots| {
            let cell = schema::find_cell(txn, roots, &id).unwrap();
            schema::append_outputs(txn, &cell, outputs);
        });
        for (_, f) in &fx.send {
            apply_frame(&client, f);
        }
        fx.send.iter().map(|(_, f)| f.len()).sum::<usize>()
    };
    let (_, fx) = room.edit(|txn, roots| {
        let cell = schema::find_cell(txn, roots, &id).unwrap();
        schema::set_outputs(txn, &cell, vec![]);
    });
    for (_, f) in &fx.send {
        apply_frame(&client, f);
    }
    let big = "x".repeat(50_000);
    edit(&mut room, vec![out("stdout", &big)]);
    // A line appended to a long stream sends about a line.
    let sent = edit(&mut room, vec![out("stdout", "tail\n")]);
    assert!(sent < 200, "sent {sent} bytes");
    let result = serde_json::json!({"data": {"text/plain": "2"}, "execution_count": 1,
        "metadata": {}, "output_type": "execute_result"});
    edit(
        &mut room,
        vec![
            out("stderr", "warn\n"),
            out("stderr", "more\n"),
            result,
            out("stdout", "after\n"),
        ],
    );

    let roots = Roots::get(&client);
    let seen = schema::get(&client.transact(), &roots).unwrap();
    let ganymede_nbformat::Cell::Code(c) = &seen.cells[1] else {
        panic!()
    };
    let v = serde_json::to_value(&c.outputs).unwrap();
    assert_eq!(c.outputs.len(), 4, "{v}");
    assert_eq!(v[0]["text"], format!("{big}tail\n"));
    assert_eq!(v[1]["name"], "stderr");
    assert_eq!(v[1]["text"], "warn\nmore\n");
    assert_eq!(v[2]["output_type"], "execute_result");
    assert_eq!(v[3]["text"], "after\n");
}

/// Another process owns the file (#272): a client's edit is undone for
/// everyone, the room stays clean, and the file's own changes still load.
#[test]
fn a_read_only_room_refuses_edits_and_takes_the_files() {
    let nb = Notebook::from_str(&fixture("code_outputs.ipynb")).unwrap();
    let text = nb.to_file_string();
    let mut room = Room::notebook_room(&nb, "x.ipynb");
    room.set_read_only(true);
    let a = Doc::new();
    let b = Doc::new();
    sync(&mut room, 1, &a);
    sync(&mut room, 2, &b);
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
    let original = read(&a);

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
    assert!(!fx.changed);
    assert!(!room.is_dirty());
    assert!(!fx.send.is_empty());
    for (to, frame) in fx.send {
        assert_eq!(to, Target::All);
        apply_frame(&a, &frame);
        apply_frame(&b, &frame);
    }
    assert_eq!(read(&a), original, "the sender's edit is undone");
    assert_eq!(read(&b), original);
    assert_eq!(room.notebook().unwrap().unwrap().to_file_string(), text);

    // Server-side edits don't make it dirty either.
    let (_, fx) = room.edit(|txn, roots| {
        roots.meta.insert(txn, "x", 1);
    });
    assert!(!fx.changed);
    assert!(!room.is_dirty());

    // The owner's save reaches it as a clean reload, with no conflict.
    let changed = Notebook::from_str(&text.replace("import sys", "import os")).unwrap();
    let (fx, conflicts) = room.merge_disk(&nb, &changed, "h2");
    assert!(conflicts.is_empty());
    for (_, frame) in fx.send {
        apply_frame(&a, &frame);
    }
    assert!(read(&a).contains("import os"));

    // Writable again once the room owns the file.
    room.set_read_only(false);
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
}

/// An update nesting a value a million levels deep (two bytes a level)
/// would overflow the stack if decoded. The room refuses it, tells the
/// sender as it does any update it can't apply, and keeps working.
#[test]
fn a_deeply_nested_update_is_refused() {
    let nb = Notebook::from_str(&fixture("code_outputs.ipynb")).unwrap();
    let mut room = Room::notebook_room(&nb, "n.ipynb");
    room.connect(1);
    let mut update = vec![1, 1, 99, 0, 8, 1, 1, b'm', 1];
    for _ in 0..1_000_000 {
        update.extend([117, 1]);
    }
    update.extend([126, 0]);
    let fx = room
        .receive(1, &Message::Sync(SyncMessage::Update(update)).encode_v1())
        .unwrap();
    assert!(!fx.changed);
    assert!(
        fx.send
            .iter()
            .any(|(t, m)| matches!(t, Target::One(1))
                && String::from_utf8_lossy(m).contains("conflict")),
        "the sender is told"
    );
    assert_eq!(
        room.notebook().unwrap().unwrap().to_file_string(),
        fixture("code_outputs.ipynb")
    );
}

/// Shared maps nested 20,000 deep (about 300 KB) would overflow the stack
/// when deleted, which yrs does recursively, or when the room turned its
/// document into a notebook. The room refuses the update instead, and the
/// sender's later delete of the tree is harmless.
#[test]
fn deeply_nested_shared_types_are_refused() {
    use ganymede_documents::depth::MAX_SHARED_DEPTH;
    use ganymede_documents::yrs::{MapPrelim, MapRef, WriteTxn};

    let text = fixture("empty.ipynb");
    let nb = Notebook::from_str(&text).unwrap();
    let mut room = Room::notebook_room(&nb, "n.ipynb");
    room.connect(1);
    let send_bytes = |room: &mut Room, update: Vec<u8>| {
        room.receive(1, &Message::Sync(SyncMessage::Update(update)).encode_v1())
            .unwrap()
    };
    let refused = |fx: &ganymede_documents::Effects| {
        fx.send.iter().any(|(t, m)| {
            matches!(t, Target::One(1)) && String::from_utf8_lossy(m).contains("conflict")
        })
    };

    // All at once. (This client's own document can't delete the tree
    // either, so it is only ever dropped.)
    let deep = client_of(&room);
    let sv = deep.transact().state_vector();
    {
        let mut txn = deep.transact_mut();
        let meta = txn.get_or_insert_map("meta");
        let mut m: MapRef = meta.insert(&mut txn, "metadata", MapPrelim::default());
        for _ in 0..20_000 {
            m = m.insert(&mut txn, "x", MapPrelim::default());
        }
    }
    let fx = send_bytes(&mut room, deep.transact().encode_state_as_update_v1(&sv));
    assert!(refused(&fx));
    assert_eq!(room.notebook().unwrap().unwrap().to_file_string(), text);

    // The sender then deletes its tree's top: the room never had it.
    let id = deep.client_id();
    let clock = sv.get(&id);
    let mut delete = vec![0, 1]; // no blocks; a delete set for one client
    for n in [id.get(), 1, u64::from(clock), 1] {
        let mut n = n;
        loop {
            let byte = (n & 0x7f) as u8;
            n >>= 7;
            if n == 0 {
                delete.push(byte);
                break;
            }
            delete.push(byte | 0x80);
        }
    }
    send_bytes(&mut room, delete);
    assert_eq!(room.notebook().unwrap().unwrap().to_file_string(), text);

    let client = client_of(&room);
    let send = |room: &mut Room, sv: &StateVector| {
        send_bytes(room, client.transact().encode_state_as_update_v1(sv))
    };

    // One level per update: accepted up to the limit, then refused.
    let mut m: MapRef = {
        let mut txn = client.transact_mut();
        let meta = txn.get_or_insert_map("meta");
        meta.insert(&mut txn, "metadata", MapPrelim::default())
    };
    let mut accepted = 2; // meta, metadata
    let mut sv = StateVector::default();
    for _ in 0..MAX_SHARED_DEPTH + 4 {
        let fx = send(&mut room, &sv);
        if refused(&fx) {
            break;
        }
        sv = client.transact().state_vector();
        let mut txn = client.transact_mut();
        m = m.insert(&mut txn, "x", MapPrelim::default());
        accepted += 1;
    }
    assert_eq!(accepted, MAX_SHARED_DEPTH + 1, "the update that went over");
    let back = room.notebook().unwrap().unwrap();
    let mut depth = 2;
    let mut v = &back.metadata;
    while let Some(next) = v.get("x").and_then(|x| x.as_object()) {
        v = next;
        depth += 1;
    }
    assert_eq!(depth, MAX_SHARED_DEPTH);

    // A notebook's own shapes (cells, outputs) are far from the limit.
    let nb = Notebook::from_str(&fixture("code_outputs.ipynb")).unwrap();
    let room = Room::notebook_room(&nb, "c.ipynb");
    let (_, deepest) = ganymede_documents::depth::shared_depths(&room.doc().transact());
    assert!(
        deepest <= ganymede_documents::depth::SCHEMA_DEPTH,
        "{deepest}"
    );
}

/// A deep tree that the same update also deletes, or overwrites, would be
/// deleted (recursively) while yrs applies it: the depth is checked from
/// the update's bytes, before anything is applied.
#[test]
fn a_deep_tree_created_and_deleted_in_one_update_is_refused() {
    use ganymede_documents::yrs::{merge_updates_v1, MapPrelim, MapRef, WriteTxn};

    let text = fixture("empty.ipynb");
    let nb = Notebook::from_str(&text).unwrap();
    let base = Room::notebook_room(&nb, "n.ipynb");

    // The tree, built on a client's copy (never deleted there: that client
    // would overflow too).
    let deep = client_of(&base);
    let sv = deep.transact().state_vector();
    {
        let mut txn = deep.transact_mut();
        let meta = txn.get_or_insert_map("meta");
        let mut m: MapRef = meta.insert(&mut txn, "metadata", MapPrelim::default());
        for _ in 0..20_000 {
            m = m.insert(&mut txn, "x", MapPrelim::default());
        }
    }
    let tree = deep.transact().encode_state_as_update_v1(&sv);

    fn varint(out: &mut Vec<u8>, mut n: u64) {
        loop {
            let byte = (n & 0x7f) as u8;
            n >>= 7;
            if n == 0 {
                out.push(byte);
                return;
            }
            out.push(byte | 0x80);
        }
    }
    let id = deep.client_id();
    let top = u64::from(sv.get(&id));
    let end = u64::from(deep.transact().state_vector().get(&id));

    // Its top deleted: a delete set naming the first item.
    let mut delete = vec![0, 1];
    for n in [id.get(), 1, top, 1] {
        varint(&mut delete, n);
    }
    // Or its key set again by a later item from the same client: a map
    // entry whose origin is the tree's top, holding the string "y".
    let mut overwrite = vec![1, 1];
    varint(&mut overwrite, id.get());
    varint(&mut overwrite, end);
    overwrite.push(0x80 | 8); // has an origin; Any content
    varint(&mut overwrite, id.get());
    varint(&mut overwrite, top);
    overwrite.extend([1, 119, 1, b'y']); // one Any: the string "y"
    overwrite.push(0); // no deletes

    for second in [delete, overwrite] {
        let update = merge_updates_v1([tree.as_slice(), second.as_slice()]).unwrap();
        let mut room = Room::notebook_room(&nb, "n.ipynb");
        room.connect(1);
        let fx = room
            .receive(1, &Message::Sync(SyncMessage::Update(update)).encode_v1())
            .unwrap();
        assert!(fx
            .send
            .iter()
            .any(|(t, m)| matches!(t, Target::One(1))
                && String::from_utf8_lossy(m).contains("conflict")));
        assert_eq!(room.notebook().unwrap().unwrap().to_file_string(), text);
    }
}

/// An update that hangs shared types on items the room doesn't have would
/// be held by yrs and land later at a depth nobody checked: refused.
#[test]
fn shared_types_on_missing_items_are_refused() {
    use ganymede_documents::yrs::{MapPrelim, MapRef, WriteTxn};

    let nb = Notebook::from_str(&fixture("empty.ipynb")).unwrap();
    let mut room = Room::notebook_room(&nb, "n.ipynb");
    room.connect(1);
    let client = client_of(&room);
    let sv0 = client.transact().state_vector();
    let m: MapRef = {
        let mut txn = client.transact_mut();
        txn.get_or_insert_map("meta")
            .insert(&mut txn, "metadata", MapPrelim::default())
    };
    let sv1 = client.transact().state_vector();
    {
        let mut txn = client.transact_mut();
        m.insert(&mut txn, "x", MapPrelim::default());
    }
    // The second update alone: its parent is in the first, never sent.
    let second = client.transact().encode_state_as_update_v1(&sv1);
    let fx = room
        .receive(1, &Message::Sync(SyncMessage::Update(second)).encode_v1())
        .unwrap();
    assert!(fx.send.iter().any(
        |(t, m)| matches!(t, Target::One(1)) && String::from_utf8_lossy(m).contains("conflict")
    ));
    // Both, in order: fine.
    let both = client.transact().encode_state_as_update_v1(&sv0);
    let fx = room
        .receive(1, &Message::Sync(SyncMessage::Update(both)).encode_v1())
        .unwrap();
    assert!(fx.changed);
}

/// Values inside the deepest shared type the room accepts are kept.
#[test]
fn values_at_the_depth_limit_are_kept() {
    use ganymede_documents::depth::MAX_SHARED_DEPTH;
    use ganymede_documents::yrs::{MapPrelim, MapRef, WriteTxn};

    let nb = Notebook::from_str(&fixture("empty.ipynb")).unwrap();
    let mut room = Room::notebook_room(&nb, "n.ipynb");
    room.connect(1);
    let client = client_of(&room);
    let sv = client.transact().state_vector();
    {
        let mut txn = client.transact_mut();
        let meta = txn.get_or_insert_map("meta");
        // meta is 1, metadata 2, then x up to the limit.
        let mut m: MapRef = meta.insert(&mut txn, "metadata", MapPrelim::default());
        for _ in 2..MAX_SHARED_DEPTH {
            m = m.insert(&mut txn, "x", MapPrelim::default());
        }
        m.insert(&mut txn, "leaf", "hello");
    }
    let update = client.transact().encode_state_as_update_v1(&sv);
    let fx = room
        .receive(1, &Message::Sync(SyncMessage::Update(update)).encode_v1())
        .unwrap();
    assert!(fx.changed);
    let back = room.notebook().unwrap().unwrap();
    let mut v = &back.metadata;
    while let Some(next) = v.get("x").and_then(|x| x.as_object()) {
        v = next;
    }
    assert_eq!(v.get("leaf"), Some(&serde_json::json!("hello")));
}

/// Hand-built v1 updates: one client's run of blocks, for shapes a yrs
/// document can't produce.
struct RawUpdate {
    client: u64,
    start: u32,
    blocks: Vec<Vec<u8>>,
}

fn varint(out: &mut Vec<u8>, mut n: u64) {
    loop {
        let byte = (n & 0x7f) as u8;
        n >>= 7;
        if n == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

enum RawParent<'a> {
    Root(&'a str),
    Type(u64, u32),
    Beside(Option<(u64, u32)>, Option<(u64, u32)>),
}

impl RawUpdate {
    fn new(client: u64, start: u32) -> Self {
        RawUpdate {
            client,
            start,
            blocks: Vec::new(),
        }
    }

    /// The clock of the next block.
    fn next(&self) -> u32 {
        self.start + self.blocks.len() as u32
    }

    /// An item of length 1: an array type, or a null.
    fn item(&mut self, parent: RawParent, array: bool) -> (u64, u32) {
        let id = (self.client, self.next());
        let content = if array { 7 } else { 8 };
        let mut b = Vec::new();
        match parent {
            RawParent::Root(name) => {
                b.push(content);
                b.push(1);
                varint(&mut b, name.len() as u64);
                b.extend(name.as_bytes());
            }
            RawParent::Type(c, k) => {
                b.push(content);
                b.push(0);
                varint(&mut b, c);
                varint(&mut b, u64::from(k));
            }
            RawParent::Beside(l, r) => {
                b.push(
                    content
                        | if l.is_some() { 0x80 } else { 0 }
                        | if r.is_some() { 0x40 } else { 0 },
                );
                for (c, k) in [l, r].into_iter().flatten() {
                    varint(&mut b, c);
                    varint(&mut b, u64::from(k));
                }
            }
        }
        if array {
            b.push(0); // TYPE_REFS_ARRAY
        } else {
            b.extend([1, 126]); // one Any: null
        }
        self.blocks.push(b);
        id
    }

    /// A garbage-collected clock.
    fn gc(&mut self) -> (u64, u32) {
        let id = (self.client, self.next());
        self.blocks.push(vec![0, 1]);
        id
    }

    fn bytes(&self) -> Vec<u8> {
        let mut out = vec![1];
        varint(&mut out, self.blocks.len() as u64);
        varint(&mut out, self.client);
        varint(&mut out, u64::from(self.start));
        for b in &self.blocks {
            out.extend(b);
        }
        out.push(0);
        out
    }
}

/// A chain of arrays under root `zz`, `depth` deep counting the root, with
/// a null in the deepest; returns the deepest array and the null.
fn raw_chain(u: &mut RawUpdate, depth: usize) -> ((u64, u32), (u64, u32)) {
    let mut a = u.item(RawParent::Root("zz"), true);
    for _ in 2..depth {
        a = u.item(RawParent::Type(a.0, a.1), true);
    }
    let null = u.item(RawParent::Type(a.0, a.1), false);
    (a, null)
}

fn conflict_for(fx: &ganymede_documents::Effects, conn: u64) -> bool {
    fx.send.iter().any(|(t, m)| {
        matches!(t, Target::One(c) if *c == conn) && String::from_utf8_lossy(m).contains("conflict")
    })
}

/// yrs takes an item's parent from its left origin only when that is an
/// item with a known parent; past a garbage-collected left origin it takes
/// the right one's. The depth check follows it: an array beside a
/// garbage-collected clock and a value deep in a chain is as deep as that
/// value, and the update is refused.
#[test]
fn a_parent_through_the_right_origin_is_counted() {
    use ganymede_documents::depth::{shared_depths, MAX_SHARED_DEPTH};

    let text = fixture("empty.ipynb");
    let nb = Notebook::from_str(&text).unwrap();

    // Within the limit, the scan's depth is what yrs builds.
    let mut ok = RawUpdate::new(7, 0);
    let (_, null) = raw_chain(&mut ok, 4);
    let gc = ok.gc();
    let x = ok.item(RawParent::Beside(Some(gc), Some(null)), true);
    ok.item(RawParent::Type(x.0, x.1), true);
    let mut room = Room::notebook_room(&nb, "n.ipynb");
    room.connect(1);
    let fx = room
        .receive(
            1,
            &Message::Sync(SyncMessage::Update(ok.bytes())).encode_v1(),
        )
        .unwrap();
    assert!(!conflict_for(&fx, 1));
    let (_, deepest) = shared_depths(&room.doc().transact());
    assert_eq!(deepest, 6, "zz, 2 arrays, the deepest, x, and one under x");

    // To the limit and past it.
    let mut deep = RawUpdate::new(7, 0);
    let (_, null) = raw_chain(&mut deep, MAX_SHARED_DEPTH);
    let gc = deep.gc();
    let mut x = deep.item(RawParent::Beside(Some(gc), Some(null)), true);
    for _ in 0..9 {
        x = deep.item(RawParent::Type(x.0, x.1), true);
    }
    let mut room = Room::notebook_room(&nb, "n.ipynb");
    room.connect(1);
    let fx = room
        .receive(
            1,
            &Message::Sync(SyncMessage::Update(deep.bytes())).encode_v1(),
        )
        .unwrap();
    assert!(conflict_for(&fx, 1));
    assert_eq!(room.notebook().unwrap().unwrap().to_file_string(), text);
}

/// The same through items already in the document: a sibling that isn't a
/// shared type could be anywhere (or garbage-collected, sending yrs to the
/// right origin), so it counts as deep as the document goes.
#[test]
fn a_document_sibling_that_isnt_a_type_counts_as_deepest() {
    use ganymede_documents::depth::MAX_SHARED_DEPTH;

    let nb = Notebook::from_str(&fixture("empty.ipynb")).unwrap();
    let mut room = Room::notebook_room(&nb, "n.ipynb");
    room.connect(1);
    // First: a chain to the limit with a null at the bottom, and a null
    // in a shallow array.
    let mut first = RawUpdate::new(7, 0);
    let (_, deep_null) = raw_chain(&mut first, MAX_SHARED_DEPTH);
    let shallow = first.item(RawParent::Root("yy"), true);
    let shallow_null = first.item(RawParent::Type(shallow.0, shallow.1), false);
    let fx = room
        .receive(
            1,
            &Message::Sync(SyncMessage::Update(first.bytes())).encode_v1(),
        )
        .unwrap();
    assert!(!conflict_for(&fx, 1));
    // Then: an array beside both nulls, with more under it.
    let mut second = RawUpdate::new(7, first.next());
    let mut x = second.item(RawParent::Beside(Some(shallow_null), Some(deep_null)), true);
    for _ in 0..9 {
        x = second.item(RawParent::Type(x.0, x.1), true);
    }
    let fx = room
        .receive(
            1,
            &Message::Sync(SyncMessage::Update(second.bytes())).encode_v1(),
        )
        .unwrap();
    assert!(conflict_for(&fx, 1));
}

/// yrs takes an item's parent from its right origin when the left origin
/// is a GC block, not an item. A chain of arrays 16 deep, an `Any` value
/// `s` in the deepest, a GC block `g`, then an array with origin `g` and
/// right origin `s` (so inside the deepest array, at 17), and arrays
/// chained under that: refused, whether `g` comes in the same update or
/// is already in the document.
#[test]
fn a_type_beside_a_gc_block_takes_its_depth_from_the_right() {
    use ganymede_documents::depth::MAX_SHARED_DEPTH;

    fn varint(out: &mut Vec<u8>, mut n: u64) {
        loop {
            let byte = (n & 0x7f) as u8;
            n >>= 7;
            if n == 0 {
                out.push(byte);
                return;
            }
            out.push(byte | 0x80);
        }
    }
    const CLIENT: u64 = 4242;
    // An array (shared type, type ref 0) whose parent is the type at `parent`.
    fn array_in(b: &mut Vec<u8>, parent: u64) {
        b.push(7);
        varint(b, 0); // parent by id
        varint(b, CLIENT);
        varint(b, parent);
        b.push(0);
    }
    // The blocks from clock 0: the chain under root "zz" (clocks 0..d-2,
    // the last at depth MAX_SHARED_DEPTH), `s`, then `g` (one clock).
    let deepest = MAX_SHARED_DEPTH as u64 - 2;
    let mut first = Vec::new();
    first.extend([7, 1, 2, b'z', b'z', 0]); // array in root "zz", depth 2
    for clock in 1..=deepest {
        array_in(&mut first, clock - 1);
    }
    let s = deepest + 1;
    first.push(8); // Any content
    varint(&mut first, 0);
    varint(&mut first, CLIENT);
    varint(&mut first, deepest);
    first.extend([1, 126]); // one Any: null
    let g = s + 1;
    first.extend([0, 1]); // GC, one clock
    let first_blocks = (deepest + 3) as usize;

    // From clock g + 1: the array beside `g`, and 9 more under it.
    let x = g + 1;
    let mut second = vec![7 | 0x80 | 0x40];
    for id in [g, s] {
        varint(&mut second, CLIENT);
        varint(&mut second, id);
    }
    second.push(0);
    for clock in x + 1..x + 10 {
        array_in(&mut second, clock - 1);
    }
    let second_blocks = 10;

    let update = |blocks: usize, clock: u64, body: &[u8]| {
        let mut u = vec![1];
        varint(&mut u, blocks as u64);
        varint(&mut u, CLIENT);
        varint(&mut u, clock);
        u.extend(body);
        u.push(0); // no deletes
        Message::Sync(SyncMessage::Update(u)).encode_v1()
    };
    let refused = |fx: &ganymede_documents::Effects| {
        fx.send.iter().any(|(t, m)| {
            matches!(t, Target::One(1)) && String::from_utf8_lossy(m).contains("conflict")
        })
    };
    let nb = Notebook::from_str(&fixture("empty.ipynb")).unwrap();

    // In one update.
    let mut room = Room::notebook_room(&nb, "n.ipynb");
    room.connect(1);
    let mut both = first.clone();
    both.extend(&second);
    let fx = room
        .receive(1, &update(first_blocks + second_blocks, 0, &both))
        .unwrap();
    assert!(refused(&fx), "one update");

    // `g` already in the document.
    let mut room = Room::notebook_room(&nb, "n.ipynb");
    room.connect(1);
    let fx = room.receive(1, &update(first_blocks, 0, &first)).unwrap();
    assert!(!refused(&fx), "the chain alone is within the limit");
    let fx = room.receive(1, &update(second_blocks, x, &second)).unwrap();
    assert!(refused(&fx), "g in the document");
}

fn varint_into(out: &mut Vec<u8>, mut n: u64) {
    loop {
        let byte = (n & 0x7f) as u8;
        n >>= 7;
        if n == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

fn refused_to(fx: &ganymede_documents::Effects, conn: u64) -> bool {
    fx.send.iter().any(|(t, m)| {
        matches!(t, Target::One(c) if *c == conn) && String::from_utf8_lossy(m).contains("conflict")
    })
}

/// An update whose clocks run past `u32::MAX`: yrs wraps the clock and
/// reads on, the scan can't follow it. Refused, so the value nested a
/// million deep after the wrap is never decoded.
#[test]
fn an_update_the_scan_cannot_follow_is_refused() {
    let nb = Notebook::from_str(&fixture("empty.ipynb")).unwrap();
    let mut room = Room::notebook_room(&nb, "n.ipynb");
    room.connect(1);
    let mut u = vec![1, 2];
    varint_into(&mut u, 99);
    varint_into(&mut u, u64::from(u32::MAX));
    u.extend([0, 1]); // GC, one clock: the clock wraps here
    u.extend([8, 1, 1, b'm', 1]); // Any content in root "m", one value
    for _ in 0..1_000_000 {
        u.extend([117, 1]);
    }
    u.push(126);
    u.push(0); // no deletes
    let fx = room
        .receive(1, &Message::Sync(SyncMessage::Update(u)).encode_v1())
        .unwrap();
    assert!(refused_to(&fx, 1));
}

/// An update that re-sends an item the document already has, claiming a
/// shallow parent for it: yrs keeps its own copy, so the depth of what
/// hangs below it comes from the document, not the re-sent copy.
#[test]
fn a_resent_item_does_not_set_its_depth() {
    use ganymede_documents::depth::MAX_SHARED_DEPTH;
    use ganymede_documents::yrs::{merge_updates_v1, Map, MapPrelim, MapRef, WriteTxn};

    let nb = Notebook::from_str(&fixture("empty.ipynb")).unwrap();
    let mut room = Room::notebook_room(&nb, "n.ipynb");
    room.connect(1);
    let client = client_of(&room);
    let id = client.client_id();
    let send = |room: &mut Room, update: Vec<u8>| {
        room.receive(1, &Message::Sync(SyncMessage::Update(update)).encode_v1())
            .unwrap()
    };

    // `meta` (1) / metadata (2) / x ... down to MAX_SHARED_DEPTH, all
    // accepted.
    let mut m: MapRef = {
        let mut txn = client.transact_mut();
        txn.get_or_insert_map("meta")
            .insert(&mut txn, "metadata", MapPrelim::default())
    };
    let mut last = 0;
    for _ in 3..=MAX_SHARED_DEPTH {
        last = client.transact().state_vector().get(&id);
        let mut txn = client.transact_mut();
        m = m.insert(&mut txn, "x", MapPrelim::default());
    }
    let all = client
        .transact()
        .encode_state_as_update_v1(&Default::default());
    let fx = send(&mut room, all);
    assert!(!refused_to(&fx, 1), "the chain is within the limit");

    // The deepest map re-sent as a type under root "z", merged with a map
    // nested under the real one.
    let mut fake = vec![1, 1];
    varint_into(&mut fake, id.get());
    varint_into(&mut fake, u64::from(last));
    fake.extend([7, 1, 1, b'z', 1, 0]);
    let sv = client.transact().state_vector();
    {
        let mut txn = client.transact_mut();
        m.insert(&mut txn, "x", MapPrelim::default());
    }
    let chain = client.transact().encode_state_as_update_v1(&sv);
    let update = merge_updates_v1([fake.as_slice(), chain.as_slice()]).unwrap();
    let fx = send(&mut room, update);
    assert!(refused_to(&fx, 1), "the re-sent copy set the depth");
}

/// What yrs would hold for later (items after a skip, waiting for the
/// skipped clocks) is never left to land unchecked: shared types behind a
/// skip are refused, and anything else held is dropped.
#[test]
fn nothing_held_back_lands_unchecked() {
    use ganymede_documents::depth::{shared_depths, MAX_SHARED_DEPTH};
    use ganymede_documents::yrs::{Doc, Transact, Update};

    const CLIENT: u64 = 99;
    let chain = |u: &mut Vec<u8>, from: u32, to: u32| {
        for k in from..to {
            u.extend([7, 0]); // array, parent by id
            varint_into(u, CLIENT);
            varint_into(u, u64::from(k - 1));
            u.push(0);
        }
    };
    let header = |blocks: u32| {
        let mut u = vec![1];
        varint_into(&mut u, u64::from(blocks));
        varint_into(&mut u, CLIENT);
        varint_into(&mut u, 0);
        u
    };
    let deepest = |room: &Room| {
        let doc = Doc::new();
        doc.transact_mut()
            .apply_update(Update::decode_v1(&room.encode_state()).unwrap())
            .unwrap();
        let (_, deepest) = shared_depths(&doc.transact());
        deepest
    };
    let nb = Notebook::from_str(&fixture("empty.ipynb")).unwrap();
    let m = 1_000;

    // A skip over clock 0, then a chain hung on it; then clock 0 itself.
    let mut room = Room::notebook_room(&nb, "n.ipynb");
    room.connect(1);
    let mut u1 = header(m + 1);
    u1.extend([10, 1]); // skip one clock
    chain(&mut u1, 1, m + 1);
    u1.push(0);
    let fx = room
        .receive(1, &Message::Sync(SyncMessage::Update(u1)).encode_v1())
        .unwrap();
    assert!(refused_to(&fx, 1), "a chain behind a skip");
    let mut u2 = header(1);
    u2.extend([7, 1, 2, b'z', b'z', 0, 0]); // an array in root "zz"
    room.receive(1, &Message::Sync(SyncMessage::Update(u2)).encode_v1())
        .unwrap();
    assert!(deepest(&room) <= MAX_SHARED_DEPTH);

    // A skip, then a plain value: no types, so it goes through, but yrs's
    // hold on it is dropped. The skipped clocks then arrive as a chain:
    // checked on its own, and refused.
    let mut room = Room::notebook_room(&nb, "n.ipynb");
    room.connect(1);
    let mut u1 = header(2);
    u1.push(10);
    varint_into(&mut u1, u64::from(m));
    u1.extend([8, 1, 2, b'z', b'z', 1, 126, 0]); // null in root "zz"
    room.receive(1, &Message::Sync(SyncMessage::Update(u1)).encode_v1())
        .unwrap();
    let txn = room.doc().transact();
    let held = ganymede_documents::yrs::ReadTxn::store(&txn).pending_update();
    assert!(held.is_none_or(|p| p.update.is_empty()), "nothing held");
    drop(txn);
    let mut u2 = header(m);
    u2.extend([7, 1, 2, b'z', b'z', 0]);
    chain(&mut u2, 1, m);
    u2.push(0);
    let fx = room
        .receive(1, &Message::Sync(SyncMessage::Update(u2)).encode_v1())
        .unwrap();
    assert!(refused_to(&fx, 1), "the chain filling the gap");
    assert!(deepest(&room) <= MAX_SHARED_DEPTH);
}

/// The backstop for what yrs holds: a plain value whose origin is an item
/// the room doesn't have is held by yrs, waiting for that item. It adds no
/// shared types, so the depth check lets it through, and the room drops
/// the hold.
#[test]
fn a_held_update_is_dropped() {
    use ganymede_documents::yrs::{ReadTxn, Transact};

    let nb = Notebook::from_str(&fixture("empty.ipynb")).unwrap();
    let mut room = Room::notebook_room(&nb, "n.ipynb");
    room.connect(1);
    let mut u = vec![1, 1];
    varint_into(&mut u, 99); // client 99
    varint_into(&mut u, 0); // clock 0
    u.push(0x80 | 8); // has an origin; Any content
    varint_into(&mut u, 77); // origin: client 77, clock 5, which the room lacks
    varint_into(&mut u, 5);
    u.extend([1, 126, 0]); // one Any: null; no deletes
    room.receive(1, &Message::Sync(SyncMessage::Update(u)).encode_v1())
        .unwrap();
    let txn = room.doc().transact();
    let held = txn.store().pending_update();
    assert!(held.is_none_or(|p| p.update.is_empty()), "nothing held");
}

/// The room credits a client's edit to the name in its awareness state, a
/// client that gave none to a stand-in, and a change from the file to the
/// file; a cell nobody changed has no author (#770).
#[test]
fn edits_are_credited_to_who_made_them() {
    use ganymede_documents::room::{FROM_FILE, UNNAMED};
    let nb = Notebook::from_str(&fixture("code_outputs.ipynb")).unwrap();
    let ids: Vec<String> = nb
        .cells
        .iter()
        .filter_map(|c| c.id().map(String::from))
        .collect();
    assert!(ids.len() >= 2, "the fixture has two cells with ids");
    let mut room = Room::notebook_room(&nb, "x.ipynb");
    let (a, b) = (Doc::new(), Doc::new());
    sync(&mut room, 1, &a);
    sync(&mut room, 2, &b);
    assert!(room.author(&ids[0]).is_none(), "loaded, not edited");

    // Client a names itself; b doesn't.
    let mut peer = Awareness::new(Doc::new());
    peer.set_local_state_raw(r#"{"user":{"name":"thomas","display_name":"Thomas"}}"#);
    room.receive(1, &Message::Awareness(peer.update().unwrap()).encode_v1())
        .unwrap();

    let type_into = |d: &Doc, index: u32, text: &str| {
        let before = d.transact().state_vector();
        {
            let roots = Roots::get(d);
            let mut txn = d.transact_mut();
            let Some(Out::YMap(cell)) = roots.cells.get(&txn, index) else {
                panic!()
            };
            let Some(Out::YText(src)) = cell.get(&txn, "source") else {
                panic!()
            };
            src.insert(&mut txn, 0, text);
        }
        Message::Sync(SyncMessage::Update(
            d.transact().encode_state_as_update_v1(&before),
        ))
        .encode_v1()
    };
    room.receive(1, &type_into(&a, 0, "# a\n")).unwrap();
    let who = room.author(&ids[0]).unwrap();
    assert_eq!((who.name.as_str(), who.person), ("Thomas", true));
    assert!(room.author(&ids[1]).is_none(), "only the cell a changed");

    room.receive(2, &type_into(&b, 1, "# b\n")).unwrap();
    assert_eq!(room.author(&ids[1]).unwrap().name, UNNAMED);

    // The file changes cell 1 again: credited to the file, not a person.
    let mut disk = room.notebook().unwrap().unwrap();
    let mut v = schema::notebook_value(&disk);
    v["cells"][1]["source"] = serde_json::json!("from disk");
    disk = Notebook::from_value(v).unwrap();
    room.reload(&disk, "h");
    let who = room.author(&ids[1]).unwrap();
    assert_eq!((who.name.as_str(), who.person), (FROM_FILE, false));
    assert_eq!(room.author(&ids[0]).unwrap().name, "Thomas", "unchanged");
}

/// `last_change` follows what attribution records: a client's edit to a
/// cell's source, a client deleting a cell, and a change from the file;
/// nothing else moves it, and `set_author` names who made the server's
/// edit only when that edit changed cells (#893).
#[test]
fn last_change_follows_attributed_changes() {
    use ganymede_documents::room::{FROM_FILE, UNNAMED};
    use ganymede_documents::Author;
    let nb = Notebook::from_str(&fixture("code_outputs.ipynb")).unwrap();
    let ids: Vec<String> = nb
        .cells
        .iter()
        .filter_map(|c| c.id().map(String::from))
        .collect();
    let mut room = Room::notebook_room(&nb, "x.ipynb");
    let (a, b) = (Doc::new(), Doc::new());
    sync(&mut room, 1, &a);
    sync(&mut room, 2, &b);
    assert!(room.last_change().is_none(), "loaded, not edited");
    let mut peer = Awareness::new(Doc::new());
    peer.set_local_state_raw(r#"{"user":{"display_name":"Thomas"}}"#);
    room.receive(1, &Message::Awareness(peer.update().unwrap()).encode_v1())
        .unwrap();
    let change = |room: &Room| {
        let (at, by) = room.last_change().unwrap();
        (at, by.name.clone(), by.person)
    };
    let update = |d: &Doc, f: &dyn Fn(&mut ganymede_documents::yrs::TransactionMut, &Roots)| {
        let before = d.transact().state_vector();
        {
            let roots = Roots::get(d);
            let mut txn = d.transact_mut();
            f(&mut txn, &roots);
        }
        Message::Sync(SyncMessage::Update(
            d.transact().encode_state_as_update_v1(&before),
        ))
        .encode_v1()
    };

    // A client types into a cell's source.
    let typed = update(&a, &|txn, roots| {
        let Some(Out::YMap(cell)) = roots.cells.get(txn, 0) else {
            panic!()
        };
        let Some(Out::YText(src)) = cell.get(txn, "source") else {
            panic!()
        };
        src.insert(txn, 0, "# a\n");
    });
    room.receive(1, &typed).unwrap();
    let (t1, who, person) = change(&room);
    assert_eq!((who.as_str(), person), ("Thomas", true));

    // Something that changes no cell's source or type leaves it alone.
    std::thread::sleep(std::time::Duration::from_millis(5));
    let (_, fx) = room.edit(|txn, roots| {
        let cell = schema::find_cell(txn, roots, &ids[1]).unwrap();
        cell.insert(txn, "execution_count", 3);
    });
    drop(fx);
    room.set_author(
        &[ids[1].clone()],
        &Author {
            name: "agent".into(),
            person: false,
        },
    );
    assert_eq!(change(&room).0, t1, "an output-only edit is not a change");
    assert_eq!(change(&room).1, "Thomas");

    // An edit that changes nothing, named by its caller, changes nothing.
    let (_, fx) = room.edit(|_, _| ());
    drop(fx);
    room.set_author(
        &[ids[0].clone()],
        &Author {
            name: "agent".into(),
            person: false,
        },
    );
    assert_eq!(change(&room), (t1, "Thomas".to_string(), true));

    // A client deletes a cell and changes nothing else.
    std::thread::sleep(std::time::Duration::from_millis(5));
    let gone = update(&b, &|txn, roots| roots.cells.remove(txn, 1));
    room.receive(2, &gone).unwrap();
    let (t2, who, person) = change(&room);
    assert!(t2 > t1);
    assert_eq!((who.as_str(), person), (UNNAMED, true));

    // An edit that changes a cell, then named by its caller: the agent's.
    std::thread::sleep(std::time::Duration::from_millis(5));
    let (_, fx) = room.edit(|txn, roots| {
        let cell = schema::find_cell(txn, roots, &ids[0]).unwrap();
        if let Some(Out::YText(src)) = cell.get(txn, "source") {
            src.insert(txn, 0, "by agent ");
        }
    });
    drop(fx);
    room.set_author(
        &[ids[0].clone()],
        &Author {
            name: "agent".into(),
            person: false,
        },
    );
    let (t3, who, person) = change(&room);
    assert!(t3 > t2);
    assert_eq!((who.as_str(), person), ("agent", false));

    // An agent deletes a cell, then names itself with the deleted id: the
    // change is still the agent's, though the room no longer holds it.
    std::thread::sleep(std::time::Duration::from_millis(5));
    let (_, fx) = room.edit(|txn, roots| {
        let at = schema::cell_index(txn, roots, &ids[2]).unwrap();
        roots.cells.remove(txn, at);
    });
    drop(fx);
    room.set_author(
        &[ids[2].clone()],
        &Author {
            name: "deleter".into(),
            person: false,
        },
    );
    let (t4, who, person) = change(&room);
    assert!(t4 > t3);
    assert_eq!((who.as_str(), person), ("deleter", false));

    // A server edit nobody claims, then an edit that changes nothing named
    // by its caller: the earlier change stays the server's.
    std::thread::sleep(std::time::Duration::from_millis(5));
    let (_, fx) = room.edit(|txn, roots| {
        let at = schema::cell_index(txn, roots, &ids[3]).unwrap();
        roots.cells.remove(txn, at);
    });
    drop(fx);
    let (t5, who, _) = change(&room);
    assert!(t5 > t4);
    assert_eq!(who, ganymede_documents::room::SERVER);
    let (_, fx) = room.edit(|_, _| ());
    drop(fx);
    room.set_author(
        &[ids[0].clone()],
        &Author {
            name: "latecomer".into(),
            person: false,
        },
    );
    assert_eq!(change(&room).1, ganymede_documents::room::SERVER);

    // The file changes a cell.
    std::thread::sleep(std::time::Duration::from_millis(5));
    let mut v = schema::notebook_value(&room.notebook().unwrap().unwrap());
    v["cells"][0]["source"] = serde_json::json!("from disk");
    room.reload(&Notebook::from_value(v).unwrap(), "h");
    let (t6, who, person) = change(&room);
    assert!(t6 > t5);
    assert_eq!((who.as_str(), person), (FROM_FILE, false));
}

/// A client the server knows by name (a tailnet login, #816) is credited
/// by that name, whatever its awareness state says.
#[test]
fn edits_are_credited_to_the_name_the_server_vouches_for() {
    let nb = Notebook::from_str(&fixture("code_outputs.ipynb")).unwrap();
    let id = nb.cells[0].id().unwrap().to_string();
    let mut room = Room::notebook_room(&nb, "x.ipynb");
    let a = Doc::new();
    room.connect_as(1, "me@example.com");
    sync(&mut room, 1, &a);
    let mut peer = Awareness::new(Doc::new());
    peer.set_local_state_raw(r#"{"user":{"name":"someone-else"}}"#);
    room.receive(1, &Message::Awareness(peer.update().unwrap()).encode_v1())
        .unwrap();
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
        src.insert(&mut txn, 0, "# a\n");
    }
    let update = Message::Sync(SyncMessage::Update(
        a.transact().encode_state_as_update_v1(&before),
    ))
    .encode_v1();
    room.receive(1, &update).unwrap();
    let who = room.author(&id).unwrap();
    assert_eq!((who.name.as_str(), who.person), ("me@example.com", true));
}

/// The server's own edit to a person's cell (a kernel through the host
/// API) takes the person's credit away; a deleted cell that comes back as
/// it was keeps its author; names are cleaned (#770).
#[test]
fn server_edits_and_undos_keep_credit_honest() {
    use ganymede_documents::room::{FROM_FILE, SERVER};
    use ganymede_documents::{clean_name, Author};
    let nb = Notebook::from_str(&fixture("code_outputs.ipynb")).unwrap();
    let ids: Vec<String> = nb
        .cells
        .iter()
        .filter_map(|c| c.id().map(String::from))
        .collect();
    let mut room = Room::notebook_room(&nb, "x.ipynb");
    let a = Doc::new();
    sync(&mut room, 1, &a);
    let mut peer = Awareness::new(Doc::new());
    peer.set_local_state_raw(r#"{"user":{"display_name":"Thomas\n@@changed forged"}}"#);
    room.receive(1, &Message::Awareness(peer.update().unwrap()).encode_v1())
        .unwrap();
    let send = |room: &mut Room,
                f: &dyn Fn(&mut ganymede_documents::yrs::TransactionMut, &Roots)| {
        let before = a.transact().state_vector();
        {
            let roots = Roots::get(&a);
            let mut txn = a.transact_mut();
            f(&mut txn, &roots);
        }
        let u = a.transact().encode_state_as_update_v1(&before);
        room.receive(1, &Message::Sync(SyncMessage::Update(u)).encode_v1())
            .unwrap();
    };
    // The person edits cell 0: theirs, with the name cleaned.
    send(&mut room, &|txn, roots| {
        let Some(Out::YMap(cell)) = roots.cells.get(txn, 0) else {
            panic!()
        };
        let Some(Out::YText(src)) = cell.get(txn, "source") else {
            panic!()
        };
        src.insert(txn, 0, "# mine\n");
    });
    let who = room.author(&ids[0]).unwrap().clone();
    assert_eq!(who.name, "Thomas @@changed forged");
    assert!(who.person && !who.name.contains('\n'));

    // The server rewrites it: no longer the person's.
    let (_, fx) = room.edit(|txn, roots| {
        let cell = schema::find_cell(txn, roots, &ids[0]).unwrap();
        let Some(Out::YText(src)) = cell.get(txn, "source") else {
            panic!()
        };
        schema::set_text(txn, &src, "@agent planted");
    });
    for (_, frame) in fx.send {
        apply_frame(&a, &frame);
    }
    let who = room.author(&ids[0]).unwrap();
    assert_eq!((who.name.as_str(), who.person), (SERVER, false));
    // Outputs only: nobody's credit moves.
    room.set_author(
        &ids[1..2],
        &Author {
            name: "x".into(),
            person: false,
        },
    );
    let (_, fx) = room.edit(|txn, roots| {
        let cell = schema::find_cell(txn, roots, &ids[1]).unwrap();
        cell.insert(txn, "execution_count", 7);
    });
    for (_, frame) in fx.send {
        apply_frame(&a, &frame);
    }
    assert_eq!(room.author(&ids[1]).unwrap().name, "x");

    // An agent's cell 1, deleted and put back as it was by the person:
    // still the agent's.
    let saved = {
        let roots = Roots::get(&a);
        let txn = a.transact();
        let Some(Out::YMap(cell)) = roots.cells.get(&txn, 1) else {
            panic!()
        };
        schema::out_to_json(&txn, &Out::YMap(cell))
    };
    send(&mut room, &|txn, roots| roots.cells.remove(txn, 1));
    assert!(room.author(&ids[1]).is_none());
    let back = saved.clone();
    send(&mut room, &move |txn, roots| {
        schema::insert_cell(txn, &roots.cells, 1, back.clone())
    });
    assert_eq!(
        room.author(&ids[1]).unwrap().name,
        "x",
        "undo keeps the author"
    );

    // A cell from the file nobody touched, deleted and put back by the
    // person: still the file's, not theirs.
    let last = ids.len() - 1;
    assert!(room.author(&ids[last]).is_none());
    let saved = {
        let roots = Roots::get(&a);
        let txn = a.transact();
        let Some(Out::YMap(cell)) = roots.cells.get(&txn, last as u32) else {
            panic!()
        };
        schema::out_to_json(&txn, &Out::YMap(cell))
    };
    send(&mut room, &move |txn, roots| {
        roots.cells.remove(txn, last as u32)
    });
    send(&mut room, &move |txn, roots| {
        schema::insert_cell(txn, &roots.cells, last as u32, saved.clone())
    });
    let who = room.author(&ids[last]).unwrap();
    assert_eq!((who.name.as_str(), who.person), (FROM_FILE, false));
    assert_eq!(clean_name("  a\u{7}b  c \n"), "ab c");
}

// The `metadata.ganymede.clean_run` stamp (#898).

const CLEAN_RUN_NB: &str = r##"{"cells":[
  {"cell_type":"markdown","id":"m","metadata":{},"source":["# title"]},
  {"cell_type":"code","id":"a","execution_count":1,"metadata":{},"outputs":[],"source":["x = 1"]},
  {"cell_type":"code","id":"b","execution_count":2,"metadata":{},"outputs":[],"source":["x + 1"]}],
  "metadata":{"ganymede":{"other":true}},"nbformat":4,"nbformat_minor":5}"##;

fn stamp() -> serde_json::Value {
    serde_json::json!({"at": "2026-11-20T10:00:00Z", "by": "claude-code", "kernel": "python3"})
}

fn stamp_in(doc: &Doc) -> Option<serde_json::Value> {
    let roots = Roots::get(doc);
    let stamp = schema::clean_run(&doc.transact(), &roots);
    stamp
}

fn stamp_of(room: &Room) -> Option<serde_json::Value> {
    stamp_in(room.doc())
}

/// A room holding the notebook with a stamp, and a client synced to it.
fn stamped() -> (Room, Doc) {
    let nb = Notebook::from_str(CLEAN_RUN_NB).unwrap();
    let mut room = Room::notebook_room(&nb, "n.ipynb");
    let client = Doc::new();
    sync(&mut room, 1, &client);
    let (_, fx) = room.edit(|txn, roots| schema::set_clean_run(txn, roots, Some(stamp())));
    for (_, frame) in fx.send {
        apply_frame(&client, &frame);
    }
    assert_eq!(stamp_of(&room), Some(stamp()));
    (room, client)
}

/// Client `client` (connection 1) makes `f` to its document and sends it
/// to `room`. The room's frames go back to the client.
fn client_change(
    room: &mut Room,
    client: &Doc,
    f: impl FnOnce(&mut ganymede_documents::yrs::TransactionMut, &Roots),
) -> ganymede_documents::Effects {
    let before = client.transact().state_vector();
    {
        let roots = Roots::get(client);
        let mut txn = client.transact_mut();
        f(&mut txn, &roots);
    }
    let update = client.transact().encode_state_as_update_v1(&before);
    let fx = room
        .receive(1, &Message::Sync(SyncMessage::Update(update)).encode_v1())
        .unwrap();
    for (to, frame) in &fx.send {
        if *to == Target::All {
            apply_frame(client, frame);
        }
    }
    fx
}

fn cell_map(txn: &impl ReadTxn, roots: &Roots, index: u32) -> ganymede_documents::yrs::MapRef {
    let Some(Out::YMap(cell)) = roots.cells.get(txn, index) else {
        panic!("cell {index}")
    };
    cell
}

fn type_into(index: u32) -> impl FnOnce(&mut ganymede_documents::yrs::TransactionMut, &Roots) {
    move |txn, roots| {
        let cell = cell_map(txn, roots, index);
        let Some(Out::YText(src)) = cell.get(txn, "source") else {
            panic!()
        };
        src.insert(txn, 0, "# ");
    }
}

#[test]
fn a_client_editing_a_cell_clears_the_clean_run_stamp() {
    // A code cell, and a Markdown one: any cell edit.
    for index in [1, 0] {
        let (mut room, client) = stamped();
        let fx = client_change(&mut room, &client, type_into(index));
        assert_eq!(stamp_of(&room), None, "cell {index}");
        assert!(fx.changed && room.is_dirty());
        // The removal reaches every client, the sender too.
        assert!(fx.send.iter().any(|(to, _)| *to == Target::All));
        assert_eq!(stamp_in(&client), None);
        // The rest of `ganymede` stays.
        let nb = room.notebook().unwrap().unwrap();
        assert_eq!(
            nb.metadata.get("ganymede"),
            Some(&serde_json::json!({"other": true}))
        );
    }
}

#[test]
fn a_client_running_a_cell_clears_the_clean_run_stamp() {
    let (mut room, client) = stamped();
    client_change(&mut room, &client, |txn, roots| {
        let cell = cell_map(txn, roots, 1);
        cell.insert(txn, "execution_count", 7);
    });
    assert_eq!(stamp_of(&room), None);
}

type CellChange = Box<dyn Fn(&mut ganymede_documents::yrs::TransactionMut, &Roots)>;

#[test]
fn adding_deleting_and_moving_a_cell_clear_the_clean_run_stamp() {
    let new_cell =
        serde_json::json!({"cell_type": "markdown", "id": "n", "metadata": {}, "source": "x"});
    let cases: Vec<(&str, CellChange)> = vec![
        (
            "add",
            Box::new(move |txn, roots| schema::insert_cell(txn, &roots.cells, 3, new_cell.clone())),
        ),
        ("delete", Box::new(|txn, roots| roots.cells.remove(txn, 0))),
        (
            "move",
            Box::new(|txn, roots| {
                // yjs has no move: re-insert cell b before a.
                let b = schema::out_to_json(txn, &Out::YMap(cell_map(txn, roots, 2)));
                roots.cells.remove(txn, 2);
                schema::insert_cell(txn, &roots.cells, 1, b);
            }),
        ),
    ];
    for (name, change) in cases {
        // From the client, and as a server edit.
        let (mut room, client) = stamped();
        client_change(&mut room, &client, |txn, roots| change(txn, roots));
        assert_eq!(stamp_of(&room), None, "{name} by a client");

        let (mut room, _client) = stamped();
        let (_, fx) = room.edit(|txn, roots| change(txn, roots));
        assert_eq!(stamp_of(&room), None, "{name} by the server");
        assert!(fx.changed);
    }
}

#[test]
fn a_server_edit_of_a_cell_clears_the_clean_run_stamp_for_every_client() {
    let (mut room, client) = stamped();
    let (_, fx) = room.edit(|txn, roots| {
        let cell = cell_map(txn, roots, 1);
        let Some(Out::YText(src)) = cell.get(txn, "source") else {
            panic!()
        };
        src.insert(txn, 0, "# ");
    });
    assert_eq!(stamp_of(&room), None);
    for (to, frame) in fx.send {
        assert_eq!(to, Target::All);
        apply_frame(&client, &frame);
    }
    assert_eq!(stamp_in(&client), None, "the clearing reached the client");
    assert!(room.is_dirty());
}

#[test]
fn changes_that_leave_the_run_standing_keep_the_clean_run_stamp() {
    // By the server.
    let (mut room, _client) = stamped();
    room.edit(|txn, roots| {
        let cell = cell_map(txn, roots, 1);
        schema::set_outputs(
            txn,
            &cell,
            vec![serde_json::json!({"output_type": "stream", "name": "stdout", "text": "hi\n"})],
        );
        cell.insert(txn, "execution_state", "running");
        let Some(Out::YMap(meta)) = cell.get(txn, "metadata") else {
            panic!()
        };
        meta.insert(txn, "tags", ganymede_documents::yrs::Any::from("x"));
        let Some(Out::YMap(nbmeta)) = roots.meta.get(txn, "metadata") else {
            panic!()
        };
        nbmeta.insert(txn, "title", "hello");
    });
    assert_eq!(stamp_of(&room), Some(stamp()), "server edits");

    // By a client.
    let (mut room, client) = stamped();
    let fx = client_change(&mut room, &client, |txn, roots| {
        let cell = cell_map(txn, roots, 2);
        cell.insert(txn, "execution_state", "running");
        let Some(Out::YMap(meta)) = cell.get(txn, "metadata") else {
            panic!()
        };
        meta.insert(txn, "collapsed", true);
        let Some(Out::YArray(outputs)) = cell.get(txn, "outputs") else {
            panic!()
        };
        schema::insert_output(
            txn,
            &outputs,
            0,
            serde_json::json!({"output_type": "stream", "name": "stdout", "text": "2\n"}),
        );
        let Some(Out::YMap(nbmeta)) = roots.meta.get(txn, "metadata") else {
            panic!()
        };
        nbmeta.insert(txn, "title", "hello");
    });
    assert!(fx.changed);
    assert_eq!(stamp_of(&room), Some(stamp()), "client edits");
    assert!(fx.send.iter().all(|(to, _)| *to == Target::Others(1)));
}

#[test]
fn writing_or_removing_the_stamp_does_not_clear_it_or_the_rest_of_the_metadata() {
    let nb = Notebook::from_str(CLEAN_RUN_NB).unwrap();
    let mut room = Room::notebook_room(&nb, "n.ipynb");
    assert_eq!(stamp_of(&room), None);
    // Stamping twice, with another stamp, never clears.
    for s in [
        stamp(),
        serde_json::json!({"at": "2026-11-21T00:00:00Z", "by": "x", "kernel": "k"}),
    ] {
        let (_, fx) = room.edit(|txn, roots| schema::set_clean_run(txn, roots, Some(s.clone())));
        assert!(fx.changed && room.is_dirty());
        assert_eq!(stamp_of(&room), Some(s));
    }
    let saved = room.notebook().unwrap().unwrap();
    assert_eq!(saved.metadata["ganymede"]["other"], true);
    assert!(saved.metadata["ganymede"]["clean_run"]["kernel"].is_string());

    room.edit(|txn, roots| schema::set_clean_run(txn, roots, None));
    assert_eq!(stamp_of(&room), None);
    assert_eq!(
        room.notebook().unwrap().unwrap().metadata["ganymede"],
        serde_json::json!({"other": true}),
        "other keys stay"
    );

    // A notebook with only the stamp loses the `ganymede` key with it.
    let nb = Notebook::from_str(&CLEAN_RUN_NB.replace(r#"{"other":true}"#, "{}")).unwrap();
    let mut room = Room::notebook_room(&nb, "n.ipynb");
    room.edit(|txn, roots| schema::set_clean_run(txn, roots, Some(stamp())));
    room.edit(|txn, roots| schema::set_clean_run(txn, roots, None));
    assert!(!room
        .notebook()
        .unwrap()
        .unwrap()
        .metadata
        .contains_key("ganymede"));
}

#[test]
fn the_clean_run_stamp_is_saved_with_the_notebook_and_kept_when_the_file_loads() {
    let (room, _client) = stamped();
    let text = room.notebook().unwrap().unwrap().to_file_string();
    assert!(text.contains(r#""clean_run""#), "{text}");
    // A file that holds a stamp opens with it, and nothing clears it
    // until a cell changes.
    let nb = Notebook::from_str(&text).unwrap();
    let mut reopened = Room::notebook_room(&nb, "n.ipynb");
    assert_eq!(stamp_of(&reopened), Some(stamp()));
    let client = Doc::new();
    sync(&mut reopened, 1, &client);
    assert_eq!(stamp_of(&reopened), Some(stamp()));
    client_change(&mut reopened, &client, type_into(1));
    assert_eq!(stamp_of(&reopened), None);
}

/// A client can write `metadata.ganymede` as a shared map, not a plain
/// value: the stamp goes in and out of it in place, keeps its other keys,
/// and takes the `ganymede` key with it when it was the only one (#898).
#[test]
fn the_stamp_goes_in_and_out_of_a_ganymede_shared_map() {
    use ganymede_documents::yrs::MapPrelim;
    let ganymede = |room: &Room| {
        room.notebook()
            .unwrap()
            .unwrap()
            .metadata
            .get("ganymede")
            .cloned()
    };
    for keys in [vec!["other"], vec![]] {
        let nb = Notebook::from_str(CLEAN_RUN_NB).unwrap();
        let mut room = Room::notebook_room(&nb, "n.ipynb");
        let client = Doc::new();
        sync(&mut room, 1, &client);
        // The client replaces `ganymede` with a shared map.
        let k = keys.clone();
        client_change(&mut room, &client, move |txn, roots| {
            let Some(Out::YMap(meta)) = roots.meta.get(txn, "metadata") else {
                panic!()
            };
            let g = meta.insert(txn, "ganymede", MapPrelim::default());
            for key in k {
                g.insert(txn, key, true);
            }
        });
        let Some(Out::YMap(_)) = ({
            let roots = Roots::get(room.doc());
            let txn = room.doc().transact();
            let Some(Out::YMap(m)) = roots.meta.get(&txn, "metadata") else {
                panic!()
            };
            m.get(&txn, "ganymede")
        }) else {
            panic!("ganymede is a shared map in the room")
        };

        room.edit(|txn, roots| schema::set_clean_run(txn, roots, Some(stamp())));
        assert_eq!(stamp_of(&room), Some(stamp()), "{keys:?}");
        let mut with = serde_json::Map::new();
        for key in &keys {
            with.insert((*key).into(), true.into());
        }
        with.insert("clean_run".into(), stamp());
        assert_eq!(ganymede(&room), Some(serde_json::Value::Object(with)));

        room.edit(|txn, roots| schema::set_clean_run(txn, roots, None));
        assert_eq!(stamp_of(&room), None);
        if keys.is_empty() {
            assert_eq!(ganymede(&room), None, "the emptied key goes");
        } else {
            assert_eq!(ganymede(&room), Some(serde_json::json!({"other": true})));
        }
    }
}

/// `CLEAN_RUN_NB` as a file that holds the stamp, with `change` applied to
/// its JSON.
fn stamped_file(change: impl FnOnce(&mut serde_json::Value)) -> Notebook {
    let mut v: serde_json::Value = serde_json::from_str(CLEAN_RUN_NB).unwrap();
    v["metadata"]["ganymede"]["clean_run"] = stamp();
    change(&mut v);
    Notebook::from_value(v).unwrap()
}

/// A change that makes the run stale, and ones that don't.
type FileChange = fn(&mut serde_json::Value);
const STALE: [(&str, FileChange); 3] = [
    ("source", |v| v["cells"][1]["source"] = "x = 2".into()),
    ("count", |v| v["cells"][2]["execution_count"] = 9.into()),
    ("delete", |v| {
        v["cells"].as_array_mut().unwrap().remove(0);
    }),
];
const STANDING: [(&str, FileChange); 3] = [
    ("outputs", |v| {
        v["cells"][1]["outputs"] = serde_json::json!([
            {"output_type": "stream", "name": "stdout", "text": "hi\n"}])
    }),
    ("cell metadata", |v| {
        v["cells"][1]["metadata"]["tags"] = serde_json::json!(["t"])
    }),
    ("notebook metadata", |v| v["metadata"]["title"] = "t".into()),
];

/// A file written by another program (a REST `PUT`, an editor) that
/// changes the cells but keeps the stamp it read clears the stamp, as a
/// change in the room would; a stamp of its own, or a change that leaves
/// the run standing, keeps it. Through `reload` for a room with no unsaved
/// edits (#898).
#[test]
fn a_file_changed_under_the_room_clears_the_clean_run_stamp_on_reload() {
    for (name, change) in STALE {
        let base = stamped_file(|_| {});
        let mut room = Room::notebook_room(&base, "n.ipynb");
        let client = Doc::new();
        sync(&mut room, 1, &client);
        let fx = room.reload(&stamped_file(change), "h1");
        assert_eq!(stamp_of(&room), None, "{name}");
        assert!(
            fx.changed && room.is_dirty(),
            "{name}: unsaved, to be saved"
        );
        // The clients are told, and told the room is unsaved.
        for (to, frame) in fx.send {
            assert_eq!(to, Target::All);
            apply_frame(&client, &frame);
        }
        assert_eq!(stamp_in(&client), None, "{name}: reached the client");
        let roots = Roots::get(&client);
        let txn = client.transact();
        assert_eq!(
            roots.state.get(&txn, "dirty").unwrap().to_string(&txn),
            "true",
            "{name}"
        );
        // Other keys of `ganymede` stay.
        assert_eq!(
            room.notebook().unwrap().unwrap().metadata["ganymede"],
            serde_json::json!({"other": true})
        );
    }
    // The file brought a new stamp along with its changes: its writer's.
    let base = stamped_file(|_| {});
    let mut room = Room::notebook_room(&base, "n.ipynb");
    let new = serde_json::json!({"at": "2026-12-01T00:00:00Z", "by": "x", "kernel": "k"});
    let n = new.clone();
    room.reload(
        &stamped_file(move |v| {
            v["cells"][1]["source"] = "x = 2".into();
            v["metadata"]["ganymede"]["clean_run"] = n;
        }),
        "h1",
    );
    assert_eq!(stamp_of(&room), Some(new));
    assert!(!room.is_dirty());
    // Changes that leave the run standing.
    for (name, change) in STANDING {
        let mut room = Room::notebook_room(&base, "n.ipynb");
        let fx = room.reload(&stamped_file(change), "h1");
        assert_eq!(stamp_of(&room), Some(stamp()), "{name}");
        assert!(!room.is_dirty() && !fx.changed, "{name}");
    }
    // A file without the stamp has none to clear, and the room has none
    // to keep: it is as the file says.
    let mut room = Room::notebook_room(&base, "n.ipynb");
    room.reload(
        &stamped_file(|v| {
            v["metadata"]["ganymede"]
                .as_object_mut()
                .unwrap()
                .remove("clean_run");
        }),
        "h1",
    );
    assert_eq!(stamp_of(&room), None);
    assert!(!room.is_dirty());
}

/// The same through `merge_disk`, for a room with unsaved edits (#898).
#[test]
fn a_file_changed_under_a_dirty_room_clears_the_clean_run_stamp_on_merge() {
    let base = stamped_file(|_| {});
    // A room with an unsaved edit that leaves the run standing.
    let dirty_room = || {
        let mut room = Room::notebook_room(&base, "n.ipynb");
        room.edit(|txn, roots| {
            let Some(Out::YMap(meta)) = roots.meta.get(txn, "metadata") else {
                panic!()
            };
            meta.insert(txn, "mine", true);
        });
        assert!(room.is_dirty() && stamp_of(&room).is_some());
        room
    };
    for (name, change) in STALE {
        let mut room = dirty_room();
        let (fx, conflicts) = room.merge_disk(&base, &stamped_file(change), "h1");
        assert!(conflicts.is_empty(), "{name}");
        assert_eq!(stamp_of(&room), None, "{name}");
        assert!(fx.changed && room.is_dirty(), "{name}");
        assert_eq!(
            room.notebook().unwrap().unwrap().metadata["mine"],
            true,
            "{name}: the room's own edit stays"
        );
    }
    // The file's own new stamp stays.
    let mut room = dirty_room();
    let new = serde_json::json!({"at": "2026-12-01T00:00:00Z", "by": "x", "kernel": "k"});
    let n = new.clone();
    room.merge_disk(
        &base,
        &stamped_file(move |v| {
            v["cells"][1]["source"] = "x = 2".into();
            v["metadata"]["ganymede"]["clean_run"] = n;
        }),
        "h1",
    );
    assert_eq!(stamp_of(&room), Some(new));
    for (name, change) in STANDING {
        let mut room = dirty_room();
        room.merge_disk(&base, &stamped_file(change), "h1");
        assert_eq!(stamp_of(&room), Some(stamp()), "{name}");
    }
}

/// `metadata` held as a plain value, or not at all, takes the stamp too,
/// keeping what it held (#898).
#[test]
fn the_stamp_goes_into_metadata_that_is_not_a_shared_map() {
    use ganymede_documents::yrs::Any;
    // As a plain value a client wrote.
    let nb = Notebook::from_str(CLEAN_RUN_NB).unwrap();
    let mut room = Room::notebook_room(&nb, "n.ipynb");
    let client = Doc::new();
    sync(&mut room, 1, &client);
    client_change(&mut room, &client, |txn, roots| {
        let plain =
            schema::json_to_any(&serde_json::json!({"title": "t", "ganymede": {"other": true}}));
        roots.meta.insert(txn, "metadata", plain);
    });
    let Some(Out::Any(Any::Map(_))) = ({
        let roots = Roots::get(room.doc());
        let txn = room.doc().transact();
        roots.meta.get(&txn, "metadata")
    }) else {
        panic!("metadata is a plain value in the room")
    };
    room.edit(|txn, roots| schema::set_clean_run(txn, roots, Some(stamp())));
    assert_eq!(stamp_of(&room), Some(stamp()));
    let meta = room.notebook().unwrap().unwrap().metadata;
    assert_eq!(meta["title"], "t");
    assert_eq!(meta["ganymede"]["other"], true);
    room.edit(|txn, roots| schema::set_clean_run(txn, roots, None));
    assert_eq!(stamp_of(&room), None);
    assert_eq!(
        room.notebook().unwrap().unwrap().metadata["ganymede"],
        serde_json::json!({"other": true})
    );

    // Not there at all.
    let mut room = Room::notebook_room(&nb, "n.ipynb");
    room.edit(|txn, roots| {
        roots.meta.remove(txn, "metadata");
    });
    assert_eq!(stamp_of(&room), None);
    room.edit(|txn, roots| schema::set_clean_run(txn, roots, Some(stamp())));
    assert_eq!(stamp_of(&room), Some(stamp()));
}

/// A read-only room, whose file another server owns, leaves the stamp as
/// the file has it: its owner decides, and the room must not become dirty
/// (#898).
#[test]
fn a_read_only_room_keeps_the_stamp_of_a_changed_file() {
    let base = stamped_file(|_| {});
    let mut room = Room::notebook_room(&base, "n.ipynb");
    room.set_read_only(true);
    let fx = room.reload(&stamped_file(STALE[0].1), "h1");
    assert_eq!(stamp_of(&room), Some(stamp()));
    assert!(!room.is_dirty() && !fx.changed);
}

/// The cell's id is part of what a run was of: a cell replaced by one with
/// the same type, source and count under a new id clears the stamp (#898).
#[test]
fn replacing_a_cell_with_a_copy_under_a_new_id_clears_the_clean_run_stamp() {
    let (mut room, client) = stamped();
    client_change(&mut room, &client, |txn, roots| {
        let mut cell = schema::out_to_json(txn, &Out::YMap(cell_map(txn, roots, 1)));
        cell["id"] = "a2".into();
        roots.cells.remove(txn, 1);
        schema::insert_cell(txn, &roots.cells, 1, cell);
    });
    assert_eq!(stamp_of(&room), None);
}

/// Removing a stamp that isn't there changes nothing: no edit, no update
/// to the clients, not dirty (#898).
#[test]
fn removing_a_stamp_that_is_not_there_changes_nothing() {
    let nb = Notebook::from_str(CLEAN_RUN_NB).unwrap();
    let mut room = Room::notebook_room(&nb, "n.ipynb");
    let (_, fx) = room.edit(|txn, roots| schema::set_clean_run(txn, roots, None));
    assert!(!fx.changed && !room.is_dirty());
    assert!(fx.send.is_empty());
}

/// A room with code cells `a` and `b`, each with an output, a count, and
/// metadata as a run through the room leaves it, and `execution_skipped`.
fn room_that_ran() -> Room {
    let cell = |id: &str| {
        serde_json::json!({"cell_type": "code", "id": id, "source": "1", "execution_count": 1,
            "outputs": [{"output_type": "stream", "name": "stdout", "text": "1\n"}],
            "metadata": {"tags": ["t"], "ganymede": {"keep": 1, "ran_source_hash": "77"}}})
    };
    let nb = Notebook::from_value(serde_json::json!({
        "nbformat": 4, "nbformat_minor": 5, "metadata": {}, "cells": [cell("a"), cell("b")]}))
    .unwrap();
    let mut room = Room::notebook_room(&nb, "x.ipynb");
    room.edit(|txn, roots| {
        for id in ["a", "b"] {
            let cell = schema::find_cell(txn, roots, id).unwrap();
            cell.insert(txn, "execution_skipped", "Cell changed while queued");
        }
    });
    room
}

/// Apply `edit` on `client` and send the room the update it makes.
fn client_edit(
    room: &mut Room,
    client: &Doc,
    edit: impl FnOnce(&mut TransactionMut, &Roots),
) -> ganymede_documents::Effects {
    let before = client.transact().state_vector();
    {
        let roots = Roots::get(client);
        let mut txn = client.transact_mut();
        edit(&mut txn, &roots);
    }
    let update = client.transact().encode_state_as_update_v1(&before);
    room.receive(1, &Message::Sync(SyncMessage::Update(update)).encode_v1())
        .unwrap()
}

/// Cell `id`'s metadata, and whether it has `execution_skipped`.
fn run_notes(doc: &Doc, id: &str) -> (serde_json::Value, bool) {
    let roots = Roots::get(doc);
    let txn = doc.transact();
    let cell = schema::find_cell(&txn, &roots, id).unwrap();
    let metadata = cell.get(&txn, "metadata").unwrap();
    (
        schema::out_to_json(&txn, &metadata),
        cell.contains_key(&txn, "execution_skipped"),
    )
}

fn kept() -> (serde_json::Value, bool) {
    let m = serde_json::json!({"tags": ["t"], "ganymede": {"keep": 1, "ran_source_hash": "77"}});
    (m, true)
}

fn cleared() -> (serde_json::Value, bool) {
    let m = serde_json::json!({"tags": ["t"], "ganymede": {"keep": 1}});
    (m, false)
}

#[test]
fn a_client_run_removes_the_ran_source_hash_and_the_skip_note() {
    let blank_count = |txn: &mut TransactionMut, roots: &Roots| {
        let cell = schema::find_cell(txn, roots, "a").unwrap();
        let null = schema::json_to_any(&serde_json::Value::Null);
        cell.insert(txn, "execution_count", null);
    };
    let append = |txn: &mut TransactionMut, roots: &Roots| {
        let cell = schema::find_cell(txn, roots, "a").unwrap();
        let out = serde_json::json!({"output_type": "stream", "name": "stdout", "text": "2\n"});
        schema::append_outputs(txn, &cell, vec![out]);
    };
    let clear = |txn: &mut TransactionMut, roots: &Roots| {
        let cell = schema::find_cell(txn, roots, "a").unwrap();
        schema::set_outputs(txn, &cell, Vec::new());
    };
    let replace = |txn: &mut TransactionMut, roots: &Roots| {
        let cell = schema::find_cell(txn, roots, "a").unwrap();
        cell.insert(txn, "outputs", ArrayPrelim::default());
    };
    type Edit<'a> = &'a dyn Fn(&mut TransactionMut, &Roots);
    // The stream's text grows in place, under the output.
    let edits: [(&str, Edit); 4] = [
        ("count", &blank_count),
        ("stream text", &append),
        ("outputs emptied", &clear),
        ("outputs replaced", &replace),
    ];
    for (what, edit) in edits {
        let mut room = room_that_ran();
        let (client, other) = (Doc::new(), Doc::new());
        sync(&mut room, 1, &client);
        sync(&mut room, 2, &other);
        let fx = client_edit(&mut room, &client, edit);
        assert!(fx.changed && room.is_dirty(), "{what}");
        assert_eq!(run_notes(room.doc(), "a"), cleared(), "{what}");
        assert_eq!(run_notes(room.doc(), "b"), kept(), "{what}: the other cell");
        // Both clients are sent the removal.
        for (to, frame) in &fx.send {
            apply_frame(&other, frame);
            if *to == Target::All {
                apply_frame(&client, frame);
            }
        }
        assert_eq!(run_notes(&client, "a"), cleared(), "{what}");
        assert_eq!(run_notes(&other, "a"), cleared(), "{what}");
    }
}

#[test]
fn a_client_change_that_is_no_run_keeps_the_ran_source_hash() {
    let mut room = room_that_ran();
    let client = Doc::new();
    sync(&mut room, 1, &client);
    // The count it has, a state, and a cell that goes with its outputs.
    client_edit(&mut room, &client, |txn, roots| {
        let cell = schema::find_cell(txn, roots, "a").unwrap();
        cell.insert(txn, "execution_count", 1);
        cell.insert(txn, "execution_state", "running");
    });
    assert_eq!(run_notes(room.doc(), "a"), kept());
    client_edit(&mut room, &client, |txn, roots| {
        let cell = schema::find_cell(txn, roots, "a").unwrap();
        schema::set_outputs(txn, &cell, Vec::new());
        roots.cells.remove(txn, 0);
    });
    assert_eq!(run_notes(room.doc(), "b"), kept());
    // A cell that comes back whole (an undo, a move) keeps its hash.
    client_edit(&mut room, &client, |txn, roots| {
        let cell = serde_json::json!({"cell_type": "code", "id": "a", "source": "1",
            "execution_count": 1, "outputs": [], "metadata": kept().0});
        schema::insert_cell(txn, &roots.cells, 0, cell);
    });
    assert_eq!(run_notes(room.doc(), "a").0, kept().0);
}

#[test]
fn the_servers_own_edit_of_outputs_keeps_the_ran_source_hash() {
    let mut room = room_that_ran();
    let client = Doc::new();
    sync(&mut room, 1, &client);
    room.edit(|txn, roots| {
        let cell = schema::find_cell(txn, roots, "a").unwrap();
        schema::set_outputs(txn, &cell, Vec::new());
        cell.insert(txn, "execution_count", 2);
    });
    assert_eq!(run_notes(room.doc(), "a"), kept());
    // Nor does it count towards the next client update.
    client_edit(&mut room, &client, |txn, roots| {
        let cell = schema::find_cell(txn, roots, "b").unwrap();
        cell.insert(txn, "execution_state", "running");
    });
    assert_eq!(run_notes(room.doc(), "a"), kept());
}

#[test]
fn a_file_change_keeps_the_ran_source_hash_the_file_has() {
    let mut room = room_that_ran();
    let mut file = serde_json::to_value(room.notebook().unwrap().unwrap()).unwrap();
    file["cells"][0]["outputs"] = serde_json::json!([]);
    file["cells"][0]["execution_count"] = serde_json::json!(5);
    room.reload(&Notebook::from_value(file).unwrap(), "h");
    assert_eq!(run_notes(room.doc(), "a").0, kept().0);
}

#[test]
fn an_edited_source_removes_the_skip_note_and_keeps_the_hash() {
    let insert = |txn: &mut TransactionMut, roots: &Roots| {
        let cell = schema::find_cell(txn, roots, "a").unwrap();
        let Some(Out::YText(src)) = cell.get(txn, "source") else {
            panic!()
        };
        src.insert(txn, 0, "x");
    };
    let edited = || (kept().0, false);
    // By a client.
    let mut room = room_that_ran();
    let client = Doc::new();
    sync(&mut room, 1, &client);
    let fx = client_edit(&mut room, &client, insert);
    assert_eq!(run_notes(room.doc(), "a"), edited());
    assert_eq!(run_notes(room.doc(), "b"), kept(), "not the other cell's");
    for (to, frame) in &fx.send {
        if *to == Target::All {
            apply_frame(&client, frame);
        }
    }
    assert_eq!(run_notes(&client, "a"), edited());
    // By the server (an agent's edit).
    let mut room = room_that_ran();
    let client = client_of(&room);
    let (_, fx) = room.edit(insert);
    assert_eq!(run_notes(room.doc(), "a"), edited());
    assert_eq!(run_notes(room.doc(), "b"), kept());
    for (_, frame) in &fx.send {
        apply_frame(&client, frame);
    }
    assert_eq!(run_notes(&client, "a"), edited());
    // By a change to the file, which leaves the room clean.
    let mut room = room_that_ran();
    let mut file = serde_json::to_value(room.notebook().unwrap().unwrap()).unwrap();
    file["cells"][0]["source"] = serde_json::json!("x1");
    let generation = room.generation();
    room.saved(generation, "g");
    room.reload(&Notebook::from_value(file).unwrap(), "h");
    assert_eq!(run_notes(room.doc(), "a"), edited());
    assert_eq!(run_notes(room.doc(), "b"), kept());
    assert!(!room.is_dirty());
    // By a change to the file while the room holds an unsaved edit, which
    // merges (`merge_disk`) rather than reloading: the cell the file
    // changed loses its skip note and keeps its hash, and the room's
    // own edit keeps its cell's note.
    let mut room = room_that_ran();
    let client = Doc::new();
    sync(&mut room, 1, &client);
    let base = room.notebook().unwrap().unwrap();
    room.saved(room.generation(), "g");
    let fx = client_edit(&mut room, &client, |txn, roots| {
        let cell = schema::find_cell(txn, roots, "b").unwrap();
        let Some(Out::YMap(meta)) = cell.get(txn, "metadata") else {
            panic!()
        };
        meta.insert(txn, "unsaved", 1);
    });
    assert!(fx.changed && room.is_dirty());
    let mut file = serde_json::to_value(&base).unwrap();
    file["cells"][0]["source"] = serde_json::json!("x1");
    let (fx, conflicts) = room.merge_disk(&base, &Notebook::from_value(file).unwrap(), "h");
    assert!(conflicts.is_empty());
    assert_eq!(run_notes(room.doc(), "a"), edited());
    assert_eq!(
        run_notes(room.doc(), "b"),
        (
            serde_json::json!({"tags": ["t"], "unsaved": 1,
                "ganymede": {"keep": 1, "ran_source_hash": "77"}}),
            true
        ),
        "the room's own edit keeps its cell's note"
    );
    assert!(fx.changed && room.is_dirty());
}

/// Removing a skip note is the room's own edit, and the next no-op
/// server edit must not count it as content to save.
#[test]
fn a_note_removed_by_the_room_is_not_content_to_save() {
    let mut room = room_that_ran();
    let mut file = serde_json::to_value(room.notebook().unwrap().unwrap()).unwrap();
    file["cells"][0]["source"] = serde_json::json!("x1");
    room.reload(&Notebook::from_value(file).unwrap(), "h");
    assert_eq!(run_notes(room.doc(), "a"), (kept().0, false));
    assert!(!room.is_dirty());
    let (_, fx) = room.edit(|_, _| ());
    assert!(!fx.changed && !room.is_dirty());
}

#[test]
fn the_ran_source_hash_is_set_and_removed_in_plain_and_shared_metadata() {
    let doc = Doc::new();
    let roots = Roots::get(&doc);
    let mut txn = doc.transact_mut();
    let json = serde_json::json!({"tags": ["t"], "ganymede": {"keep": 1}});
    let with_hash = serde_json::json!({"tags": ["t"],
        "ganymede": {"keep": 1, "ran_source_hash": "77"}});
    let cell = roots.cells.insert(&mut txn, 0, MapPrelim::default());
    let metadata = |txn: &TransactionMut, cell: &MapRef| {
        schema::out_to_json(txn, &cell.get(txn, "metadata").unwrap())
    };

    // Plain JSON: kept whole when the hash is written.
    cell.insert(&mut txn, "metadata", schema::json_to_any(&json));
    schema::set_ran_source_hash(&mut txn, &cell, "77");
    assert_eq!(metadata(&txn, &cell), with_hash);
    assert!(schema::clear_ran_source_hash(&mut txn, &cell));
    assert_eq!(metadata(&txn, &cell), json);
    assert!(!schema::clear_ran_source_hash(&mut txn, &cell));

    // Plain JSON that has the hash.
    cell.insert(&mut txn, "metadata", schema::json_to_any(&with_hash));
    assert!(schema::clear_ran_source_hash(&mut txn, &cell));
    assert_eq!(metadata(&txn, &cell), json);

    // Shared maps. The `ganymede` it empties stays.
    let meta = cell.insert(&mut txn, "metadata", MapPrelim::default());
    let g = meta.insert(&mut txn, "ganymede", MapPrelim::default());
    g.insert(&mut txn, "ran_source_hash", "77");
    assert!(schema::clear_ran_source_hash(&mut txn, &cell));
    assert_eq!(metadata(&txn, &cell), serde_json::json!({"ganymede": {}}));
}
