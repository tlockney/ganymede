//! Integration tests for `FsContents` against a temporary directory.

use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime};

use base64::Engine;
use ganymede_contents::{
    ContentType, Contents, ContentsModel, Error, Format, FsContents, FsOptions, GetOptions,
    SaveRequest, CHECKPOINT_ID, DEFAULT_CHECKPOINT_DIR,
};
use ganymede_nbformat::Notebook;
use serde_json::{json, Value};
use tempfile::TempDir;

fn setup() -> (TempDir, FsContents) {
    setup_with(FsOptions::default())
}

fn setup_with(options: FsOptions) -> (TempDir, FsContents) {
    let dir = tempfile::tempdir().unwrap();
    let contents = FsContents::new(dir.path(), options).unwrap();
    (dir, contents)
}

fn sample_notebook() -> Value {
    json!({
        "nbformat": 4,
        "nbformat_minor": 5,
        "metadata": {
            "kernelspec": {"name": "python3", "display_name": "Python 3", "language": "python"}
        },
        "cells": [
            {
                "cell_type": "code",
                "id": "c1",
                "metadata": {},
                "execution_count": 1,
                "outputs": [{"output_type": "stream", "name": "stdout", "text": "hi\nthere\n"}],
                "source": "print('hi')\nprint('there')\n"
            },
            {"cell_type": "markdown", "id": "m1", "metadata": {}, "source": "# Title\n\nbody"}
        ]
    })
}

/// The bytes `nbformat.write` would put on disk for `value`.
fn expected_file(value: &Value) -> String {
    Notebook::from_value(value.clone())
        .unwrap()
        .to_file_string()
}

fn status(result: &Result<impl std::fmt::Debug, Error>) -> u16 {
    match result {
        Ok(v) => panic!("expected an error, got {v:?}"),
        Err(e) => e.status(),
    }
}

fn set_mtime(path: &Path, t: SystemTime) {
    fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(t)
        .unwrap();
}

fn child_names(model: &ContentsModel) -> Vec<String> {
    model
        .content
        .as_ref()
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap().to_owned())
        .collect()
}

fn assert_timestamp(ts: &str) {
    assert_eq!(ts.len(), 27, "{ts}");
    assert!(ts.ends_with('Z'), "{ts}");
    assert_eq!(&ts[19..20], ".", "{ts}");
    chrono::DateTime::parse_from_rfc3339(ts).unwrap();
}

#[tokio::test]
async fn notebook_round_trip() {
    let (dir, c) = setup();
    let nb = sample_notebook();

    let saved = c
        .save("nb.ipynb", SaveRequest::notebook(nb.clone()))
        .await
        .unwrap();
    assert_eq!(saved.kind, ContentType::Notebook);
    assert_eq!(saved.path, "nb.ipynb");
    assert_eq!(saved.name, "nb.ipynb");
    assert_eq!(saved.content, None);
    assert_eq!(saved.format, None);
    assert_eq!(saved.mimetype, None);
    assert!(saved.writable);
    assert!(saved.size.unwrap() > 0);
    assert_timestamp(&saved.created);
    assert_timestamp(&saved.last_modified);

    let expected = expected_file(&nb);
    assert_eq!(
        fs::read_to_string(dir.path().join("nb.ipynb")).unwrap(),
        expected
    );

    let got = c.get("nb.ipynb", GetOptions::with_content()).await.unwrap();
    assert_eq!(got.format, Some(Format::Json));
    assert_eq!(got.mimetype, None);
    let content = got.content.unwrap();
    assert_eq!(
        content,
        serde_json::to_value(Notebook::from_value(nb).unwrap()).unwrap()
    );
    // Served as nbformat.read gives it: sources rejoined into one string.
    assert_eq!(
        content["cells"][0]["source"],
        "print('hi')\nprint('there')\n"
    );
    assert_eq!(content["nbformat"], 4);

    // type=file on a notebook returns the raw text.
    let raw = c
        .get(
            "nb.ipynb",
            GetOptions {
                content: true,
                type_hint: Some(ContentType::File),
                format: None,
                hash: false,
            },
        )
        .await
        .unwrap();
    assert_eq!(raw.kind, ContentType::File);
    assert_eq!(raw.format, Some(Format::Text));
    assert_eq!(raw.content.unwrap().as_str().unwrap(), expected);
}

