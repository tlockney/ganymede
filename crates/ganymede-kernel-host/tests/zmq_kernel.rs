//! The ZeroMQ transport against a kernel that binds real sockets: ROUTER
//! for shell and control, PUB for iopub, REP for heartbeat, signed with
//! the connection key. Checks the startup nudge against ROUTERs, the
//! heartbeat, and binary buffers through the real framing.
#![cfg(feature = "zmq")]

use bytes::Bytes;
use ganymede_kernel_host::protocol::wire;
use ganymede_kernel_host::protocol::{Channel, ConnectionInfo, Message};
use ganymede_kernel_host::zmq::ZmqTransport;
use ganymede_kernel_host::KernelSession;
use std::time::Duration;
use zeromq::{Socket, SocketRecv, SocketSend, ZmqMessage};

/// The port a socket bound to `tcp://127.0.0.1:0` got. Binding port 0
/// lets the OS pick, so parallel tests can never collide the way probing
/// for a free port and binding it later can.
fn port(e: zeromq::Endpoint) -> u16 {
    match e {
        zeromq::Endpoint::Tcp(_, port) => port,
        other => panic!("not a TCP endpoint: {other}"),
    }
}

const ANY: &str = "tcp://127.0.0.1:0";

struct Sockets {
    shell: zeromq::RouterSocket,
    control: zeromq::RouterSocket,
    stdin: zeromq::RouterSocket,
    iopub: zeromq::PubSocket,
    hb: zeromq::RepSocket,
}

/// The kernel's sockets, bound, and the connection info that reaches them.
async fn bind() -> (ConnectionInfo, Sockets) {
    let mut k = Sockets {
        shell: zeromq::RouterSocket::new(),
        control: zeromq::RouterSocket::new(),
        stdin: zeromq::RouterSocket::new(),
        iopub: zeromq::PubSocket::new(),
        hb: zeromq::RepSocket::new(),
    };
    let ports = [
        port(k.shell.bind(ANY).await.unwrap()),
        port(k.iopub.bind(ANY).await.unwrap()),
        port(k.stdin.bind(ANY).await.unwrap()),
        port(k.control.bind(ANY).await.unwrap()),
        port(k.hb.bind(ANY).await.unwrap()),
    ];
    let mut info = ConnectionInfo::new_tcp("127.0.0.1", ports);
    info.key = "a-secret-key".into();
    (info, k)
}

fn zmsg(frames: Vec<Bytes>) -> ZmqMessage {
    ZmqMessage::try_from(frames).unwrap()
}

/// Answers `kernel_info_request` on shell and control, echoes `comm_msg`
/// on iopub with its buffers, and echoes heartbeats.
async fn kernel(info: ConnectionInfo, k: Sockets) {
    let signer = info.signer().unwrap();
    let Sockets {
        mut shell,
        mut control,
        mut stdin,
        mut iopub,
        mut hb,
    } = k;
    let topic = |t: &str| vec![Bytes::from(format!("kernel.test.{t}"))];

    loop {
        let (router, frames) = tokio::select! {
            m = shell.recv() => (Channel::Shell, m.unwrap().into_vec()),
            m = control.recv() => (Channel::Control, m.unwrap().into_vec()),
            m = hb.recv() => {
                hb.send(m.unwrap()).await.unwrap();
                continue;
            }
            _ = stdin.recv() => continue,
        };
        // Unsigned or wrongly signed: dropped, as a kernel does.
        let Ok(w) = wire::decode(&signer, &frames) else {
            continue;
        };
        let req = &w.message;
        let publish = |msg: Message| wire::encode(&signer, &topic(msg.msg_type()), &msg).unwrap();
        let status = |state: &str| {
            publish(Message::reply_to(
                req,
                "status",
                &serde_json::json!({ "execution_state": state }),
            ))
        };
        iopub.send(zmsg(status("busy"))).await.unwrap();
        match req.msg_type() {
            "kernel_info_request" => {
                let reply = Message::reply_to(
                    req,
                    "kernel_info_reply",
                    &serde_json::json!({
                        "status": "ok", "protocol_version": "5.4",
                        "implementation": "zmq-test", "implementation_version": "0",
                        "language_info": {"name": "test", "version": "0", "mimetype": "text/plain", "file_extension": ".txt"},
                        "banner": "", "help_links": [],
                    }),
                );
                let frames = wire::encode(&signer, &w.identities, &reply).unwrap();
                let socket = match router {
                    Channel::Shell => &mut shell,
                    _ => &mut control,
                };
                socket.send(zmsg(frames)).await.unwrap();
            }
            "comm_msg" => {
                let echo = Message::reply_to(req, "comm_msg", &req.content)
                    .with_buffers(req.buffers.clone());
                iopub.send(zmsg(publish(echo))).await.unwrap();
            }
            _ => {}
        }
        iopub.send(zmsg(status("idle"))).await.unwrap();
    }
}

#[tokio::test]
async fn nudge_heartbeat_and_binary_buffers_over_zmq() {
    let (info, sockets) = bind().await;
    let k = tokio::spawn(kernel(info.clone(), sockets));
    let transport = ZmqTransport::connect(&info).await.unwrap();
    let s = KernelSession::start_with(transport, None, None);
    let ki = s.nudge(Duration::from_secs(20)).await.unwrap();
    assert_eq!(ki.implementation, "zmq-test");
    assert!(s.heartbeat(Duration::from_secs(5)).await.unwrap());

    // Buffers that look like framing must come back byte for byte.
    let buffers: Vec<Bytes> = vec![
        Bytes::from_static(wire::DELIMITER),
        Bytes::new(),
        Bytes::from((0..=255u8).collect::<Vec<_>>()),
        Bytes::from(vec![0u8; 100_000]),
    ];
    let mut sub = s.subscribe();
    let m = s
        .message(
            "comm_msg",
            &serde_json::json!({"comm_id": "c1", "data": {"x": 1}}),
        )
        .with_buffers(buffers.clone());
    let id = m.header.msg_id.clone();
    s.send(Channel::Shell, m).await.unwrap();
    let echo = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let (ch, m) = sub.recv().await.unwrap();
            if ch == Channel::IoPub && m.msg_type() == "comm_msg" && m.parent_id() == Some(&id) {
                return m;
            }
        }
    })
    .await
    .expect("no echo");
    assert_eq!(echo.content["data"], serde_json::json!({"x": 1}));
    assert_eq!(echo.buffers, buffers);

    s.shutdown(false, Duration::from_millis(100)).await.unwrap();
    k.abort();
}

#[tokio::test]
async fn a_wrong_key_is_dropped_not_delivered() {
    let (info, sockets) = bind().await;
    let k = tokio::spawn(kernel(info.clone(), sockets));
    let mut wrong = info.clone();
    wrong.key = "not-the-key".into();
    let transport = ZmqTransport::connect(&wrong).await.unwrap();
    let s = KernelSession::start_with(transport, None, None);
    // The kernel drops every message: the nudge never completes.
    let r = s.nudge(Duration::from_secs(2)).await;
    assert!(r.is_err());
    k.abort();
}
