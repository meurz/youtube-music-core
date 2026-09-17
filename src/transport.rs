//! Blocking facade over cancellable async I/O. No detached request survives cancellation.
use crate::{operation, Error, Result};
use reqwest::{header::HeaderMap, RequestBuilder, StatusCode, Url};
use std::{
    io::{Cursor, Read},
    sync::OnceLock,
    time::{Duration, SystemTime},
};

pub(crate) struct Response {
    status: StatusCode,
    headers: HeaderMap,
    url: Url,
    body: Cursor<Vec<u8>>,
}
impl Response {
    pub fn status(&self) -> StatusCode {
        self.status
    }
    pub fn headers(&self) -> &HeaderMap {
        &self.headers
    }
    pub fn url(&self) -> &Url {
        &self.url
    }
}
impl Read for Response {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.body.read(buf)
    }
}

fn runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("HTTP runtime")
    })
}

pub(crate) fn retry_after(headers: &HeaderMap) -> Option<u64> {
    let value = headers.get("retry-after")?.to_str().ok()?;
    value.parse().ok().or_else(|| {
        httpdate::parse_http_date(value)
            .ok()?
            .duration_since(SystemTime::now())
            .ok()
            .map(|v| v.as_secs().saturating_add(1))
    })
}
pub(crate) fn status_error(response: &Response) -> Error {
    if response.status == StatusCode::TOO_MANY_REQUESTS {
        Error::RateLimited {
            retry_after_seconds: retry_after(&response.headers),
        }
    } else {
        Error::Http(response.status.as_u16())
    }
}
pub(crate) fn network(error: reqwest::Error) -> Error {
    if error.is_timeout() {
        Error::Timeout
    } else {
        Error::Network("transport connection or response failed".into())
    }
}

pub(crate) fn send(
    request: RequestBuilder,
    limit: usize,
    truncate: bool,
    retry: bool,
) -> Result<Response> {
    operation::check()?;
    let context = operation::current();
    operation::phase("http");
    runtime().block_on(async {
        let work = async {
            let attempts = if retry { 3 } else { 1 };
            for attempt in 0..attempts {
                let req = request.try_clone().ok_or_else(||Error::Protocol("request body is not replayable".into()))?;
                let result = async {
                    let mut response = req.send().await.map_err(network)?;
                    let status = response.status(); let headers = response.headers().clone(); let url = response.url().clone();
                    let mut body = Vec::new();
                    if status.is_success() {
                        while let Some(chunk) = response.chunk().await.map_err(network)? {
                            let remaining = limit.saturating_sub(body.len());
                            if chunk.len() > remaining && !truncate { return Err(Error::Protocol("response exceeds size limit".into())); }
                            body.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
                            if truncate && body.len() == limit { break; }
                        }
                    }
                    Ok(Response { status, headers, url, body: Cursor::new(body) })
                }.await;
                let delay = match &result {
                    Ok(r) if matches!(r.status.as_u16(), 429|502|503|504) => {
                        let requested = retry_after(&r.headers).unwrap_or(0);
                        if requested > 10 { return result; }
                        Some(Duration::from_millis((250 << attempt).max(requested * 1000)))
                    }
                    Err(Error::Network(_) | Error::Timeout) => Some(Duration::from_millis(250 << attempt)),
                    _ => None,
                };
                if let Some(delay) = delay.filter(|_|attempt + 1 < attempts) { tokio::time::sleep(delay).await; }
                else { return result; }
            }
            unreachable!()
        };
        if let Some(context) = context {
            tokio::select! {
                biased;
                error = async { loop { if let Err(e) = context.check() { break e; } tokio::time::sleep(Duration::from_millis(10)).await; } } => Err(error),
                value = work => value,
            }
        } else { work.await }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::Write,
        net::TcpListener,
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        },
    };
    fn client() -> reqwest::Client {
        reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(3))
            .build()
            .unwrap()
    }
    #[test]
    fn cancellation_aborts_inflight_body_and_deadline_aborts_headers() {
        for cancel in [false, true] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            let context =
                operation::OperationContext::new(operation::OperationOptions { timeout_ms: 250 })
                    .unwrap();
            let controller = context.clone();
            let server = std::thread::spawn(move || {
                let (mut socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut buf = [0; 4096];
                let _ = socket.read(&mut buf);
                if cancel {
                    socket
                        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\nx")
                        .unwrap();
                    controller.cancel();
                }
                // Dropping the future must close the incomplete response, not leave a worker fetching it.
                assert_eq!(socket.read(&mut buf).unwrap_or(0), 0);
            });
            let result =
                context.run(|| send(client().get(format!("http://{addr}/")), 1024, false, true));
            assert!(if cancel {
                matches!(result, Err(Error::Cancelled))
            } else {
                matches!(result, Err(Error::Timeout))
            });
            server.join().unwrap();
        }
    }
    #[test]
    fn rate_limit_preserves_server_delay_without_fast_retry() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let mut buf = [0; 4096];
            let _ = socket.read(&mut buf);
            socket.write_all(b"HTTP/1.1 429 Busy\r\nRetry-After: 60\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
        });
        let r = send(client().get(format!("http://{addr}/")), 1024, false, true).unwrap();
        let info = status_error(&r).info();
        assert_eq!(info.code, "rate_limited");
        assert_eq!(info.http_status, Some(429));
        assert_eq!(info.retry_after_seconds, Some(60));
        server.join().unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            "retry-after",
            httpdate::fmt_http_date(SystemTime::now() + Duration::from_secs(60))
                .parse()
                .unwrap(),
        );
        assert!((59..=61).contains(&retry_after(&headers).unwrap()));
        let unknown = Error::MutationUncertain(Box::new(Error::Timeout)).info();
        assert!(!unknown.retryable);
        assert_eq!(unknown.cause_code, Some("timeout"));
    }

    #[test]
    fn reads_retry_but_writes_are_never_replayed() {
        for retry in [false, true] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            let count = Arc::new(AtomicUsize::new(0));
            let observed = count.clone();
            let server = std::thread::spawn(move || {
                for _ in 0..if retry { 2 } else { 1 } {
                    let (mut socket, _) = listener.accept().unwrap();
                    let mut buf = [0; 4096];
                    let _ = socket.read(&mut buf);
                    let n = observed.fetch_add(1, Ordering::SeqCst);
                    let status = if n == 0 { "503 Busy" } else { "200 OK" };
                    write!(
                        socket,
                        "HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    )
                    .unwrap();
                }
            });
            let response =
                send(client().post(format!("http://{addr}/")), 1024, false, retry).unwrap();
            assert_eq!(response.status.as_u16(), if retry { 200 } else { 503 });
            server.join().unwrap();
            assert_eq!(count.load(Ordering::SeqCst), if retry { 2 } else { 1 });
        }
    }
}