#[tokio::test]
async fn invalid_notebooks_are_bad_requests() {
    let (dir, c) = setup();
    let r = c
        .save("bad.ipynb", SaveRequest::notebook(json!({"cells": []})))
        .await;
    assert_eq!(status(&r), 400);
    assert!(!dir.path().join("bad.ipynb").exists());

    fs::write(dir.path().join("broken.ipynb"), "not json").unwrap();
    let r = c.get("broken.ipynb", GetOptions::with_content()).await;
    assert_eq!(status(&r), 400);
    // Without content the model is still served.
    let m = c.get("broken.ipynb", GetOptions::default()).await.unwrap();
    assert_eq!(m.kind, ContentType::Notebook);
}

#[tokio::test]
async fn text_file_round_trip() {
    let (dir, c) = setup();

    let r = c.save("notes/a.md", SaveRequest::text("x")).await;
    assert_eq!(status(&r), 404);

    c.save("notes", SaveRequest::directory()).await.unwrap();
    let saved = c
        .save("notes/a.md", SaveRequest::text("héllo\n"))
        .await
        .unwrap();
    assert_eq!(saved.kind, ContentType::File);
    assert_eq!(saved.size, Some(7));
    assert_eq!(saved.content, None);
    assert_eq!(saved.format, None);
    assert_eq!(saved.mimetype.as_deref(), Some("text/markdown"));
    assert_eq!(
        fs::read_to_string(dir.path().join("notes/a.md")).unwrap(),
        "héllo\n"
    );

    let got = c
        .get("notes/a.md", GetOptions::with_content())
        .await
        .unwrap();
    assert_eq!(got.format, Some(Format::Text));
    assert_eq!(got.content, Some(Value::String("héllo\n".into())));
    assert_eq!(got.mimetype.as_deref(), Some("text/markdown"));

    // Unknown extension: text/plain only once content is read.
    c.save("noext", SaveRequest::text("plain")).await.unwrap();
    let m = c.get("noext", GetOptions::default()).await.unwrap();
    assert_eq!(m.mimetype, None);
    let m = c.get("noext", GetOptions::with_content()).await.unwrap();
    assert_eq!(m.mimetype.as_deref(), Some("text/plain"));

    // A file save needs a format and string content.
    let mut no_format = SaveRequest::text("x");
    no_format.format = None;
    assert_eq!(status(&c.save("b.txt", no_format).await), 400);
    let mut not_string = SaveRequest::text("x");
    not_string.content = Some(json!({"a": 1}));
    assert_eq!(status(&c.save("b.txt", not_string).await), 400);
    let mut no_content = SaveRequest::text("x");
    no_content.content = None;
    assert_eq!(status(&c.save("b.txt", no_content).await), 400);
}

#[tokio::test]
async fn base64_file_round_trip() {
    let (dir, c) = setup();
    let bytes: Vec<u8> = (0..=255u8).collect();
    let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);

    let saved = c
        .save("blob.bin", SaveRequest::base64(encoded.clone()))
        .await
        .unwrap();
    assert_eq!(saved.size, Some(256));
    assert_eq!(fs::read(dir.path().join("blob.bin")).unwrap(), bytes);

    // Not UTF-8 and no format requested: served as base64.
    let got = c.get("blob.bin", GetOptions::with_content()).await.unwrap();
    assert_eq!(got.format, Some(Format::Base64));
    assert_eq!(got.content, Some(Value::String(encoded.clone())));
    assert_eq!(got.mimetype.as_deref(), Some("application/octet-stream"));

    // Explicit text on binary bytes is a 400.
    let r = c
        .get(
            "blob.bin",
            GetOptions {
                content: true,
                type_hint: None,
                format: Some(Format::Text),
                hash: false,
            },
        )
        .await;
    assert_eq!(status(&r), 400);

    // Explicit base64 on a text file works.
    c.save("t.txt", SaveRequest::text("abc")).await.unwrap();
    let got = c
        .get(
            "t.txt",
            GetOptions {
                content: true,
                type_hint: None,
                format: Some(Format::Base64),
                hash: false,
            },
        )
        .await
        .unwrap();
    assert_eq!(got.format, Some(Format::Base64));
    assert_eq!(got.content, Some(Value::String("YWJj".into())));

    // Lenient decoding: line breaks (base64.encodebytes style) and missing
    // padding are accepted; garbage is a 400.
    let wrapped = format!("{}\n{}", &encoded[..76], &encoded[76..]);
    c.save("blob2.bin", SaveRequest::base64(wrapped))
        .await
        .unwrap();
    assert_eq!(fs::read(dir.path().join("blob2.bin")).unwrap(), bytes);
    c.save("abc.bin", SaveRequest::base64("YWJj"))
        .await
        .unwrap();
    assert_eq!(fs::read(dir.path().join("abc.bin")).unwrap(), b"abc");
    assert_eq!(
        status(&c.save("bad.bin", SaveRequest::base64("!!!")).await),
        400
    );
}

