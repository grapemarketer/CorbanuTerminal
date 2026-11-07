//! `opentelemetry_http::HttpClient` adapters for the workspace's reqwest 0.12
//! clients.
//!
//! `opentelemetry-http` 0.33 only implements `HttpClient` for reqwest 0.13,
//! whose rustls backend is aws-lc-rs with the platform verifier. Wrapping our
//! own reqwest 0.12 clients keeps the OTLP/HTTP TLS stack, custom CA and mTLS
//! handling exactly as before.
//!
//! Like the upstream adapters, these return non-success HTTP statuses as
//! `Ok(response)` so the exporter can classify them, and reject response bodies
//! larger than 4 MiB.
//!
//! `HttpClient` is declared with `#[async_trait]`; the impls below spell out
//! the expanded signature because the workspace bans depending on
//! `async-trait` directly.

use bytes::Bytes;
use bytes::BytesMut;
use http::Request;
use http::Response;
use opentelemetry_http::HttpClient;
use opentelemetry_http::HttpError;
use opentelemetry_http::ResponseBodyTooLarge;
use std::future::Future;
use std::io::Read;
use std::pin::Pin;

type SendFuture<'a> = Pin<Box<dyn Future<Output = Result<Response<Bytes>, HttpError>> + Send + 'a>>;

/// Same limit as the built-in `opentelemetry-http` clients.
const MAX_RESPONSE_BODY_BYTES: usize = 4 * 1024 * 1024;

/// Blocking client for exporters that run on their own OS threads.
#[derive(Debug)]
pub(crate) struct BlockingOtlpHttpClient(reqwest::blocking::Client);

impl BlockingOtlpHttpClient {
    pub(crate) fn new(client: reqwest::blocking::Client) -> Self {
        Self(client)
    }

    fn send_blocking(&self, request: Request<Bytes>) -> Result<Response<Bytes>, HttpError> {
        let mut response = self.0.execute(request.try_into()?)?;
        let status = response.status();
        let headers = std::mem::take(response.headers_mut());
        let mut body = Vec::with_capacity(body_capacity(response.content_length()));
        response
            .take(MAX_RESPONSE_BODY_BYTES as u64 + 1)
            .read_to_end(&mut body)?;
        if body.len() > MAX_RESPONSE_BODY_BYTES {
            return Err(Box::new(ResponseBodyTooLarge::new()));
        }
        build_response(status, headers, Bytes::from(body))
    }
}

impl HttpClient for BlockingOtlpHttpClient {
    fn send_bytes<'life0, 'async_trait>(
        &'life0 self,
        request: Request<Bytes>,
    ) -> SendFuture<'async_trait>
    where
        'life0: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move { self.send_blocking(request) })
    }
}

/// Async client for the Tokio batch span processor.
#[derive(Debug)]
pub(crate) struct AsyncOtlpHttpClient(reqwest::Client);

impl AsyncOtlpHttpClient {
    pub(crate) fn new(client: reqwest::Client) -> Self {
        Self(client)
    }

    async fn send_async(&self, request: Request<Bytes>) -> Result<Response<Bytes>, HttpError> {
        let mut response = self.0.execute(request.try_into()?).await?;
        let status = response.status();
        let headers = std::mem::take(response.headers_mut());
        let mut body = BytesMut::with_capacity(body_capacity(response.content_length()));
        while let Some(chunk) = response.chunk().await? {
            if body.len() + chunk.len() > MAX_RESPONSE_BODY_BYTES {
                return Err(Box::new(ResponseBodyTooLarge::new()));
            }
            body.extend_from_slice(&chunk);
        }
        build_response(status, headers, body.freeze())
    }
}

impl HttpClient for AsyncOtlpHttpClient {
    fn send_bytes<'life0, 'async_trait>(
        &'life0 self,
        request: Request<Bytes>,
    ) -> SendFuture<'async_trait>
    where
        'life0: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(self.send_async(request))
    }
}

fn body_capacity(content_length: Option<u64>) -> usize {
    content_length
        .unwrap_or(0)
        .min(MAX_RESPONSE_BODY_BYTES as u64) as usize
}

fn build_response(
    status: http::StatusCode,
    headers: http::HeaderMap,
    body: Bytes,
) -> Result<Response<Bytes>, HttpError> {
    let mut response = Response::builder().status(status).body(body)?;
    *response.headers_mut() = headers;
    Ok(response)
}

#[cfg(test)]
#[path = "otlp_http_client_tests.rs"]
mod tests;
