//! ZeroMQ multipart framing and HMAC signing.
//!
//! On the wire a message is:
//! `[identities..., b"<IDS|MSG>", signature, header, parent_header, metadata, content, buffers...]`.
//! The signature is the hex HMAC over the four JSON frames in order, or the
//! empty string when the connection file has no key.

use crate::{Channel, Error, Header, Message, Result};
use bytes::Bytes;
use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

pub const DELIMITER: &[u8] = b"<IDS|MSG>";

/// Signs and verifies frames. Clone is cheap.
#[derive(Clone)]
pub struct Signer {
    key: Option<Vec<u8>>,
}

impl std::fmt::Debug for Signer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Signer")
            .field("signed", &self.key.is_some())
            .finish()
    }
}

impl Signer {
    pub fn new(scheme: &str, key: &[u8]) -> Result<Self> {
        if key.is_empty() {
            return Ok(Signer { key: None });
        }
        if scheme != "hmac-sha256" {
            return Err(Error::UnsupportedScheme(scheme.to_string()));
        }
        Ok(Signer {
            key: Some(key.to_vec()),
        })
    }

    pub fn unsigned() -> Self {
        Signer { key: None }
    }

    pub fn sign(&self, parts: [&[u8]; 4]) -> String {
        match &self.key {
            None => String::new(),
            Some(k) => {
                let mut mac =
                    Hmac::<Sha256>::new_from_slice(k).expect("hmac accepts any key length");
                for p in parts {
                    mac.update(p);
                }
                hex::encode(mac.finalize().into_bytes())
            }
        }
    }

    pub fn verify(&self, signature: &[u8], parts: [&[u8]; 4]) -> bool {
        match &self.key {
            None => true,
            Some(k) => {
                let Ok(sig) = hex::decode(signature) else {
                    return false;
                };
                let mut mac =
                    Hmac::<Sha256>::new_from_slice(k).expect("hmac accepts any key length");
                for p in parts {
                    mac.update(p);
                }
                mac.verify_slice(&sig).is_ok()
            }
        }
    }
}

/// A decoded wire message plus the routing identities that preceded the
/// delimiter. On a DEALER socket there are none; on a SUB socket there is
/// the topic frame; on a ROUTER socket (the kernel side) there is the peer
/// identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireMessage {
    pub identities: Vec<Bytes>,
    pub message: Message,
}

/// Encode a message into frames, signing with `signer`.
pub fn encode(signer: &Signer, identities: &[Bytes], msg: &Message) -> Result<Vec<Bytes>> {
    let header = serde_json::to_vec(&msg.header)?;
    let parent = match &msg.parent_header {
        Some(h) => serde_json::to_vec(h)?,
        None => b"{}".to_vec(),
    };
    let metadata = serde_json::to_vec(&msg.metadata)?;
    let content = serde_json::to_vec(&msg.content)?;
    let sig = signer.sign([&header, &parent, &metadata, &content]);

    let mut frames = Vec::with_capacity(identities.len() + 6 + msg.buffers.len());
    frames.extend(identities.iter().cloned());
    frames.push(Bytes::from_static(DELIMITER));
    frames.push(Bytes::from(sig.into_bytes()));
    frames.push(Bytes::from(header));
    frames.push(Bytes::from(parent));
    frames.push(Bytes::from(metadata));
    frames.push(Bytes::from(content));
    frames.extend(msg.buffers.iter().cloned());
    Ok(frames)
}