#[tokio::test]
async fn directory_listing_shape() {
    let (dir, c) = setup();
    c.save("sub", SaveRequest::directory()).await.unwrap();
    c.save("b.txt", SaveRequest::text("B")).await.unwrap();
    c.save("n.ipynb", SaveRequest::notebook(sample_notebook()))
        .await
        .unwrap();
    fs::write(dir.path().join(".hidden"), "").unwrap();
    fs::create_dir(dir.path().join(".git")).unwrap();

    let root = c.get("", GetOptions::with_content()).await.unwrap();
    assert_eq!(root.name, "");
    assert_eq!(root.path, "");
    assert_eq!(root.kind, ContentType::Directory);
    assert_eq!(root.format, Some(Format::Json));
    assert_eq!(root.mimetype, None);
    assert_eq!(root.size, None);
    assert!(root.writable);
    assert_eq!(child_names(&root), ["b.txt", "n.ipynb", "sub"]);

    let children: Vec<ContentsModel> =
        serde_json::from_value(root.content.clone().unwrap()).unwrap();
    for child in &children {
        assert_eq!(child.content, None);
        assert_eq!(child.format, None);
        assert_eq!(child.path, child.name);
    }
    assert_eq!(children[0].kind, ContentType::File);
    assert_eq!(children[0].mimetype.as_deref(), Some("text/plain"));
    assert_eq!(children[0].size, Some(1));
    assert_eq!(children[1].kind, ContentType::Notebook);
    assert_eq!(children[1].mimetype, None);
    assert!(children[1].size.is_some());
    assert_eq!(children[2].kind, ContentType::Directory);
    assert_eq!(children[2].size, None);

    // Serialized field names.
    let json = serde_json::to_value(&root).unwrap();
    let keys: Vec<&str> = json
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        [
            "name",
            "path",
            "type",
            "created",
            "last_modified",
            "mimetype",
            "format",
            "content",
            "writable",
            "size",
            "hash",
            "hash_algorithm"
        ]
    );
    assert_eq!(json["type"], "directory");
    assert_eq!(json["content"][0]["type"], "file");

    let empty = c.get("sub", GetOptions::with_content()).await.unwrap();
    assert_eq!(empty.content, Some(json!([])));
    let no_content = c.get("", GetOptions::default()).await.unwrap();
    assert_eq!(no_content.content, None);
    assert_eq!(no_content.format, None);

    // Type hints that do not match the entry.
    let r = c
        .get(
            "b.txt",
            GetOptions {
                content: false,
                type_hint: Some(ContentType::Directory),
                format: None,
                hash: false,
            },
        )
        .await;
    assert_eq!(status(&r), 400);
    let r = c
        .get(
            "sub",
            GetOptions {
                content: false,
                type_hint: Some(ContentType::File),
                format: None,
                hash: false,
            },
        )
        .await;
    assert_eq!(status(&r), 400);

    assert_eq!(status(&c.get("missing", GetOptions::default()).await), 404);
    // Saving a directory over a file, or a file over a directory.
    assert_eq!(
        status(&c.save("b.txt", SaveRequest::directory()).await),
        400
    );
    assert_eq!(status(&c.save("sub", SaveRequest::text("x")).await), 400);
    // Saving an existing directory is a no-op.
    c.save("sub", SaveRequest::directory()).await.unwrap();
}

#[tokio::test]
async fn untitled_naming_sequence() {
    let (dir, c) = setup();
    let mut names = Vec::new();
    for _ in 0..3 {
        let m = c
            .new_untitled("", ContentType::Notebook, None)
            .await
            .unwrap();
        assert_eq!(m.kind, ContentType::Notebook);
        assert_eq!(m.content, None);
        names.push(m.name);
    }
    assert_eq!(
        names,
        ["Untitled.ipynb", "Untitled1.ipynb", "Untitled2.ipynb"]
    );
    assert_eq!(
        fs::read_to_string(dir.path().join("Untitled.ipynb")).unwrap(),
        Notebook::new().to_file_string()
    );
    let m = c
        .get("Untitled1.ipynb", GetOptions::with_content())
        .await
        .unwrap();
    assert_eq!(m.content.unwrap()["cells"], json!([]));

    let a = c
        .new_untitled("", ContentType::File, Some(".txt"))
        .await
        .unwrap();
    let b = c
        .new_untitled("", ContentType::File, Some("txt"))
        .await
        .unwrap();
    let n = c.new_untitled("", ContentType::File, None).await.unwrap();
    assert_eq!(a.name, "untitled.txt");
    assert_eq!(a.size, Some(0));
    assert_eq!(b.name, "untitled1.txt");
    assert_eq!(n.name, "untitled");

    let d0 = c
        .new_untitled("", ContentType::Directory, None)
        .await
        .unwrap();
    let d1 = c
        .new_untitled("", ContentType::Directory, None)
        .await
        .unwrap();
    assert_eq!(d0.name, "Untitled Folder");
    assert_eq!(d0.kind, ContentType::Directory);
    assert_eq!(d1.name, "Untitled Folder 1");

    let nested = c
        .new_untitled("Untitled Folder", ContentType::Notebook, None)
        .await
        .unwrap();
    assert_eq!(nested.path, "Untitled Folder/Untitled.ipynb");

    let r = c.new_untitled("nope", ContentType::Notebook, None).await;
    assert_eq!(status(&r), 404);
    let r = c
        .new_untitled("untitled.txt", ContentType::Notebook, None)
        .await;
    assert_eq!(status(&r), 404);
}

