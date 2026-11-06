use super::*;
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    time::{Duration, Instant},
};
fn receive(socket: &mut TcpStream) -> (String, Vec<u8>) {
    socket
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut bytes = Vec::new();
    let mut one = [0u8; 1];
    while !bytes.ends_with(b"\r\n\r\n") {
        socket.read_exact(&mut one).unwrap();
        bytes.push(one[0]);
        assert!(bytes.len() < 32768);
    }
    let headers = String::from_utf8(bytes).unwrap();
    let length = headers
        .lines()
        .find_map(|s| s.strip_prefix("Content-Length: "))
        .unwrap()
        .parse()
        .unwrap();
    let mut body = vec![0; length];
    socket.read_exact(&mut body).unwrap();
    (headers, body)
}
fn reply(socket: &mut TcpStream, body: &str) {
    write!(
        socket,
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
}
fn wait(client: &mut Client) -> Update {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut collected = Update::default();
    loop {
        let update = client.poll();
        collected.delta.push_str(&update.delta);
        if update.models.is_some() {
            collected.models = update.models;
        }
        if update.done.is_some() {
            collected.done = update.done;
            return collected;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
}
#[test]
fn json_escapes_and_cloud_models_are_not_mistaken_for_local_models() {
    let json = Json::new().unwrap();
    let input = "Русский \"текст\"\n🚀\\\0";
    let body = request(
        &json,
        "model:local",
        &[Message {
            role: "user",
            text: input.into(),
        }],
        8,
    )
    .unwrap();
    let value = object(&json, body.as_bytes()).unwrap();
    let messages = value.GetNamedArray(&HSTRING::from("messages")).unwrap();
    let item = messages.GetAt(1).unwrap().GetObject().unwrap();
    assert_eq!(json::string(&item, "content").unwrap(), input);
    assert!(json::remote(
        &object(&json, br#"{"remote_host":"https://remote.invalid"}"#).unwrap()
    ));
    assert!(!json::remote(&object(&json, b"{}").unwrap()));
    assert!(object(&json, &[b'['; 65]).is_err());
}
#[test]
fn model_list_is_bounded_sorted_and_excludes_cloud_aliases() {
    let server = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let addr = server.local_addr().unwrap();
    let peer = std::thread::spawn(move || {
        let (mut socket, _) = server.accept().unwrap();
        assert!(receive(&mut socket).0.starts_with("GET /api/tags "));
        reply(
            &mut socket,
            r#"{"models":[{"name":"z"},{"name":"remote-alias","remote_host":"https://remote.invalid"},{"name":"a"},{"name":"a"}]}"#,
        );
    });
    let mut client = Client::at(addr).unwrap();
    client.refresh().unwrap();
    assert!(client.refresh().is_err());
    let result = wait(&mut client);
    assert!(result.done.unwrap().is_ok());
    assert_eq!(result.models.unwrap(), vec!["a", "z"]);
    peer.join().unwrap();
}
#[test]
fn chat_stream_arrives_before_done_and_survives_split_utf8_http_chunks() {
    let server = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let addr = server.local_addr().unwrap();
    let (release, gate) = mpsc::channel();
    let peer = std::thread::spawn(move || {
        let (mut show, _) = server.accept().unwrap();
        assert!(receive(&mut show).0.starts_with("POST /api/show "));
        reply(&mut show, "{}");
        let (mut socket, _) = server.accept().unwrap();
        let (headers, body) = receive(&mut socket);
        assert!(headers.starts_with("POST /api/chat "));
        let body = String::from_utf8(body).unwrap();
        assert!(body.contains("synthetic-only"));
        assert!(body.contains("\"stream\":true"));
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n")
            .unwrap();
        let first = "{\"message\":{\"content\":\"Привет 🚀\"},\"done\":false}\n";
        // One-byte HTTP chunks deliberately split UTF-8 and JSON tokens.
        for byte in first.as_bytes() {
            socket
                .write_all(&[b'1', b'\r', b'\n', *byte, b'\r', b'\n'])
                .unwrap();
        }
        gate.recv_timeout(Duration::from_secs(5)).unwrap();
        let last = b"{\"message\":{\"content\":\"!\"},\"done\":true}\n";
        write!(socket, "{:x}\r\n", last.len()).unwrap();
        socket.write_all(last).unwrap();
        socket.write_all(b"\r\n0\r\n\r\n").unwrap();
    });
    let mut client = Client::at(addr).unwrap();
    client
        .send(
            "local".into(),
            vec![Message {
                role: "user",
                text: "synthetic-only".into(),
            }],
        )
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut first = String::new();
    while first.is_empty() {
        let update = client.poll();
        assert!(update.done.is_none());
        first.push_str(&update.delta);
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(first, "Привет 🚀");
    assert!(client.busy());
    release.send(()).unwrap();
    let rest = wait(&mut client);
    assert_eq!(rest.delta, "!");
    assert!(rest.done.unwrap().is_ok());
    peer.join().unwrap();
}
#[test]
fn cancellation_interrupts_a_silent_server_without_joining_the_ui() {
    let server = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let addr = server.local_addr().unwrap();
    let (ready, arrived) = mpsc::channel();
    let (release, gate) = mpsc::channel();
    let peer = std::thread::spawn(move || {
        let (mut socket, _) = server.accept().unwrap();
        receive(&mut socket);
        ready.send(()).unwrap();
        gate.recv_timeout(Duration::from_secs(5)).unwrap();
    });
    let mut client = Client::at(addr).unwrap();
    client.refresh().unwrap();
    arrived.recv_timeout(Duration::from_secs(5)).unwrap();
    client.cancel();
    let result = wait(&mut client);
    release.send(()).unwrap();
    assert_eq!(result.done.unwrap().unwrap_err(), "Запрос отменён");
    peer.join().unwrap();
}
#[test]
fn cloud_show_metadata_prevents_sending_the_chat_prompt() {
    let server = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let addr = server.local_addr().unwrap();
    let peer = std::thread::spawn(move || {
        let (mut socket, _) = server.accept().unwrap();
        let (header, body) = receive(&mut socket);
        assert!(header.starts_with("POST /api/show "));
        assert!(
            !String::from_utf8(body)
                .unwrap()
                .contains("private-synthetic")
        );
        reply(&mut socket, r#"{"remote_model":"cloud-secret"}"#);
    });
    let mut client = Client::at(addr).unwrap();
    client
        .send(
            "alias".into(),
            vec![Message {
                role: "user",
                text: "private-synthetic".into(),
            }],
        )
        .unwrap();
    assert!(
        wait(&mut client)
            .done
            .unwrap()
            .unwrap_err()
            .contains("облачная")
    );
    peer.join().unwrap();
}
#[test]
fn truncated_stream_and_http_errors_preserve_partial_text() {
    let server = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let addr = server.local_addr().unwrap();
    let peer = std::thread::spawn(move || {
        let (mut show, _) = server.accept().unwrap();
        receive(&mut show);
        reply(&mut show, "{}");
        let (mut socket, _) = server.accept().unwrap();
        receive(&mut socket);
        reply(
            &mut socket,
            "{\"message\":{\"content\":\"kept\"},\"done\":false}\n",
        );
    });
    let mut client = Client::at(addr).unwrap();
    client
        .send(
            "local".into(),
            vec![Message {
                role: "user",
                text: "test".into(),
            }],
        )
        .unwrap();
    let result = wait(&mut client);
    assert_eq!(result.delta, "kept");
    assert!(result.done.unwrap().unwrap_err().contains("оборвался"));
    peer.join().unwrap();
}
#[test]
fn timeout_and_ambiguous_http_framing_are_rejected() {
    let server = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let addr = server.local_addr().unwrap();
    let (hold, held) = mpsc::channel();
    let peer = std::thread::spawn(move || {
        let (mut socket, _) = server.accept().unwrap();
        receive(&mut socket);
        held.recv_timeout(Duration::from_secs(5)).unwrap();
    });
    let cancel = AtomicBool::new(false);
    let result = Response::open_with_timeout(
        addr,
        "/api/tags",
        None,
        &cancel,
        Duration::from_millis(80),
        Duration::from_secs(1),
    );
    assert!(result.is_err());
    hold.send(()).unwrap();
    peer.join().unwrap();
    let server = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let addr = server.local_addr().unwrap();
    let peer = std::thread::spawn(move || {
        let (mut socket, _) = server.accept().unwrap();
        receive(&mut socket);
        socket
            .write_all(
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nContent-Length: 10\r\n\r\n",
            )
            .unwrap();
    });
    assert!(Response::open(addr, "/api/tags", None, &cancel).is_err());
    peer.join().unwrap();
}
#[test]
#[ignore = "manual local Ollama probe; no models are downloaded"]
fn real_local_ollama_stream() {
    let model =
        std::env::var("PAPERIK_TEST_OLLAMA_MODEL").expect("Set a local model name explicitly");
    let json = Json::new().unwrap();
    let cancel = AtomicBool::new(false);
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, 11434));
    let models = list(&json, address, &cancel).unwrap();
    assert!(models.contains(&model));
    let mut chunks = 0;
    let mut text = String::new();
    chat(
        &json,
        address,
        &model,
        &[Message {
            role: "user",
            text: "Ответь одним словом: готово.".into(),
        }],
        16,
        &cancel,
        |delta| {
            if !delta.is_empty() {
                chunks += 1;
                text.push_str(delta);
            }
        },
    )
    .unwrap();
    assert!(!text.trim().is_empty());
    println!(
        "LOCAL_OLLAMA models={} streamed_chunks={chunks} response_bytes={}",
        models.len(),
        text.len()
    );
}

#[test]
fn json_factories_survive_repeated_worker_apartment_restarts() {
    // No simultaneous initialized apartment can hide a stale process-wide cache.
    for _ in 0..12 {
        std::thread::spawn(|| {
            let json = Json::new().unwrap();
            let value = json.object(br#"{"content":"local"}"#).unwrap();
            assert_eq!(json::string(&value, "content").unwrap(), "local");
            assert_eq!(json.quote("text").unwrap(), "\"text\"");
        })
        .join()
        .unwrap();
    }
}
