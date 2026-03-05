//! Fragmentation unit tests — RFC 6455 §5.4 reassembly.

use super::frame::{encode_frame, Frame, Opcode};
use super::stream::MAX_FRAGMENT_SIZE;
use super::{Message, WsError, WsStream};

/// Build a raw frame with explicit FIN and opcode.
fn raw_frame(fin: bool, opcode: Opcode, payload: &[u8]) -> Vec<u8> {
    let frame = Frame { fin, opcode, payload: payload.to_vec() };
    let mut buf = Vec::new();
    encode_frame(&frame, &mut buf);
    buf
}

/// Create a `WsStream` backed by a loopback TCP connection, pre-filled
/// with `data`. Returns `(ws_stream, client_tcp)` — dropping client
/// causes EOF on the server side after buffered data is consumed.
fn ws_with_data(data: Vec<u8>) -> (WsStream, moduvex_runtime::TcpStream) {
    use moduvex_runtime::{TcpListener, TcpStream};
    use std::net::SocketAddr;

    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let listener = TcpListener::bind(addr).unwrap();
    let bound_addr = listener.local_addr().unwrap();

    let (client, server) = moduvex_runtime::block_on_with_spawn(async move {
        let connect = TcpStream::connect(bound_addr);
        let accept = listener.accept();
        let client = connect.await.unwrap();
        let (server, _) = accept.await.unwrap();
        (client, server)
    });

    let stream = crate::server::tls::Stream::Plain(server);
    let mut ws = WsStream::new(stream);
    ws.prepend_read_buf(data);
    (ws, client)
}

/// Feed frames, recv one message.
fn feed_and_recv(frames: &[Vec<u8>]) -> Result<Message, WsError> {
    let mut all = Vec::new();
    for f in frames {
        all.extend_from_slice(f);
    }
    let (mut ws, _client) = ws_with_data(all);
    moduvex_runtime::block_on_with_spawn(async move { ws.recv().await })
}

#[test]
fn fragmented_text_two_frames() {
    let f1 = raw_frame(false, Opcode::Text, b"hel");
    let f2 = raw_frame(true, Opcode::Continuation, b"lo");
    let msg = feed_and_recv(&[f1, f2]).unwrap();
    assert_eq!(msg, Message::Text("hello".to_string()));
}

#[test]
fn fragmented_binary_three_frames() {
    let f1 = raw_frame(false, Opcode::Binary, &[1, 2]);
    let f2 = raw_frame(false, Opcode::Continuation, &[3, 4]);
    let f3 = raw_frame(true, Opcode::Continuation, &[5]);
    let msg = feed_and_recv(&[f1, f2, f3]).unwrap();
    assert_eq!(msg, Message::Binary(vec![1, 2, 3, 4, 5]));
}

#[test]
fn continuation_without_start_is_error() {
    let f = raw_frame(true, Opcode::Continuation, b"orphan");
    let result = feed_and_recv(&[f]);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("continuation"));
}

#[test]
fn new_data_frame_during_fragment_is_error() {
    let f1 = raw_frame(false, Opcode::Text, b"start");
    let f2 = raw_frame(true, Opcode::Text, b"interrupt");
    let result = feed_and_recv(&[f1, f2]);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("new data frame"));
}

#[test]
fn control_frame_interleaved_mid_fragment() {
    let f1 = raw_frame(false, Opcode::Text, b"hel");
    let ping = raw_frame(true, Opcode::Ping, b"p");
    let f2 = raw_frame(true, Opcode::Continuation, b"lo");

    let mut all = Vec::new();
    for f in &[f1, ping, f2] {
        all.extend_from_slice(f);
    }
    let (mut ws, _client) = ws_with_data(all);

    let msgs: Vec<Message> = moduvex_runtime::block_on_with_spawn(async move {
        let mut msgs = Vec::new();
        if let Ok(m) = ws.recv().await { msgs.push(m); }
        if let Ok(m) = ws.recv().await { msgs.push(m); }
        msgs
    });

    assert_eq!(msgs.len(), 2);
    assert_eq!(msgs[0], Message::Ping(b"p".to_vec()));
    assert_eq!(msgs[1], Message::Text("hello".to_string()));
}

#[test]
fn oversized_fragment_buffer_is_error() {
    let big = vec![0xAA; MAX_FRAGMENT_SIZE + 1];
    let f1 = raw_frame(false, Opcode::Binary, &[]);
    let f2 = raw_frame(true, Opcode::Continuation, &big);
    let result = feed_and_recv(&[f1, f2]);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("limit"));
}