#[tokio::test]
async fn rename_and_copy() {
    let (dir, c) = setup();
    c.save("a.txt", SaveRequest::text("A")).await.unwrap();
    c.save("sub", SaveRequest::directory()).await.unwrap();

    let m = c.rename("a.txt", "sub/b.txt").await.unwrap();
    assert_eq!(m.path, "sub/b.txt");
    assert_eq!(m.name, "b.txt");
    assert_eq!(m.content, None);
    assert!(!dir.path().join("a.txt").exists());
    assert_eq!(
        fs::read_to_string(dir.path().join("sub/b.txt")).unwrap(),
        "A"
    );

    c.save("sub/c.txt", SaveRequest::text("C")).await.unwrap();
    assert_eq!(status(&c.rename("sub/b.txt", "sub/c.txt").await), 409);
    assert_eq!(status(&c.rename("nope", "x").await), 404);
    assert_eq!(status(&c.rename("", "x").await), 403);
    // Renaming onto itself is a no-op.
    assert_eq!(
        c.rename("sub/b.txt", "sub/b.txt").await.unwrap().path,
        "sub/b.txt"
    );

    let c1 = c.copy("sub/b.txt", None).await.unwrap();
    assert_eq!(c1.path, "sub/b-Copy1.txt");
    let c2 = c.copy("sub/b.txt", None).await.unwrap();
    assert_eq!(c2.path, "sub/b-Copy2.txt");
    // Copying a copy strips the suffix and keeps counting.
    let c3 = c.copy("sub/b-Copy1.txt", None).await.unwrap();
    assert_eq!(c3.path, "sub/b-Copy3.txt");
    assert_eq!(
        fs::read_to_string(dir.path().join("sub/b-Copy3.txt")).unwrap(),
        "A"
    );

    let to_root = c.copy("sub/b.txt", Some("")).await.unwrap();
    assert_eq!(to_root.path, "b.txt");
    // A destination that is not a directory is the full target path.
    let full = c.copy("sub/b.txt", Some("dest.txt")).await.unwrap();
    assert_eq!(full.path, "dest.txt");
    assert_eq!(
        fs::read_to_string(dir.path().join("dest.txt")).unwrap(),
        "A"
    );

    assert_eq!(status(&c.copy("sub", None).await), 400);
    assert_eq!(status(&c.copy("missing.txt", None).await), 404);

    // Notebooks copy byte for byte.
    c.save("n.ipynb", SaveRequest::notebook(sample_notebook()))
        .await
        .unwrap();
    let nc = c.copy("n.ipynb", None).await.unwrap();
    assert_eq!(nc.path, "n-Copy1.ipynb");
    assert_eq!(nc.kind, ContentType::Notebook);
    assert_eq!(
        fs::read(dir.path().join("n-Copy1.ipynb")).unwrap(),
        fs::read(dir.path().join("n.ipynb")).unwrap()
    );

    // Directories rename too.
    let moved = c.rename("sub", "moved").await.unwrap();
    assert_eq!(moved.kind, ContentType::Directory);
    assert_eq!(moved.path, "moved");
    c.get("moved/b.txt", GetOptions::default()).await.unwrap();
}

