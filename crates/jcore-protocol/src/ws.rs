//! The two websocket codecs a Jupyter server speaks on
//! `/api/kernels/{id}/channels`.
//!
//! - [`v1`]: subprotocol `v1.kernel.websocket.jupyter.org`. One binary frame
//!   per message: an 8-byte little-endian offset count, that many 8-byte
//!   little-endian offsets, the channel name, then header, parent_header,
//!   metadata, content as JSON, then each binary buffer. Layout copied from
//!   `jupyter_server.services.kernels.connection.base`.
//! - [`legacy`]: no subprotocol. A JSON text frame with a top-level
//!   `channel` key; messages with buffers go as a binary frame with a
//!   big-endian offset table in front of the JSON.

use crate::{Channel, Error, Message, Result};
use bytes::Bytes;

pub mod v1 {
    use super::*;

    pub const SUBPROTOCOL: &str = "v1.kernel.websocket.jupyter.org";

    pub fn encode(channel: Channel, msg: &Message) -> Result<Vec<u8>> {
        let mut parts: Vec<Vec<u8>> = vec![
            serde_json::to_vec(&msg.header)?,
            match &msg.parent_header {
                Some(h) => serde_json::to_vec(h)?,
                None => b"{}".to_vec(),
            },
            serde_json::to_vec(&msg.metadata)?,
            serde_json::to_vec(&msg.content)?,
        ];
        parts.extend(msg.buffers.iter().map(|b| b.to_vec()));
        let channel = channel.as_str().as_bytes();

        let mut offsets: Vec<u64> = Vec::with_capacity(parts.len() + 2);
        offsets.push(8 * (1 + 1 + parts.len() as u64 + 1));
        offsets.push(offsets[0] + channel.len() as u64);
        for p in &parts {
            offsets.push(offsets.last().unwrap() + p.len() as u64);
        }

        let mut out = Vec::with_capacity(*offsets.last().unwrap() as usize);
        out.extend_from_slice(&(offsets.len() as u64).to_le_bytes());
        for o in &offsets {
            out.extend_from_slice(&o.to_le_bytes());
        }
        out.extend_from_slice(channel);
        for p in parts {
            out.extend_from_slice(&p);
        }
        Ok(out)
    }

    pub fn decode(frame: &[u8]) -> Result<(Channel, Message)> {
        let read_u64 = |at: usize| -> Result<usize> {
            let b = frame
                .get(at..at + 8)
                .ok_or(Error::Malformed("ws v1 frame truncated"))?;
            Ok(u64::from_le_bytes(b.try_into().unwrap()) as usize)
        };
        let n = read_u64(0)?;
        if n < 6 {
            return Err(Error::Malformed("ws v1 frame needs at least 6 offsets"));
        }
        let offsets: Vec<usize> = (0..n)
            .map(|i| read_u64(8 * (i + 1)))
            .collect::<Result<_>>()?;
        let slice = |i: usize| -> Result<&[u8]> {
            frame
                .get(offsets[i]..offsets[i + 1])
                .ok_or(Error::Malformed("ws v1 offset out of range"))
        };
        let channel: Channel = std::str::from_utf8(slice(0)?)
            .map_err(|_| Error::Malformed("ws v1 channel not utf-8"))?
            .parse()?;
        let header = serde_json::from_slice(slice(1)?)?;
        let parent_header = parse_parent(slice(2)?)?;
        let metadata = serde_json::from_slice(slice(3)?)?;
        let content = serde_json::from_slice(slice(4)?)?;
        let buffers = (5..n - 1)
            .map(|i| slice(i).map(Bytes::copy_from_slice))
            .collect::<Result<_>>()?;
        Ok((
            channel,
            Message {
                header,
                parent_header,
                metadata,
                content,
                buffers,
            },
        ))
    }
}

pub mod legacy {
    use super::*;
    use serde_json::Value;

    /// Encode as a text frame (no buffers) or a binary frame (with buffers).
    pub enum Frame {
        Text(String),
        Binary(Vec<u8>),
    }

    pub fn encode(channel: Channel, msg: &Message) -> Result<Frame> {
        let mut v = serde_json::to_value(msg)?;
        v["channel"] = Value::String(channel.as_str().to_string());
        if msg.buffers.is_empty() {
            return Ok(Frame::Text(serde_json::to_string(&v)?));
        }
        // jupyter_server.base.zmqhandlers.serialize_binary_message
        let json = serde_json::to_vec(&v)?;
        let nbufs = 1 + msg.buffers.len();
        let mut offsets = vec![4 * (nbufs as u32 + 1)];
        let mut lens = vec![json.len() as u32];
        lens.extend(msg.buffers.iter().map(|b| b.len() as u32));
        for l in &lens[..lens.len() - 1] {
            offsets.push(offsets.last().unwrap() + l);
        }
        let mut out = Vec::new();
        out.extend_from_slice(&(nbufs as u32).to_be_bytes());
        for o in offsets {
            out.extend_from_slice(&o.to_be_bytes());
        }
        out.extend_from_slice(&json);
        for b in &msg.buffers {
            out.extend_from_slice(b);
        }
        Ok(Frame::Binary(out))
    }

