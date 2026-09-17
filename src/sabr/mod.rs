//! Native, bounded audio transport for YouTube's Web SABR protocol.
//!
//! A session owns its download cursor; call `next_chunk` serially and feed the
//! unchanged initialization/media bytes to the host decoder. URLs, ustreamer
//! configurations, tokens and playback cookies are credentials: do not log them.
//! Protocol reference: LuanRT/googlevideo (MIT); see THIRD_PARTY_NOTICES.md.
mod wire;

use crate::{operation, Error, Result};
use base64::{
    engine::general_purpose::{STANDARD, URL_SAFE, URL_SAFE_NO_PAD},
    Engine,
};
use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    io::Read,
    time::{Duration, Instant},
};
use wire::{bad, bytes, float, uint, Message};

pub(super) const MAX_SEGMENT: usize = 16 * 1024 * 1024;
const MAX_RESPONSE: usize = 32 * 1024 * 1024;
const MAX_DURATION_MS: u64 = 7 * 24 * 60 * 60 * 1000;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SabrFormat {
    pub itag: u32,
    pub last_modified: u64,
    #[serde(default)]
    pub xtags: Option<String>,
    pub mime_type: String,
    pub duration_ms: u64,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SabrConfig {
    pub video_id: String,
    pub server_abr_streaming_url: String,
    pub ustreamer_config: String,
    pub client_version: String,
    pub format: SabrFormat,
    #[serde(default)]
    pub po_token: Option<String>,
}

/// One complete, length-checked media segment. `data` is never a UMP envelope.
/// Seeking starts a new decoder epoch and returns an initialization segment again.
pub struct SabrChunk {
    pub data: Vec<u8>,
    pub is_init: bool,
    pub sequence: u32,
    pub start_ms: u64,
    pub duration_ms: u64,
    pub finished: bool,
}
impl Clone for SabrChunk {
    fn clone(&self) -> Self {
        Self {
            data: self.data.clone(),
            is_init: self.is_init,
            sequence: self.sequence,
            start_ms: self.start_ms,
            duration_ms: self.duration_ms,
            finished: self.finished,
        }
    }
}

#[derive(Clone)]
struct Pending {
    chunk: SabrChunk,
    expected: Option<usize>,
    selected: bool,
}
#[derive(Clone)]
struct Context {
    value: Vec<u8>,
    active: bool,
    scope: u64,
}
#[derive(Clone, Default)]
struct State {
    initialized: bool,
    init_received: bool,
    end_sequence: Option<u32>,
    position_ms: u64,
    start_ms: Option<u64>,
    first_sequence: Option<u32>,
    last_sequence: Option<u32>,
    pending: BTreeMap<u8, Pending>,
    queue: VecDeque<SabrChunk>,
    seen: BTreeSet<(bool, u32)>,
    contexts: BTreeMap<u64, Context>,
    active_contexts: BTreeSet<u64>,
    playback_cookie: Vec<u8>,
    backoff_ms: u64,
    complete: bool,
}

/// Stateful audio-only SABR transport. Uses the same cancellable HTTP client as
/// the core, with no Cookies or account authorization headers sent to the CDN.
pub struct SabrSession {
    http: Client,
    config: SabrConfig,
    url: Url,
    ustreamer: Vec<u8>,
    po_token: Option<Vec<u8>>,
    state: State,
    request_number: u32,
    retry_at: Option<Instant>,
    pending_reload: Option<String>,
}

pub(crate) fn validate_url(raw: &str) -> Result<Url> {
    if raw.len() > 65536 {
        return Err(bad());
    }
    let u = Url::parse(raw).map_err(|_| bad())?;
    if u.scheme() != "https"
        || !u
            .host_str()
            .is_some_and(|h| h.ends_with(".googlevideo.com") || h == "googlevideo.com")
        || u.port_or_known_default() != Some(443)
        || !u.username().is_empty()
        || u.password().is_some()
        || u.path() != "/videoplayback"
        || u.fragment().is_some()
    {
        return Err(Error::InvalidInput(
            "SABR URL must use the official HTTPS Google video CDN".into(),
        ));
    }
    Ok(u)
}
fn decode(v: &str, limit: usize) -> Result<Vec<u8>> {
    if v.is_empty() || v.len() > limit * 2 {
        return Err(bad());
    }
    let data = URL_SAFE_NO_PAD
        .decode(v)
        .or_else(|_| URL_SAFE.decode(v))
        .or_else(|_| STANDARD.decode(v))
        .map_err(|_| bad())?;
    if data.len() > limit {
        return Err(bad());
    }
    Ok(data)
}
fn format_id(f: &SabrFormat) -> Vec<u8> {
    let mut out = Vec::new();
    uint(1, u64::from(f.itag), &mut out);
    uint(2, f.last_modified, &mut out);
    if let Some(s) = &f.xtags {
        bytes(3, s.as_bytes(), &mut out);
    }
    out
}
fn same_format(m: &Message<'_>, f: &SabrFormat) -> Result<bool> {
    Ok(m.uint(1)? == Some(u64::from(f.itag))
        && m.uint(2)?.is_none_or(|v| v == f.last_modified)
        && m.string(3)?.unwrap_or("") == f.xtags.as_deref().unwrap_or(""))
}
fn number(v: Option<u64>) -> Result<u32> {
    u32::try_from(v.ok_or_else(bad)?).map_err(|_| bad())
}
fn time(m: &Message<'_>, field: u32, ticks: u32) -> Result<u64> {
    if let Some(v) = m.uint(field)? {
        if v > MAX_DURATION_MS {
            return Err(bad());
        }
        return Ok(v);
    }
    if let Some(t) = m.bytes(15)? {
        let t = Message::parse(t)?;
        let scale = t.uint(3)?.filter(|v| *v > 0).ok_or_else(bad)?;
        let v = t
            .uint(ticks)?
            .unwrap_or(0)
            .checked_mul(1000)
            .ok_or_else(bad)?
            .div_ceil(scale);
        if v > MAX_DURATION_MS {
            return Err(bad());
        }
        return Ok(v);
    }
    Ok(0)
}

impl SabrSession {
    pub(crate) fn new(http: Client, mut config: SabrConfig) -> Result<Self> {
        crate::client::validate_video_id(&config.video_id)?;
        if config.format.itag == 0
            || config.format.last_modified == 0
            || config.format.duration_ms == 0
            || config.format.duration_ms > MAX_DURATION_MS
            || !matches!(
                config.format.mime_type.split(';').next().map(str::trim),
                Some("audio/mp4" | "audio/webm")
            )
            || config
                .format
                .xtags
                .as_ref()
                .is_some_and(|v| v.len() > 4096 || v.contains('\0'))
            || config.client_version.is_empty()
            || config.client_version.len() > 128
            || !config
                .client_version
                .bytes()
                .all(|v| v.is_ascii_alphanumeric() || v == b'.' || v == b'_')
        {
            return Err(Error::InvalidInput(
                "invalid Web SABR audio configuration".into(),
            ));
        }
        let url = validate_url(&config.server_abr_streaming_url)?;
        let ustreamer = decode(&config.ustreamer_config, 65536)?;
        let po_token = config
            .po_token
            .take()
            .map(|mut value| {
                use zeroize::Zeroize;
                let decoded = decode(&value, 16384);
                value.zeroize();
                decoded
            })
            .transpose()?;
        Ok(Self {
            http,
            config,
            url,
            ustreamer,
            po_token,
            state: State::default(),
            request_number: 0,
            retry_at: None,
            pending_reload: None,
        })
    }
    /// The opaque token must be sent only to the official player reload endpoint.
    pub(crate) fn take_reload_token(&mut self) -> Option<String> {
        self.pending_reload.take()
    }
    pub(crate) fn apply_reload(&mut self, config: SabrConfig) -> Result<()> {
        if config.video_id != self.config.video_id
            || config.format.itag != self.config.format.itag
            || config.format.last_modified != self.config.format.last_modified
            || config.format.xtags != self.config.format.xtags
            || config.format.mime_type != self.config.format.mime_type
        {
            return Err(Error::StreamUnavailable(
                "SABR format changed; open a new audio session".into(),
            ));
        }
        let replacement = Self::new(self.http.clone(), config)?;
        self.config = replacement.config;
        self.url = replacement.url;
        self.ustreamer = replacement.ustreamer;
        self.clear_po_token();
        self.po_token = replacement.po_token;
        self.retry_at = None;
        self.pending_reload = None;
        Ok(())
    }
    pub fn video_id(&self) -> &str {
        &self.config.video_id
    }
    pub fn format(&self) -> &SabrFormat {
        &self.config.format
    }
    /// Replace an expired proof obtained by the host for this same Web session.
    pub fn set_po_token(&mut self, token: &str) -> Result<()> {
        let decoded = decode(token, 16384)?;
        self.clear_po_token();
        self.po_token = Some(decoded);
        Ok(())
    }
    pub fn clear_po_token(&mut self) {
        use zeroize::Zeroize;
        if let Some(mut token) = self.po_token.take() {
            token.zeroize();
        }
    }
    /// Reset buffered segments. The host must reset its decoder and discard old chunks.
    pub fn seek(&mut self, position_ms: u64) -> Result<()> {
        operation::check()?;
        if position_ms > self.config.format.duration_ms {
            return Err(Error::InvalidInput("seek exceeds audio duration".into()));
        }
        let contexts = self.state.contexts.clone();
        let active_contexts = self.state.active_contexts.clone();
        let cookie = self.state.playback_cookie.clone();
        self.state = State {
            position_ms,
            contexts,
            active_contexts,
            playback_cookie: cookie,
            complete: position_ms == self.config.format.duration_ms,
            ..State::default()
        };
        Ok(())
    }
    /// Confirm a complete initialization segment before publishing the session.
    /// The first host read still receives those exact initialization bytes.
    pub(crate) fn prepare(&mut self) -> Result<()> {
        operation::ensure(|| {
            let chunk = self.next_inner()?;
            if !chunk.is_init || chunk.data.is_empty() {
                return Err(Error::StreamUnavailable(
                    "SABR did not provide an audio initialization segment".into(),
                ));
            }
            self.state.queue.push_front(chunk);
            Ok(())
        })
    }
    pub fn next_chunk(&mut self) -> Result<SabrChunk> {
        operation::ensure(|| self.next_inner())
    }
    fn next_inner(&mut self) -> Result<SabrChunk> {
        operation::check()?;
        for _ in 0..8 {
            if let Some(chunk) = self.state.queue.pop_front() {
                return Ok(chunk);
            }
            if self.state.complete {
                return Ok(SabrChunk {
                    data: Vec::new(),
                    is_init: false,
                    sequence: self.state.last_sequence.unwrap_or(0),
                    start_ms: self.state.position_ms,
                    duration_ms: 0,
                    finished: true,
                });
            }
            self.fetch()?;
        }
        Err(Error::StreamUnavailable(
            "SABR made no media progress after eight responses".into(),
        ))
    }
    fn request_body(&self) -> Vec<u8> {
        let mut state = Vec::new();
        uint(21, 360, &mut state);
        uint(22, 0, &mut state);
        uint(28, self.state.position_ms, &mut state);
        uint(34, 1, &mut state);
        float(35, 1., &mut state);
        uint(40, 1, &mut state);
        uint(46, 0, &mut state);
        let mut client = Vec::new();
        uint(16, 67, &mut client);
        bytes(17, self.config.client_version.as_bytes(), &mut client);
        let mut context = Vec::new();
        bytes(1, &client, &mut context);
        if let Some(po) = &self.po_token {
            bytes(2, po, &mut context);
        }
        if !self.state.playback_cookie.is_empty() {
            bytes(3, &self.state.playback_cookie, &mut context);
        }
        for (kind, ctx) in &self.state.contexts {
            if ctx.active {
                let mut c = Vec::new();
                uint(1, *kind, &mut c);
                bytes(2, &ctx.value, &mut c);
                bytes(5, &c, &mut context);
            } else {
                uint(6, *kind, &mut context);
            }
        }
        let id = format_id(&self.config.format);
        let mut out = Vec::new();
        bytes(1, &state, &mut out);
        if self.state.initialized {
            bytes(2, &id, &mut out);
        }
        if let (Some(start), Some(first), Some(last)) = (
            self.state.start_ms,
            self.state.first_sequence,
            self.state.last_sequence,
        ) {
            let mut range = Vec::new();
            bytes(1, &id, &mut range);
            uint(2, start, &mut range);
            uint(3, self.state.position_ms.saturating_sub(start), &mut range);
            uint(4, u64::from(first), &mut range);
            uint(5, u64::from(last), &mut range);
            bytes(3, &range, &mut out);
        }
        bytes(5, &self.ustreamer, &mut out);
        bytes(16, &id, &mut out);
        bytes(19, &context, &mut out);
        out
    }
    fn fetch(&mut self) -> Result<()> {
        if let Some(deadline) = self.retry_at {
            while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
                operation::check()?;
                std::thread::sleep(remaining.min(Duration::from_millis(10)));
            }
        }
        self.retry_at = None;
        operation::phase("receiving_sabr");
        let body = self.request_body();
        let mut url = self.url.clone();
        let params: Vec<_> = url
            .query_pairs()
            .filter(|(k, _)| k != "rn")
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        url.set_query(None);
        url.query_pairs_mut()
            .extend_pairs(params)
            .append_pair("rn", &self.request_number.to_string());
        self.request_number = self.request_number.checked_add(1).ok_or_else(bad)?;
        let req = self
            .http
            .post(url)
            .header("Content-Type", "application/x-protobuf")
            .header("Accept", "application/vnd.yt-ump")
            .header("Accept-Encoding", "identity")
            .header("Origin", "https://music.youtube.com")
            .header("Referer", "https://music.youtube.com/")
            .body(body);
        // Stateful SABR POST is not automatically replayed; callers may retry after an error.
        let mut response = crate::transport::send(req, MAX_RESPONSE, false, false)?;
        if !response.status().is_success() {
            return Err(crate::transport::status_error(&response));
        }
        let mime = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .split(';')
            .next()
            .unwrap_or("")
            .trim();
        if mime != "application/vnd.yt-ump" {
            return Err(Error::MediaValidation(
                "SABR returned an unexpected content type".into(),
            ));
        }
        let mut body = Vec::new();
        response.read_to_end(&mut body).map_err(|_| bad())?;
        self.process(&body)
    }
    fn process(&mut self, body: &[u8]) -> Result<()> {
        // Parse transactionally: truncated media, cancellation or a server error must
        // not publish half a response or advance the acknowledged buffer cursor.
        let mut next = self.state.clone();
        next.backoff_ms = 0;
        next.contexts.retain(|_, c| c.scope != 2);
        let mut url = self.url.clone();
        let mut protection_pending = false;
        let parts = wire::parts(body)?;
        if parts.is_empty() {
            return Err(bad());
        }
        for (kind, data) in parts {
            operation::check()?;
            match kind {
                20 => Self::header(&self.config, &mut next, data)?,
                21 => {
                    let (&id, data) = data.split_first().ok_or_else(bad)?;
                    let p = next.pending.get_mut(&id).ok_or_else(bad)?;
                    if p.chunk.data.len().saturating_add(data.len()) > MAX_SEGMENT {
                        return Err(bad());
                    }
                    p.chunk.data.extend_from_slice(data);
                }
                22 => Self::finish_segment(&self.config, &mut next, data)?,
                35 => {
                    let m = Message::parse(data)?;
                    next.backoff_ms = m.uint(4)?.unwrap_or(0);
                    if next.backoff_ms > 600_000 {
                        return Err(Error::StreamUnavailable(
                            "SABR requested an excessive backoff".into(),
                        ));
                    }
                    if let Some(c) = m.bytes(7)? {
                        if c.len() > 65536 {
                            return Err(bad());
                        }
                        next.playback_cookie = c.to_vec();
                    }
                }
                42 => {
                    let m = Message::parse(data)?;
                    if let Some(id) = m.bytes(2)? {
                        if same_format(&Message::parse(id)?, &self.config.format)? {
                            if m.string(1)?.is_some_and(|v| v != self.config.video_id) {
                                return Err(bad());
                            }
                            if let Some(mime) = m.string(5)? {
                                if mime.split(';').next().map(str::trim)
                                    != self
                                        .config
                                        .format
                                        .mime_type
                                        .split(';')
                                        .next()
                                        .map(str::trim)
                                {
                                    return Err(bad());
                                }
                            }
                            next.initialized = true;
                            next.end_sequence = m
                                .uint(4)?
                                .map(|v| u32::try_from(v).map_err(|_| bad()))
                                .transpose()?;
                        }
                    }
                }
                43 => {
                    let m = Message::parse(data)?;
                    url = validate_url(m.string(1)?.ok_or_else(bad)?)?;
                }
                44 => {
                    return Err(Error::StreamUnavailable(
                        "SABR server rejected the playback request".into(),
                    ))
                }
                45 => {
                    return Err(Error::StreamUnavailable(
                        "SABR requested a server-directed seek".into(),
                    ))
                }
                46 => {
                    let context = Message::parse(data)?;
                    let params = Message::parse(context.bytes(1)?.ok_or_else(bad)?)?;
                    let token = params
                        .string(1)?
                        .filter(|v| !v.is_empty() && v.len() <= 16384)
                        .ok_or_else(bad)?;
                    self.pending_reload = Some(token.into());
                    return Err(Error::SabrReloadRequired);
                }
                57 => {
                    let m = Message::parse(data)?;
                    let t = m.uint(1)?.ok_or_else(bad)?;
                    if t > i32::MAX as u64 {
                        return Err(bad());
                    }
                    let value = m.bytes(3)?.ok_or_else(bad)?;
                    if value.len() > 65536
                        || (!next.contexts.contains_key(&t) && next.contexts.len() >= 64)
                    {
                        return Err(bad());
                    }
                    if m.uint(5)? != Some(2) || !next.contexts.contains_key(&t) {
                        let active = m.uint(4)? == Some(1) || next.active_contexts.contains(&t);
                        if active {
                            next.active_contexts.insert(t);
                        }
                        next.contexts.insert(
                            t,
                            Context {
                                value: value.to_vec(),
                                active,
                                scope: m.uint(2)?.unwrap_or(0),
                            },
                        );
                    }
                }
                58 => {
                    let m = Message::parse(data)?;
                    let status = m.uint(1)?.unwrap_or(0);
                    protection_pending |= status == 2;
                    if status >= 3 {
                        return Err(Error::PoTokenRequired);
                    }
                }
                59 => {
                    let m = Message::parse(data)?;
                    for t in m.repeated_uint(1)? {
                        next.active_contexts.insert(t);
                        if let Some(c) = next.contexts.get_mut(&t) {
                            c.active = true;
                        }
                    }
                    for t in m.repeated_uint(2)? {
                        next.active_contexts.remove(&t);
                        if let Some(c) = next.contexts.get_mut(&t) {
                            c.active = false;
                        }
                    }
                    for t in m.repeated_uint(3)? {
                        next.contexts.remove(&t);
                    }
                }
                // End-of-track is only advisory; require contiguous complete media
                // and the selected format's initialization metadata before EOF.
                62 => {}
                _ => {}
            }
            let buffered: usize = next
                .pending
                .values()
                .map(|p| p.chunk.data.len())
                .sum::<usize>()
                + next.queue.iter().map(|c| c.data.len()).sum::<usize>();
            if buffered > MAX_RESPONSE
                || next.pending.len() > 64
                || next.queue.len() > 4096
                || next.seen.len() > 65536
                || next.active_contexts.len() > 256
            {
                return Err(bad());
            }
        }
        if protection_pending && next.queue.is_empty() && next.pending.is_empty() {
            return Err(Error::PoTokenRequired);
        }
        if next.init_received
            && (next
                .last_sequence
                .is_some_and(|n| next.end_sequence == Some(n))
                || (next.end_sequence.is_none()
                    && next.position_ms >= self.config.format.duration_ms))
        {
            next.complete = true;
        }
        if let Some(last) = next.queue.back_mut() {
            last.finished = next.complete;
        }
        operation::check()?;
        self.retry_at = Some(Instant::now() + Duration::from_millis(next.backoff_ms));
        self.state = next;
        self.url = url;
        Ok(())
    }
    fn header(config: &SabrConfig, state: &mut State, data: &[u8]) -> Result<()> {
        let m = Message::parse(data)?;
        let id = u8::try_from(m.uint(1)?.ok_or_else(bad)?).map_err(|_| bad())?;
        if state.pending.contains_key(&id) {
            return Err(bad());
        }
        let selected = if let Some(f) = m.bytes(13)? {
            same_format(&Message::parse(f)?, &config.format)?
        } else {
            m.uint(3)? == Some(u64::from(config.format.itag))
                && m.uint(4)?.is_none_or(|v| v == config.format.last_modified)
                && m.string(5)?.unwrap_or("") == config.format.xtags.as_deref().unwrap_or("")
        };
        if selected && m.string(2)?.is_some_and(|v| v != config.video_id) {
            return Err(bad());
        }
        if m.uint(7)?.unwrap_or(0) != 0 {
            return Err(Error::MediaValidation(
                "compressed SABR media segments are unsupported".into(),
            ));
        }
        let is_init = m.uint(8)?.unwrap_or(0) != 0;
        let sequence = if is_init { 0 } else { number(m.uint(9)?)? };
        let expected = m
            .uint(14)?
            .map(|v| usize::try_from(v).map_err(|_| bad()))
            .transpose()?;
        if expected.is_some_and(|v| v > MAX_SEGMENT) {
            return Err(bad());
        }
        state.pending.insert(
            id,
            Pending {
                chunk: SabrChunk {
                    data: Vec::new(),
                    is_init,
                    sequence,
                    start_ms: time(&m, 11, 1)?,
                    duration_ms: time(&m, 12, 2)?,
                    finished: false,
                },
                expected,
                selected,
            },
        );
        Ok(())
    }
    fn finish_segment(config: &SabrConfig, state: &mut State, data: &[u8]) -> Result<()> {
        if data.len() != 1 {
            return Err(bad());
        }
        let pending = state.pending.remove(&data[0]).ok_or_else(bad)?;
        let chunk = pending.chunk;
        if pending.expected.is_some_and(|v| v != chunk.data.len()) || chunk.data.is_empty() {
            return Err(bad());
        }
        if !pending.selected || state.seen.contains(&(chunk.is_init, chunk.sequence)) {
            return Ok(());
        }
        if !state.initialized {
            return Err(bad());
        }
        if chunk.is_init {
            let valid = if config.format.mime_type.starts_with("audio/mp4") {
                chunk.data.get(4..8) == Some(b"ftyp")
            } else {
                chunk.data.starts_with(&[0x1a, 0x45, 0xdf, 0xa3])
            };
            if !valid {
                return Err(Error::MediaValidation(
                    "SABR initialization is not the selected audio container".into(),
                ));
            }
            state.init_received = true;
        } else {
            if !state.init_received
                || chunk.duration_ms == 0
                || state
                    .last_sequence
                    .is_some_and(|last| last.checked_add(1) != Some(chunk.sequence))
            {
                return Err(Error::MediaValidation(
                    "SABR returned discontinuous audio segments".into(),
                ));
            }
            if let Some(last) = state.last_sequence {
                if last >= chunk.sequence {
                    return Err(bad());
                }
            }
            let end = chunk
                .start_ms
                .checked_add(chunk.duration_ms)
                .ok_or_else(bad)?;
            if end > MAX_DURATION_MS {
                return Err(bad());
            }
            state.start_ms.get_or_insert(chunk.start_ms);
            state.first_sequence.get_or_insert(chunk.sequence);
            state.last_sequence = Some(chunk.sequence);
            state.position_ms = end;
        }
        state.seen.insert((chunk.is_init, chunk.sequence));
        state.queue.push_back(chunk);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn config() -> SabrConfig {
        SabrConfig {
            video_id: "4D7u5KF7SP8".into(),
            server_abr_streaming_url: "https://rr1.googlevideo.com/videoplayback?expire=9999999999"
                .into(),
            ustreamer_config: "AQ".into(),
            client_version: "1.20260916.01.00".into(),
            format: SabrFormat {
                itag: 140,
                last_modified: 1,
                xtags: None,
                mime_type: "audio/mp4; codecs=\"mp4a.40.2\"".into(),
                duration_ms: 2000,
            },
            po_token: None,
        }
    }
    fn session() -> SabrSession {
        SabrSession::new(
            Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap(),
            config(),
        )
        .unwrap()
    }
    fn ump_uint(v: u32, out: &mut Vec<u8>) {
        if v < 128 {
            out.push(v as u8);
        } else if v < 16384 {
            out.push(128 | (v & 63) as u8);
            out.push((v >> 6) as u8);
        } else {
            out.push(240);
            out.extend(v.to_le_bytes());
        }
    }
    fn part(kind: u32, data: &[u8], out: &mut Vec<u8>) {
        ump_uint(kind, out);
        ump_uint(data.len() as u32, out);
        out.extend_from_slice(data);
    }
    fn initialization(out: &mut Vec<u8>) {
        let mut m = Vec::new();
        bytes(1, config().video_id.as_bytes(), &mut m);
        bytes(2, &format_id(&config().format), &mut m);
        uint(4, 2, &mut m);
        bytes(5, b"audio/mp4", &mut m);
        part(42, &m, out);
    }
    fn segment(init: bool, n: u32, content: &[u8], expected: usize, out: &mut Vec<u8>) {
        let mut h = Vec::new();
        uint(1, 0, &mut h);
        uint(3, 140, &mut h);
        uint(4, 1, &mut h);
        uint(8, u64::from(init), &mut h);
        uint(9, u64::from(n), &mut h);
        uint(11, u64::from(n.saturating_sub(1)) * 1000, &mut h);
        uint(12, if init { 0 } else { 1000 }, &mut h);
        uint(14, expected as u64, &mut h);
        part(20, &h, out);
        let mut c = vec![0];
        c.extend(content);
        part(21, &c, out);
        part(22, &[0], out);
    }
    #[test]
    fn complete_audio_cursor_roundtrips_request_and_seek() {
        let mut s = session();
        let mut response = Vec::new();
        initialization(&mut response);
        segment(true, 0, b"\0\0\0\x10ftypisomxxxx", 16, &mut response);
        segment(false, 1, b"media1", 6, &mut response);
        s.process(&response).unwrap();
        assert_eq!(s.state.position_ms, 1000);
        assert!(!s.state.complete);
        s.prepare().unwrap();
        assert!(s.next_chunk().unwrap().is_init);
        assert_eq!(s.next_chunk().unwrap().data, b"media1");
        let req = s.request_body();
        let m = Message::parse(&req).unwrap();
        assert!(m.bytes(2).unwrap().is_some());
        let range = Message::parse(m.bytes(3).unwrap().unwrap()).unwrap();
        assert_eq!(range.uint(3).unwrap(), Some(1000));
        let mut response = Vec::new();
        segment(false, 2, b"media2", 6, &mut response);
        s.process(&response).unwrap();
        let final_chunk = s.next_chunk().unwrap();
        assert!(final_chunk.finished);
        assert_eq!(final_chunk.data, b"media2");
        assert!(s.next_chunk().unwrap().data.is_empty());
        s.seek(500).unwrap();
        assert!(!s.state.initialized);
        assert_eq!(s.state.position_ms, 500);
        assert!(s.state.queue.is_empty());
        assert!(s.seek(2001).is_err());
        s.seek(2000).unwrap();
        assert!(s.next_chunk().unwrap().finished);
    }
    #[test]
    fn malformed_media_never_acknowledges_or_publishes_partial_response() {
        let mut s = session();
        let mut body = Vec::new();
        initialization(&mut body);
        segment(true, 0, b"\0\0\0\x10ftypisomxxxx", 16, &mut body);
        segment(false, 1, b"short", 10, &mut body);
        assert!(s.process(&body).is_err());
        assert!(!s.state.initialized);
        assert!(s.state.queue.is_empty());
        let mut body = Vec::new();
        initialization(&mut body);
        segment(true, 0, b"\0\0\0\x10ftypisomxxxx", 16, &mut body);
        segment(false, 1, b"a", 1, &mut body);
        segment(false, 3, b"b", 1, &mut body);
        assert!(s.process(&body).is_err());
        assert!(s.state.queue.is_empty());
    }
    #[test]
    fn redirect_context_policy_cookie_and_attestation_are_validated() {
        let mut s = session();
        let mut body = Vec::new();
        let mut ctx = Vec::new();
        uint(1, 7, &mut ctx);
        uint(2, 1, &mut ctx);
        bytes(3, b"opaque", &mut ctx);
        uint(4, 1, &mut ctx);
        part(57, &ctx, &mut body);
        let mut policy = Vec::new();
        uint(4, 50, &mut policy);
        bytes(7, b"cookie", &mut policy);
        part(35, &policy, &mut body);
        s.process(&body).unwrap();
        let request = s.request_body();
        let request = Message::parse(&request).unwrap();
        let c = Message::parse(request.bytes(19).unwrap().unwrap()).unwrap();
        assert_eq!(c.bytes(3).unwrap(), Some(b"cookie".as_slice()));
        assert!(c.bytes(5).unwrap().is_some());
        let mut body = Vec::new();
        let mut policy = Vec::new();
        uint(2, 7, &mut policy);
        part(59, &policy, &mut body);
        s.process(&body).unwrap();
        assert!(!s.state.contexts[&7].active);
        let mut body = Vec::new();
        let mut redirect = Vec::new();
        bytes(1, b"https://evil.invalid/videoplayback", &mut redirect);
        part(43, &redirect, &mut body);
        assert!(s.process(&body).is_err());
        let mut body = Vec::new();
        let mut status = Vec::new();
        uint(1, 3, &mut status);
        part(58, &status, &mut body);
        let err = s.process(&body).err().unwrap();
        assert!(matches!(err, Error::PoTokenRequired));
        for bad in [
            "http://rr1.googlevideo.com/videoplayback",
            "https://googlevideo.com.evil.invalid/videoplayback",
            "https://user:password@rr1.googlevideo.com/videoplayback",
            "https://rr1.googlevideo.com:8443/videoplayback",
            "https://rr1.googlevideo.com/other",
        ] {
            assert!(validate_url(bad).is_err());
        }
    }
    #[test]
    fn player_reload_token_stays_private_and_preserves_cursor() {
        let mut session = session();
        session.state.position_ms = 1000;
        let mut params = Vec::new();
        bytes(1, b"opaque-reload-token", &mut params);
        let mut context = Vec::new();
        bytes(1, &params, &mut context);
        let mut body = Vec::new();
        part(46, &context, &mut body);
        let error = session.process(&body).err().unwrap();
        assert!(matches!(error, Error::SabrReloadRequired));
        assert!(!error.to_string().contains("opaque-reload-token"));
        assert_eq!(
            session.take_reload_token().as_deref(),
            Some("opaque-reload-token")
        );
        assert!(session.take_reload_token().is_none());
        let mut changed = config();
        changed.format.last_modified = 2;
        assert!(session.apply_reload(changed).is_err());
        session.apply_reload(config()).unwrap();
        assert_eq!(session.state.position_ms, 1000);
    }
    #[test]
    fn inflight_sabr_cancellation_closes_response_and_keeps_cursor() {
        use std::{
            io::{Read, Write},
            net::TcpListener,
        };
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let context =
            operation::OperationContext::new(operation::OperationOptions { timeout_ms: 5000 })
                .unwrap();
        let control = context.clone();
        let worker = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut received = Vec::new();
            loop {
                let mut chunk = [0; 4096];
                let n = socket.read(&mut chunk).unwrap();
                assert!(n > 0);
                received.extend_from_slice(&chunk[..n]);
                if let Some(end) = received.windows(4).position(|w| w == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&received[..end]).to_ascii_lowercase();
                    let len: usize = headers
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length:"))
                        .unwrap()
                        .trim()
                        .parse()
                        .unwrap();
                    if received.len() >= end + 4 + len {
                        break;
                    }
                }
            }
            socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/vnd.yt-ump\r\nContent-Length: 10000\r\n\r\nx").unwrap();
            control.cancel();
            let mut b = [0; 1];
            assert_eq!(socket.read(&mut b).unwrap_or(0), 0);
        });
        let mut session = session();
        session.url = Url::parse(&format!("http://{address}/videoplayback")).unwrap();
        let result = context.run(|| session.next_chunk());
        assert!(matches!(result, Err(Error::Cancelled)));
        assert_eq!(session.state.position_ms, 0);
        assert!(session.state.queue.is_empty());
        worker.join().unwrap();
    }
    #[test]
    fn cancellation_stops_parsing_before_cursor_changes() {
        let mut s = session();
        let c = operation::OperationContext::new(operation::OperationOptions::default()).unwrap();
        c.cancel();
        assert!(matches!(
            c.run(|| s.process(&[62, 0])),
            Err(Error::Cancelled)
        ));
        assert_eq!(s.state.position_ms, 0);
    }
    #[test]
    #[ignore = "requires a private current WEB_REMIX SABR configuration"]
    fn live_audio_transport() {
        let path = std::env::var("YTMUSIC_SABR_CONFIG").expect("private config path");
        let config: SabrConfig = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        let http = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(60))
            .build()
            .unwrap();
        let mut s = SabrSession::new(http, config).unwrap();
        let mut segments = 0;
        let mut bytes = 0;
        let mut duration = 0;
        let output = std::env::var("YTMUSIC_SABR_OUTPUT").ok();
        let mut output = output.map(|p| std::fs::File::create(p).unwrap());
        loop {
            let chunk = s.next_chunk().unwrap();
            if !chunk.is_init {
                duration += chunk.duration_ms;
            }
            bytes += chunk.data.len();
            segments += 1;
            if let Some(file) = &mut output {
                use std::io::Write;
                file.write_all(&chunk.data).unwrap();
            }
            if chunk.finished {
                break;
            }
            assert!(segments < 10000);
        }
        eprintln!("SABR audio verified: {segments} segments, {bytes} bytes, {duration} ms");
        assert!(bytes > 4096);
        assert!(segments >= 2);
        let target = s.format().duration_ms / 2;
        s.seek(target).unwrap();
        let init = s.next_chunk().unwrap();
        assert!(init.is_init);
        let media = s.next_chunk().unwrap();
        assert!(!media.is_init);
        assert!(media.start_ms.abs_diff(target) <= media.duration_ms + 1000);
        let next = s.next_chunk().unwrap();
        assert_eq!(next.sequence, media.sequence + 1);
        eprintln!(
            "SABR seek verified: target {target} ms, first media {} ms",
            media.start_ms
        );
    }
}