#[tokio::test]
async fn delete() {
    let (dir, c) = setup();
    c.save("a.txt", SaveRequest::text("A")).await.unwrap();
    c.delete("a.txt").await.unwrap();
    assert_eq!(status(&c.get("a.txt", GetOptions::default()).await), 404);
    assert_eq!(status(&c.delete("a.txt").await), 404);
    assert_eq!(status(&c.delete("").await), 403);

    c.save("d", SaveRequest::directory()).await.unwrap();
    c.save("d/x.txt", SaveRequest::text("x")).await.unwrap();
    assert_eq!(status(&c.delete("d").await), 400);
    c.delete("d/x.txt").await.unwrap();
    c.delete("d").await.unwrap();
    assert!(!dir.path().join("d").exists());

    // Deleting a file removes its checkpoint; a directory holding only its
    // checkpoint folder counts as empty.
    c.save("e", SaveRequest::directory()).await.unwrap();
    c.save("e/n.ipynb", SaveRequest::notebook(sample_notebook()))
        .await
        .unwrap();
    c.create_checkpoint("e/n.ipynb").await.unwrap();
    let cp = dir
        .path()
        .join("e")
        .join(DEFAULT_CHECKPOINT_DIR)
        .join("n-checkpoint.ipynb");
    assert!(cp.is_file());
    c.delete("e/n.ipynb").await.unwrap();
    assert!(!cp.exists());
    assert!(dir.path().join("e").join(DEFAULT_CHECKPOINT_DIR).is_dir());
    c.delete("e").await.unwrap();
    assert!(!dir.path().join("e").exists());
}

#[tokio::test]
async fn path_traversal_rejected() {
    let (dir, c) = setup();
    for bad in ["../x", "a/../../x", "/etc/passwd", "a\\b", "..\\x"] {
        assert_eq!(
            status(&c.get(bad, GetOptions::default()).await),
            403,
            "{bad}"
        );
        assert_eq!(
            status(&c.save(bad, SaveRequest::text("x")).await),
            403,
            "{bad}"
        );
        assert_eq!(status(&c.delete(bad).await), 403, "{bad}");
        assert_eq!(status(&c.rename("x", bad).await), 403, "{bad}");
        assert_eq!(status(&c.list_checkpoints(bad).await), 403, "{bad}");
    }
    // Nothing was created in the root's parent.
    assert!(!dir.path().parent().unwrap().join("x").exists());
}

#[cfg(unix)]
#[tokio::test]
async fn symlink_escaping_root_rejected() {
    use std::os::unix::fs::symlink;

    let (dir, c) = setup();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("secret.txt"), "secret").unwrap();

    symlink(outside.path(), dir.path().join("esc")).unwrap();
    symlink(
        outside.path().join("secret.txt"),
        dir.path().join("link.txt"),
    )
    .unwrap();
    symlink(
        outside.path().join("nothing.txt"),
        dir.path().join("dangling.txt"),
    )
    .unwrap();

    assert_eq!(status(&c.get("esc", GetOptions::with_content()).await), 403);
    assert_eq!(
        status(&c.get("esc/secret.txt", GetOptions::with_content()).await),
        403
    );
    assert_eq!(
        status(&c.get("link.txt", GetOptions::with_content()).await),
        403
    );
    assert_eq!(
        status(&c.save("esc/new.txt", SaveRequest::text("x")).await),
        403
    );
    assert_eq!(
        status(&c.save("dangling.txt", SaveRequest::text("x")).await),
        403
    );
    assert!(!outside.path().join("new.txt").exists());
    assert!(!outside.path().join("nothing.txt").exists());
    assert_eq!(
        fs::read_to_string(outside.path().join("secret.txt")).unwrap(),
        "secret"
    );

    // Escaping entries are left out of listings.
    let root = c.get("", GetOptions::with_content()).await.unwrap();
    assert_eq!(child_names(&root), Vec::<String>::new());

    // A symlink that stays inside the root is fine.
    fs::create_dir(dir.path().join("real")).unwrap();
    fs::write(dir.path().join("real/f.txt"), "f").unwrap();
    symlink(dir.path().join("real"), dir.path().join("alias")).unwrap();
    let m = c
        .get("alias/f.txt", GetOptions::with_content())
        .await
        .unwrap();
    assert_eq!(m.content, Some(Value::String("f".into())));
    let root = c.get("", GetOptions::with_content()).await.unwrap();
    assert_eq!(child_names(&root), ["alias", "real"]);
}

