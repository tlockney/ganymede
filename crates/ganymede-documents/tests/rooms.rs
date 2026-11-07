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
