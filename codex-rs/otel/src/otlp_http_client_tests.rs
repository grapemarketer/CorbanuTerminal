use super::*;
use pretty_assertions::assert_eq;
use std::future::Future;
use std::io::Write;
use std::net::TcpListener;
use std::pin::pin;
use std::task::Context;
use std::task::Poll;
use std::task::Waker;
use std::thread::JoinHandle;

/// Serves one HTTP/1.1 response, then closes the connection.
fn serve_once(status_line: &'static str, body: Vec<u8>) -> (String, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let url = format!("http://{}/v1/traces", listener.local_addr().expect("addr"));
    let handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        let mut request = Vec::new();
        let mut buf = [0_u8; 1024];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let read = stream.read(&mut buf).expect("read request");
            assert!(read > 0, "client closed before sending headers");
            request.extend_from_slice(&buf[..read]);
        }
        let head = format!(
            "HTTP/1.1 {status_line}\r\ncontent-length: {}\r\nretry-after: 7\r\nconnection: close\r\n\r\n",
            body.len()
        );
        stream.write_all(head.as_bytes()).expect("write head");
        // The client may stop reading once it hits the body limit.
        let _ = stream.write_all(&body);
    });
    (url, handle)
}

fn request(url: &str) -> Request<Bytes> {
    Request::post(url)
        .body(Bytes::from_static(b"payload"))
        .expect("request")
}

/// The blocking adapter never yields, so a single poll completes it.
fn poll_ready<F: Future>(future: F) -> F::Output {
    let mut future = pin!(future);
    match future
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
    {
        Poll::Ready(output) => output,
        Poll::Pending => panic!("blocking OTLP client future was pending"),
    }
}

fn blocking_send(url: String) -> Result<Response<Bytes>, String> {
    std::thread::spawn(move || {
        let client = BlockingOtlpHttpClient::new(reqwest::blocking::Client::new());
        poll_ready(client.send_bytes(request(&url))).map_err(|error| error.to_string())
    })
    .join()
    .expect("blocking client thread")
}

fn async_send(url: String) -> Result<Response<Bytes>, String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    runtime.block_on(async move {
        let client = AsyncOtlpHttpClient::new(reqwest::Client::new());
        client
            .send_bytes(request(&url))
            .await
            .map_err(|error| error.to_string())
    })
}

fn assert_error_status_is_returned(send: fn(String) -> Result<Response<Bytes>, String>) {
    let (url, server) = serve_once("503 Service Unavailable", b"busy".to_vec());
    let response = send(url).expect("non-success statuses are responses, not errors");
    server.join().expect("server thread");

    assert_eq!(response.status(), http::StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        response
            .headers()
            .get("retry-after")
            .and_then(|value| value.to_str().ok()),
        Some("7")
    );
    assert_eq!(response.body(), &Bytes::from_static(b"busy"));
}

fn assert_oversized_body_is_rejected(send: fn(String) -> Result<Response<Bytes>, String>) {
    let (url, server) = serve_once("200 OK", vec![b'x'; MAX_RESPONSE_BODY_BYTES + 1]);
    let error = send(url).expect_err("oversized body must be rejected");
    server.join().expect("server thread");

    assert_eq!(error, ResponseBodyTooLarge::new().to_string());
}

#[test]
fn blocking_client_returns_error_status_as_response() {
    assert_error_status_is_returned(blocking_send);
}

#[test]
fn async_client_returns_error_status_as_response() {
    assert_error_status_is_returned(async_send);
}

#[test]
fn blocking_client_rejects_oversized_response_body() {
    assert_oversized_body_is_rejected(blocking_send);
}

#[test]
fn async_client_rejects_oversized_response_body() {
    assert_oversized_body_is_rejected(async_send);
}