#[tokio::test]
async fn hidden_paths() {
    let (dir, c) = setup();
    fs::create_dir(dir.path().join(".secret")).unwrap();
    fs::write(dir.path().join(".secret/x.txt"), "x").unwrap();
    fs::write(dir.path().join(".dotfile"), "d").unwrap();
    fs::write(dir.path().join("plain.txt"), "p").unwrap();

    for hidden in [".secret", ".secret/x.txt", ".dotfile"] {
        assert_eq!(
            status(&c.get(hidden, GetOptions::with_content()).await),
            404,
            "{hidden}"
        );
    }
    assert_eq!(
        status(&c.save(".secret/y.txt", SaveRequest::text("y")).await),
        400
    );
    assert_eq!(status(&c.delete(".dotfile").await), 400);
    assert_eq!(status(&c.rename("plain.txt", ".plain").await), 400);
    assert!(dir.path().join(".dotfile").exists());
    assert!(!dir.path().join(".secret/y.txt").exists());
    let root = c.get("", GetOptions::with_content()).await.unwrap();
    assert_eq!(child_names(&root), ["plain.txt"]);

    let c = FsContents::new(
        dir.path(),
        FsOptions {
            allow_hidden: true,
            ..FsOptions::default()
        },
    )
    .unwrap();
    let m = c
        .get(".secret/x.txt", GetOptions::with_content())
        .await
        .unwrap();
    assert_eq!(m.content, Some(Value::String("x".into())));
    let root = c.get("", GetOptions::with_content()).await.unwrap();
    assert_eq!(child_names(&root), [".dotfile", ".secret", "plain.txt"]);
    c.save(".secret/y.txt", SaveRequest::text("y"))
        .await
        .unwrap();
}

#[tokio::test]
async fn conflict_detection() {
    let (dir, c) = setup();
    let path = dir.path().join("a.txt");

    // A new file with a last_modified is fine.
    let m1 = c
        .save(
            "a.txt",
            SaveRequest::text("v1").with_last_modified("2020-01-01T00:00:00.000000Z"),
        )
        .await
        .unwrap();
    let t1 = m1.last_modified.clone();

    // Another writer changes the file after the client's copy.
    fs::write(&path, "external").unwrap();
    set_mtime(&path, SystemTime::now() + Duration::from_secs(2));

    let r = c
        .save("a.txt", SaveRequest::text("v2").with_last_modified(&t1))
        .await;
    assert_eq!(status(&r), 409);
    assert!(matches!(r, Err(Error::Conflict(_))));
    assert_eq!(fs::read_to_string(&path).unwrap(), "external");

    // A fresh timestamp from the server wins.
    let fresh = c.get("a.txt", GetOptions::default()).await.unwrap();
    c.save(
        "a.txt",
        SaveRequest::text("v2").with_last_modified(&fresh.last_modified),
    )
    .await
    .unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "v2");

    // Changes within the 500 ms window are not conflicts.
    let fresh = c.get("a.txt", GetOptions::default()).await.unwrap();
    let t: SystemTime = chrono::DateTime::parse_from_rfc3339(&fresh.last_modified)
        .unwrap()
        .into();
    set_mtime(&path, t + Duration::from_millis(300));
    c.save(
        "a.txt",
        SaveRequest::text("v3").with_last_modified(&fresh.last_modified),
    )
    .await
    .unwrap();

    // Without last_modified the client's copy always wins.
    fs::write(&path, "external again").unwrap();
    set_mtime(&path, SystemTime::now() + Duration::from_secs(2));
    c.save("a.txt", SaveRequest::text("v4")).await.unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "v4");

    // Notebooks get the same check.
    let nb = c
        .save("n.ipynb", SaveRequest::notebook(sample_notebook()))
        .await
        .unwrap();
    set_mtime(
        &dir.path().join("n.ipynb"),
        SystemTime::now() + Duration::from_secs(2),
    );
    let r = c
        .save(
            "n.ipynb",
            SaveRequest::notebook(sample_notebook()).with_last_modified(&nb.last_modified),
        )
        .await;
    assert_eq!(status(&r), 409);

    let r = c
        .save(
            "a.txt",
            SaveRequest::text("v5").with_last_modified("yesterday"),
        )
        .await;
    assert_eq!(status(&r), 400);
}

#[tokio::test]
async fn atomic_write_preserves_mode_and_replaces_in_place() {
    let (dir, c) = setup();
    let path = dir.path().join("a.txt");
    c.save("a.txt", SaveRequest::text("v1")).await.unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        c.save("a.txt", SaveRequest::text("v2")).await.unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o640
        );
    }
    assert_eq!(fs::read_to_string(&path).unwrap(), "v2");
    // No temp files left behind.
    let leftovers: Vec<_> = fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(leftovers, ["a.txt"]);
}