/// Decode frames, verifying the signature.
pub fn decode(signer: &Signer, frames: &[Bytes]) -> Result<WireMessage> {
    let delim = frames
        .iter()
        .position(|f| f.as_ref() == DELIMITER)
        .ok_or(Error::Malformed("no <IDS|MSG> delimiter"))?;
    let rest = &frames[delim + 1..];
    if rest.len() < 5 {
        return Err(Error::Malformed("fewer than 5 frames after delimiter"));
    }
    let (sig, header, parent, metadata, content) =
        (&rest[0], &rest[1], &rest[2], &rest[3], &rest[4]);
    if !signer.verify(sig, [header, parent, metadata, content]) {
        return Err(Error::BadSignature);
    }
    let header: Header = serde_json::from_slice(header)?;
    let parent_header = crate::ws::parse_parent(parent)?;
    let metadata = crate::message::parse_metadata(metadata)?;
    let content = serde_json::from_slice(content)?;
    Ok(WireMessage {
        identities: frames[..delim].to_vec(),
        message: Message {
            header,
            parent_header,
            metadata,
            content,
            buffers: rest[5..].to_vec(),
        },
    })
}

/// Signatures already accepted on one connection, as `jupyter_client`'s
/// `Session.digest_history`: a signed message seen twice is a replay. Up
/// to `size` signatures are kept; past that a tenth of them, picked
/// arbitrarily, are forgotten, as there. Share one across a connection's
/// channels, as one `Session` is.
#[derive(Debug)]
pub struct DigestHistory {
    seen: std::sync::Mutex<std::collections::HashSet<Bytes>>,
    size: usize,
}

impl Default for DigestHistory {
    fn default() -> Self {
        // `jupyter_client`'s `digest_history_size`.
        DigestHistory::new(1 << 16)
    }
}

impl DigestHistory {
    pub fn new(size: usize) -> Self {
        DigestHistory {
            seen: Default::default(),
            size: size.max(1),
        }
    }

    /// Record `signature`; `false` if it was already there.
    fn insert(&self, signature: &Bytes) -> bool {
        let mut seen = self.seen.lock().unwrap_or_else(|e| e.into_inner());
        if !seen.insert(signature.clone()) {
            return false;
        }
        if seen.len() > self.size {
            // `HashSet` iterates in a per-process random order.
            let cull: Vec<Bytes> = seen.iter().take(self.size.div_ceil(10)).cloned().collect();
            for s in cull {
                seen.remove(&s);
            }
        }
        true
    }
}

/// [`decode`], refusing a signed message whose signature `history` has
/// already seen with [`Error::DuplicateSignature`]. Unsigned connections
/// have no signatures to compare and are not checked.
pub fn decode_once(
    signer: &Signer,
    history: &DigestHistory,
    frames: &[Bytes],
) -> Result<WireMessage> {
    let w = decode(signer, frames)?;
    if signer.key.is_some() {
        let delim = frames.len() - 6 - w.message.buffers.len();
        if !history.insert(&frames[delim + 1]) {
            return Err(Error::DuplicateSignature);
        }
    }
    Ok(w)
}

/// The iopub topic frame a kernel publishes under. Hosts subscribe to
/// everything, so this only matters for a host that implements the kernel
/// side (an in-process kernel, for example).
pub fn iopub_topic(kernel_id: &str, msg_type: &str) -> Bytes {
    Bytes::from(format!("kernel.{kernel_id}.{msg_type}"))
}

/// Which socket type each channel uses on the host side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocketKind {
    Dealer,
    Sub,
}