    pub fn decode_text(text: &str) -> Result<(Channel, Message)> {
        let mut v: Value = serde_json::from_str(text)?;
        let channel: Channel = v
            .get("channel")
            .and_then(Value::as_str)
            .ok_or(Error::Malformed("legacy ws message has no channel"))?
            .parse()?;
        v.as_object_mut().unwrap().remove("channel");
        let msg: Message = serde_json::from_value(v)?;
        Ok((channel, msg))
    }

    pub fn decode_binary(frame: &[u8]) -> Result<(Channel, Message)> {
        let read_u32 = |at: usize| -> Result<usize> {
            let b = frame
                .get(at..at + 4)
                .ok_or(Error::Malformed("legacy binary frame truncated"))?;
            Ok(u32::from_be_bytes(b.try_into().unwrap()) as usize)
        };
        let nbufs = read_u32(0)?;
        let offsets: Vec<usize> = (0..nbufs)
            .map(|i| read_u32(4 * (i + 1)))
            .collect::<Result<_>>()?;
        let end = |i: usize| {
            if i + 1 < nbufs {
                offsets[i + 1]
            } else {
                frame.len()
            }
        };
        let json = frame
            .get(offsets[0]..end(0))
            .ok_or(Error::Malformed("legacy offset out of range"))?;
        let (channel, mut msg) = decode_text(
            std::str::from_utf8(json).map_err(|_| Error::Malformed("legacy json not utf-8"))?,
        )?;
        for (i, start) in offsets.iter().enumerate().skip(1) {
            let b = frame
                .get(*start..end(i))
                .ok_or(Error::Malformed("legacy offset out of range"))?;
            msg.buffers.push(Bytes::copy_from_slice(b));
        }
        Ok((channel, msg))
    }
}

pub(crate) fn parse_parent(b: &[u8]) -> Result<Option<crate::Header>> {
    let v: serde_json::Value = serde_json::from_slice(b)?;
    match v {
        serde_json::Value::Null => Ok(None),
        serde_json::Value::Object(ref m) if m.is_empty() => Ok(None),
        other => Ok(Some(serde_json::from_value(other)?)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content::ExecuteRequest;

    fn sample() -> Message {
        let parent = Message::new("kernel_info_request", "sess", "me", &serde_json::json!({}));
        Message::reply_to(
            &parent,
            "execute_request",
            &ExecuteRequest::new("console.log('héllo')"),
        )
        .with_buffers(vec![
            Bytes::from_static(b"\x00\x01\x02"),
            Bytes::from_static(b"buffer two"),
        ])
    }

    #[test]
    fn v1_round_trip_with_buffers() {
        let m = sample();
        let frame = v1::encode(Channel::Shell, &m).unwrap();
        // offset count: channel + 4 json parts + 2 buffers = 6 parts -> 8 offsets
        assert_eq!(u64::from_le_bytes(frame[..8].try_into().unwrap()), 8);
        let (ch, back) = v1::decode(&frame).unwrap();
        assert_eq!(ch, Channel::Shell);
        assert_eq!(back, m);
    }

    #[test]
    fn v1_layout_matches_jupyter_server() {
        // Python: serialize_msg_to_ws_v1([b'{"h":1}', b'{}', b'{}', b'{}'], "iopub")
        // offsets: 8*(1+1+4+1)=56, 61, 68, 70, 72, 74 ; offset_number=6
        let m = Message {
            header: serde_json::from_str(
                r#"{"msg_id":"","session":"","username":"","date":"","msg_type":"","version":""}"#,
            )
            .unwrap(),
            parent_header: None,
            metadata: Default::default(),
            content: serde_json::json!({}),
            buffers: vec![],
        };
        let frame = v1::encode(Channel::IoPub, &m).unwrap();
        assert_eq!(u64::from_le_bytes(frame[..8].try_into().unwrap()), 6);
        assert_eq!(u64::from_le_bytes(frame[8..16].try_into().unwrap()), 56);
        assert_eq!(&frame[56..61], b"iopub");
        assert_eq!(
            frame.len() as u64,
            u64::from_le_bytes(frame[48..56].try_into().unwrap())
        );
    }

    #[test]
    fn legacy_text_and_binary_round_trip() {
        let mut m = sample();
        m.buffers.clear();
        let legacy::Frame::Text(t) = legacy::encode(Channel::Control, &m).unwrap() else {
            panic!("expected text")
        };
        assert!(t.contains("\"channel\":\"control\""));
        let (ch, back) = legacy::decode_text(&t).unwrap();
        assert_eq!((ch, back), (Channel::Control, m));

        let m = sample();
        let legacy::Frame::Binary(b) = legacy::encode(Channel::Shell, &m).unwrap() else {
            panic!("expected binary")
        };
        assert_eq!(u32::from_be_bytes(b[..4].try_into().unwrap()), 3);
        let (ch, back) = legacy::decode_binary(&b).unwrap();
        assert_eq!((ch, back), (Channel::Shell, m));
    }
}