#[tokio::test]
async fn checkpoints_create_list_restore_delete() {
    let (dir, c) = setup();
    c.save("d", SaveRequest::directory()).await.unwrap();
    let v1 = sample_notebook();
    c.save("d/n.ipynb", SaveRequest::notebook(v1.clone()))
        .await
        .unwrap();

    assert_eq!(c.list_checkpoints("d/n.ipynb").await.unwrap(), vec![]);
    let cp = c.create_checkpoint("d/n.ipynb").await.unwrap();
    assert_eq!(cp.id, CHECKPOINT_ID);
    assert_timestamp(&cp.last_modified);
    let cp_path = dir
        .path()
        .join("d")
        .join(DEFAULT_CHECKPOINT_DIR)
        .join("n-checkpoint.ipynb");
    assert_eq!(fs::read_to_string(&cp_path).unwrap(), expected_file(&v1));
    let listed = c.list_checkpoints("d/n.ipynb").await.unwrap();
    assert_eq!(listed, vec![cp.clone()]);

    // The checkpoint folder is hidden from listings.
    let d = c.get("d", GetOptions::with_content()).await.unwrap();
    assert_eq!(child_names(&d), ["n.ipynb"]);

    let mut v2 = v1.clone();
    v2["cells"][1]["source"] = json!("# Changed");
    c.save("d/n.ipynb", SaveRequest::notebook(v2.clone()))
        .await
        .unwrap();
    assert_eq!(
        fs::read_to_string(dir.path().join("d/n.ipynb")).unwrap(),
        expected_file(&v2)
    );

    c.restore_checkpoint("d/n.ipynb", CHECKPOINT_ID)
        .await
        .unwrap();
    assert_eq!(
        fs::read_to_string(dir.path().join("d/n.ipynb")).unwrap(),
        expected_file(&v1)
    );
    assert_eq!(
        status(&c.restore_checkpoint("d/n.ipynb", "nope").await),
        404
    );

    // Renaming carries the checkpoint along.
    c.rename("d/n.ipynb", "d/m.ipynb").await.unwrap();
    assert!(!cp_path.exists());
    assert_eq!(c.list_checkpoints("d/m.ipynb").await.unwrap().len(), 1);

    c.delete_checkpoint("d/m.ipynb", CHECKPOINT_ID)
        .await
        .unwrap();
    assert_eq!(c.list_checkpoints("d/m.ipynb").await.unwrap(), vec![]);
    assert_eq!(
        status(&c.restore_checkpoint("d/m.ipynb", CHECKPOINT_ID).await),
        404
    );
    assert_eq!(
        status(&c.delete_checkpoint("d/m.ipynb", CHECKPOINT_ID).await),
        404
    );

    assert_eq!(status(&c.create_checkpoint("d").await), 400);
    assert_eq!(status(&c.create_checkpoint("missing.txt").await), 404);
    // jupyter_server lists checkpoints without checking the file exists.
    assert_eq!(c.list_checkpoints("d/none.ipynb").await.unwrap(), vec![]);

    // Plain files have checkpoints too.
    c.save("d/t.txt", SaveRequest::text("one")).await.unwrap();
    c.create_checkpoint("d/t.txt").await.unwrap();
    c.save("d/t.txt", SaveRequest::text("two")).await.unwrap();
    c.restore_checkpoint("d/t.txt", CHECKPOINT_ID)
        .await
        .unwrap();
    assert_eq!(
        fs::read_to_string(dir.path().join("d/t.txt")).unwrap(),
        "one"
    );
}