pub fn host_socket_kind(channel: Channel) -> SocketKind {
    match channel {
        Channel::IoPub => SocketKind::Sub,
        _ => SocketKind::Dealer,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hmac_matches_python_hmac_sha256() {
        // python: hmac.new(b"secret-key", sha256) updated with the four parts
        let s = Signer::new("hmac-sha256", b"secret-key").unwrap();
        let sig = s.sign([br#"{"a":1}"#, b"{}", b"{}", br#"{"code":"1+1"}"#]);
        assert_eq!(
            sig,
            "ac38879172351d3dcc909177b26e1caf096c9c02e59dc737cad7a4d9283c3250"
        );
        assert!(s.verify(
            sig.as_bytes(),
            [br#"{"a":1}"#, b"{}", b"{}", br#"{"code":"1+1"}"#]
        ));
        assert!(!s.verify(
            sig.as_bytes(),
            [br#"{"a":2}"#, b"{}", b"{}", br#"{"code":"1+1"}"#]
        ));
    }

    #[test]
    fn encode_decode_round_trip_with_identities_and_buffers() {
        let s = Signer::new("hmac-sha256", b"k").unwrap();
        let m = Message::new(
            "execute_request",
            "sess",
            "me",
            &serde_json::json!({"code": "1"}),
        )
        .with_buffers(vec![Bytes::from_static(b"\xff\x00")]);
        let ids = vec![Bytes::from_static(b"peer-1")];
        let frames = encode(&s, &ids, &m).unwrap();
        assert_eq!(frames[1].as_ref(), DELIMITER);
        let w = decode(&s, &frames).unwrap();
        assert_eq!(w.identities, ids);
        assert_eq!(w.message, m);
    }

    #[test]
    fn bad_signature_is_rejected_and_unsigned_is_accepted() {
        let s = Signer::new("hmac-sha256", b"k").unwrap();
        let m = Message::new("status", "s", "u", &serde_json::json!({}));
        let mut frames = encode(&s, &[], &m).unwrap();
        frames[1] = Bytes::from_static(b"00");
        assert!(matches!(decode(&s, &frames), Err(Error::BadSignature)));
        let u = Signer::unsigned();
        let frames = encode(&u, &[], &m).unwrap();
        assert_eq!(frames[1].len(), 0);
        assert!(decode(&u, &frames).is_ok());
    }

    #[test]
    fn decodes_sparse_header_and_null_metadata() {
        // What a nonconforming kernel might send: no username/date/version,
        // `null` metadata.
        let s = Signer::new("hmac-sha256", b"k").unwrap();
        let header: &[u8] = br#"{"msg_id":"r1","session":"s","msg_type":"kernel_info_reply"}"#;
        let parent: &[u8] = br#"{"msg_id":"q1","session":"s","msg_type":"kernel_info_request"}"#;
        let sig = s.sign([header, parent, b"null", b"{}"]);
        let frames: Vec<Bytes> = [DELIMITER, sig.as_bytes(), header, parent, b"null", b"{}"]
            .into_iter()
            .map(Bytes::copy_from_slice)
            .collect();
        let w = decode(&s, &frames).unwrap();
        assert_eq!(w.message.header.version, "");
        assert_eq!(w.message.parent_id(), Some("q1"));
        assert!(w.message.metadata.is_empty());
    }

    #[test]
    fn replayed_messages_are_refused() {
        let s = Signer::new("hmac-sha256", b"k").unwrap();
        let history = DigestHistory::new(20);
        let m = Message::new("status", "s", "u", &serde_json::json!({}))
            .with_buffers(vec![Bytes::from_static(b"buf")]);
        let ids = vec![Bytes::from_static(b"topic")];
        let frames = encode(&s, &ids, &m).unwrap();
        assert_eq!(decode_once(&s, &history, &frames).unwrap().message, m);
        assert!(matches!(
            decode_once(&s, &history, &frames),
            Err(Error::DuplicateSignature)
        ));
        // A forged copy fails on its signature and is not remembered.
        let mut bad = encode(&s, &[], &m).unwrap();
        bad[1] = Bytes::from_static(b"00");
        assert!(matches!(
            decode_once(&s, &history, &bad),
            Err(Error::BadSignature)
        ));
        // Fresh messages keep coming past the size; the history stays bounded.
        for i in 0..100 {
            let m = Message::new("status", "s", "u", &serde_json::json!({ "i": i }));
            decode_once(&s, &history, &encode(&s, &[], &m).unwrap()).unwrap();
        }
        assert!(history.seen.lock().unwrap().len() <= 20);
        // Unsigned: nothing to compare.
        let u = Signer::unsigned();
        let frames = encode(&u, &[], &m).unwrap();
        decode_once(&u, &history, &frames).unwrap();
        decode_once(&u, &history, &frames).unwrap();
    }

    #[test]
    fn unknown_scheme_rejected() {
        assert!(matches!(
            Signer::new("hmac-md5", b"k"),
            Err(Error::UnsupportedScheme(_))
        ));
    }
}