#[tokio::test]
async fn checkpoint_dir_outside_root() {
    let outside = tempfile::tempdir().unwrap();
    let cp_root = outside.path().join("cps");
    let (dir, c) = setup_with(FsOptions {
        checkpoint_dir: Some(cp_root.clone()),
        ..FsOptions::default()
    });
    c.save("sub", SaveRequest::directory()).await.unwrap();
    c.save("other", SaveRequest::directory()).await.unwrap();
    let v1 = sample_notebook();
    c.save("sub/n.ipynb", SaveRequest::notebook(v1.clone()))
        .await
        .unwrap();
    c.save("other/n.ipynb", SaveRequest::notebook(v1.clone()))
        .await
        .unwrap();

    c.create_checkpoint("sub/n.ipynb").await.unwrap();
    c.create_checkpoint("other/n.ipynb").await.unwrap();
    assert!(cp_root.join("sub/n-checkpoint.ipynb").is_file());
    assert!(cp_root.join("other/n-checkpoint.ipynb").is_file());
    assert_eq!(c.list_checkpoints("sub/n.ipynb").await.unwrap().len(), 1);

    // Nothing checkpoint-related under the synced root.
    fn find_checkpoint_dirs(p: &Path, out: &mut Vec<std::path::PathBuf>) {
        for e in fs::read_dir(p).unwrap() {
            let e = e.unwrap();
            if e.file_name() == DEFAULT_CHECKPOINT_DIR {
                out.push(e.path());
            }
            if e.file_type().unwrap().is_dir() {
                find_checkpoint_dirs(&e.path(), out);
            }
        }
    }
    let mut found = Vec::new();
    find_checkpoint_dirs(dir.path(), &mut found);
    assert!(found.is_empty(), "{found:?}");

    let mut v2 = v1.clone();
    v2["cells"] = json!([]);
    c.save("sub/n.ipynb", SaveRequest::notebook(v2))
        .await
        .unwrap();
    c.restore_checkpoint("sub/n.ipynb", CHECKPOINT_ID)
        .await
        .unwrap();
    assert_eq!(
        fs::read_to_string(dir.path().join("sub/n.ipynb")).unwrap(),
        expected_file(&v1)
    );

    // Renames of the file and of its directory move the checkpoints.
    c.rename("sub/n.ipynb", "sub/m.ipynb").await.unwrap();
    assert!(cp_root.join("sub/m-checkpoint.ipynb").is_file());
    assert!(!cp_root.join("sub/n-checkpoint.ipynb").exists());
    c.rename("sub", "sub2").await.unwrap();
    assert!(cp_root.join("sub2/m-checkpoint.ipynb").is_file());
    assert_eq!(c.list_checkpoints("sub2/m.ipynb").await.unwrap().len(), 1);

    c.delete("sub2/m.ipynb").await.unwrap();
    assert!(!cp_root.join("sub2/m-checkpoint.ipynb").exists());
    // The other directory's checkpoint is untouched.
    assert!(cp_root.join("other/n-checkpoint.ipynb").is_file());
}

#[tokio::test]
async fn read_only_root() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("a.txt"), "A").unwrap();
    let c = FsContents::new(
        dir.path(),
        FsOptions {
            read_only: true,
            ..FsOptions::default()
        },
    )
    .unwrap();
    let m = c.get("a.txt", GetOptions::with_content()).await.unwrap();
    assert!(!m.writable);
    assert_eq!(m.content, Some(Value::String("A".into())));
    let root = c.get("", GetOptions::with_content()).await.unwrap();
    assert!(!root.writable);

    assert_eq!(status(&c.save("b.txt", SaveRequest::text("B")).await), 403);
    assert_eq!(status(&c.delete("a.txt").await), 403);
    assert_eq!(status(&c.rename("a.txt", "b.txt").await), 403);
    assert_eq!(
        status(&c.new_untitled("", ContentType::Notebook, None).await),
        403
    );
    assert_eq!(status(&c.copy("a.txt", None).await), 403);
    assert_eq!(status(&c.create_checkpoint("a.txt").await), 403);
    assert_eq!(fs::read_to_string(dir.path().join("a.txt")).unwrap(), "A");
    assert_eq!(
        fs::read_dir(dir.path()).unwrap().count(),
        1,
        "nothing was created"
    );
}

#[tokio::test]
async fn root_must_be_a_directory() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("f"), "").unwrap();
    assert!(FsContents::new(dir.path().join("f"), FsOptions::default()).is_err());
    assert!(FsContents::new(dir.path().join("missing"), FsOptions::default()).is_err());
}

#[tokio::test]
async fn hash_is_null_unless_requested() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "abc").unwrap();
    let c = ganymede_contents::FsContents::new(dir.path(), Default::default()).unwrap();
    use ganymede_contents::Contents;
    let m = c
        .get("a.txt", ganymede_contents::GetOptions::default())
        .await
        .unwrap();
    let v = serde_json::to_value(&m).unwrap();
    assert!(v["hash"].is_null() && v["hash_algorithm"].is_null());
    let m = c
        .get(
            "a.txt",
            ganymede_contents::GetOptions {
                hash: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        m.hash.as_deref(),
        Some("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
    );
    assert_eq!(m.hash_algorithm.as_deref(), Some("sha256"));
}

#[cfg(unix)]
#[tokio::test]
async fn save_through_symlink_keeps_the_link() {
    use std::os::unix::fs::symlink;
    let (dir, c) = setup();
    c.save("real.txt", SaveRequest::text("v1")).await.unwrap();
    symlink("real.txt", dir.path().join("alias.txt")).unwrap();
    c.save("alias.txt", SaveRequest::text("v2")).await.unwrap();
    assert!(fs::symlink_metadata(dir.path().join("alias.txt"))
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(
        fs::read_to_string(dir.path().join("real.txt")).unwrap(),
        "v2"
    );
}
