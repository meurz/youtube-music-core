//! Official Web discovery, queue context, lyrics availability and account selection.
use crate::{
    auth::AccountInfo,
    client::{nonempty, validate_video_id},
    model::{Page, Thumbnail},
    parse, Error, MusicClient, Result,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchSuggestion {
    pub query: String,
    pub from_history: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveryFilter {
    pub title: String,
    pub browse_id: String,
    pub params: Option<String>,
    pub selected: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveryPage {
    #[serde(flatten)]
    pub page: Page,
    /// Feed continuation, distinct from an individual carousel's continuation.
    pub continuation: Option<String>,
    pub filters: Vec<DiscoveryFilter>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct QueueContext {
    pub video_id: Option<String>,
    pub playlist_id: Option<String>,
    pub params: Option<String>,
    pub index: Option<u32>,
    pub continuation: Option<String>,
    pub queue_context_params: Option<String>,
}
impl QueueContext {
    pub fn validate(&self) -> Result<()> {
        if let Some(id) = &self.video_id {
            validate_video_id(id)?;
        }
        if self.video_id.is_none() && self.playlist_id.is_none() && self.continuation.is_none() {
            return Err(Error::InvalidInput(
                "queue requires video_id, playlist_id or continuation".into(),
            ));
        }
        for (name, value) in [
            ("playlist_id", &self.playlist_id),
            ("params", &self.params),
            ("continuation", &self.continuation),
            ("queue_context_params", &self.queue_context_params),
        ] {
            if let Some(value) = value {
                nonempty(value, name)?;
            }
        }
        Ok(())
    }
    fn body(&self) -> Result<Value> {
        self.validate()?;
        let mut body = json!({"isAudioOnly":true,"enablePersistentPlaylistPanel":true});
        for (key, value) in [
            ("videoId", &self.video_id),
            ("playlistId", &self.playlist_id),
            ("params", &self.params),
            ("continuation", &self.continuation),
            ("queueContextParams", &self.queue_context_params),
        ] {
            if let Some(value) = value {
                body[key] = value.clone().into();
            }
        }
        if let Some(index) = self.index {
            body["index"] = index.into();
        }
        Ok(body)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueuePage {
    #[serde(flatten)]
    pub page: Page,
    pub context: QueueContext,
    pub continuation: Option<String>,
    pub lyrics_browse_id: Option<String>,
    pub related_browse_id: Option<String>,
    pub automix: Option<QueueContext>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LyricLine {
    pub text: String,
    pub start_ms: u64,
    pub end_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimedLyrics {
    pub browse_id: String,
    pub text: String,
    pub source: Option<String>,
    pub timed: bool,
    /// `available` or `not_provided_by_web`; no synthetic timestamps.
    pub timing_availability: String,
    pub source_client: String,
    pub lines: Vec<LyricLine>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AccountSelector {
    pub auth_user: u32,
    pub delegated_session_id: Option<String>,
    /// When available, require the remote active channel to match this handle.
    pub expected_channel_handle: Option<String>,
}
impl AccountSelector {
    pub fn validate(&self) -> Result<()> {
        if self.auth_user > 99 {
            return Err(Error::InvalidInput(
                "auth_user must be between 0 and 99".into(),
            ));
        }
        if let Some(id) = &self.delegated_session_id {
            if id.is_empty()
                || id.len() > 256
                || !id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
            {
                return Err(Error::InvalidInput("invalid delegated session ID".into()));
            }
        }
        if let Some(handle) = &self.expected_channel_handle {
            nonempty(handle, "expected_channel_handle")?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountChoice {
    pub name: String,
    pub channel_handle: Option<String>,
    pub thumbnails: Vec<Thumbnail>,
    pub selected: bool,
    pub disabled: bool,
    /// None when Web lists an identity without a reusable selector. Reimport that
    /// identity from the browser; obfuscated GAIA IDs are not session IDs.
    pub selector: Option<AccountSelector>,
}

fn collect<'a>(value: &'a Value, key: &str, output: &mut Vec<&'a Value>) {
    match value {
        Value::Object(map) => {
            if let Some(value) = map.get(key) {
                output.push(value);
            }
            for value in map.values() {
                collect(value, key, output);
            }
        }
        Value::Array(items) => {
            for value in items {
                collect(value, key, output);
            }
        }
        _ => {}
    }
}
fn optional_text(value: &Value) -> Option<String> {
    let s = parse::text(value);
    (!s.is_empty()).then_some(s)
}
fn string(value: &Value) -> Option<String> {
    value.as_str().map(str::to_owned)
}
fn number(value: &Value) -> Option<u64> {
    value.as_u64().or_else(|| value.as_str()?.parse().ok())
}
fn direct_continuation(renderer: &Value) -> Option<String> {
    renderer["continuations"]
        .as_array()
        .and_then(|values| {
            values.iter().find_map(|v| {
                string(&v["nextContinuationData"]["continuation"])
                    .or_else(|| string(&v["nextRadioContinuationData"]["continuation"]))
            })
        })
        .or_else(|| {
            renderer["contents"].as_array()?.iter().find_map(|v| {
                string(
                    &v["continuationItemRenderer"]["continuationEndpoint"]["continuationCommand"]
                        ["token"],
                )
            })
        })
}
fn parse_discovery(value: &Value) -> Result<DiscoveryPage> {
    let page = parse::page(value)?;
    let list = parse::find(value, "sectionListRenderer")
        .or_else(|| parse::find(value, "sectionListContinuation"));
    let continuation = list.and_then(direct_continuation);
    let mut chips = Vec::new();
    collect(value, "chipCloudChipRenderer", &mut chips);
    let filters = chips
        .into_iter()
        .filter_map(|chip| {
            let ep = &chip["navigationEndpoint"]["browseEndpoint"];
            Some(DiscoveryFilter {
                title: optional_text(&chip["text"])?,
                browse_id: string(&ep["browseId"])?,
                params: string(&ep["params"]),
                selected: chip["isSelected"] == true,
            })
        })
        .collect();
    Ok(DiscoveryPage {
        page,
        continuation,
        filters,
    })
}
fn tab_browse(value: &Value, lyrics: bool) -> Option<String> {
    let tabs = parse::find(value, "watchNextTabbedResultsRenderer")?["tabs"].as_array()?;
    tabs.iter()
        .filter(|tab| tab["tabRenderer"]["unselectable"] != true)
        .find_map(|tab| {
            let ep = &tab["tabRenderer"]["endpoint"]["browseEndpoint"];
            let id = ep["browseId"].as_str()?;
            let page_type = parse::find(ep, "pageType").and_then(Value::as_str);
            let matches = if lyrics {
                id.starts_with("MPLY") || page_type == Some("MUSIC_PAGE_TYPE_TRACK_LYRICS")
            } else {
                id.starts_with("MPTR") || page_type == Some("MUSIC_PAGE_TYPE_TRACK_RELATED")
            };
            matches.then(|| id.to_owned())
        })
}
fn parse_timed(value: &Value, browse_id: &str) -> Result<TimedLyrics> {
    let raw = parse::find(value, "timedLyricsData").and_then(Value::as_array);
    if let Some(raw) = raw.filter(|raw| !raw.is_empty()) {
        let text = raw
            .iter()
            .filter_map(|line| line["lyricLine"].as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let lines = raw
            .iter()
            .map(|line| {
                let start_ms = number(&line["cueRange"]["startTimeMilliseconds"])?;
                let end_ms = number(&line["cueRange"]["endTimeMilliseconds"])?;
                let text = line["lyricLine"].as_str()?.to_owned();
                (end_ms >= start_ms).then_some(LyricLine {
                    text,
                    start_ms,
                    end_ms,
                })
            })
            .collect::<Option<Vec<_>>>();
        let lines = lines
            .filter(|lines| {
                lines
                    .windows(2)
                    .all(|pair| pair[0].start_ms <= pair[1].start_ms)
            })
            .unwrap_or_default();
        if text.is_empty() {
            return Err(Error::LyricsUnavailable);
        }
        return Ok(TimedLyrics {
            browse_id: browse_id.into(),
            text,
            source: parse::find(value, "sourceMessage").and_then(optional_text),
            timed: !lines.is_empty(),
            timing_availability: if lines.is_empty() {
                "not_provided_by_web"
            } else {
                "available"
            }
            .into(),
            source_client: "WEB_REMIX".into(),
            lines,
        });
    }
    let plain = parse::lyrics(value, browse_id)?;
    Ok(TimedLyrics {
        browse_id: plain.browse_id,
        text: plain.text,
        source: plain.source,
        timed: false,
        timing_availability: "not_provided_by_web".into(),
        source_client: "WEB_REMIX".into(),
        lines: Vec::new(),
    })
}

fn parse_accounts(value: &Value, current: &AccountInfo) -> Result<Vec<AccountChoice>> {
    if crate::auth::explicitly_signed_out(value) {
        return Err(Error::AuthenticationRejected);
    }
    let mut accounts = Vec::new();
    collect(value, "accountItem", &mut accounts);
    if accounts.is_empty() {
        return Err(Error::Protocol(
            "account list contains no account items".into(),
        ));
    }
    accounts
        .into_iter()
        .map(|account| {
            let name = optional_text(&account["accountName"])
                .ok_or_else(|| Error::Protocol("account item is missing name".into()))?;
            let channel_handle = optional_text(&account["channelHandle"]);
            let selected = account["isSelected"] == true;
            let disabled = account["isDisabled"] == true;
            let endpoint = &account["serviceEndpoint"];
            let delegated = parse::find(endpoint, "delegatedSessionId").and_then(string);
            let index = parse::find(endpoint, "sessionIndex")
                .and_then(number)
                .and_then(|n| u32::try_from(n).ok());
            let selector = if disabled {
                None
            } else if selected {
                Some(AccountSelector {
                    auth_user: current.auth_user,
                    delegated_session_id: current.delegated_session_id.clone(),
                    expected_channel_handle: channel_handle.clone(),
                })
            } else if delegated.is_some() || index.is_some() {
                Some(AccountSelector {
                    auth_user: index.unwrap_or(current.auth_user),
                    delegated_session_id: delegated,
                    expected_channel_handle: channel_handle.clone(),
                })
            } else {
                None
            }
            .filter(|selector| {
                selector.validate().is_ok()
                    && ((selector.auth_user == current.auth_user
                        && selector.delegated_session_id == current.delegated_session_id)
                        || selector.expected_channel_handle.is_some())
            });
            let thumbnails = account["accountPhoto"]["thumbnails"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|v| {
                    Some(Thumbnail {
                        url: string(&v["url"])?,
                        width: number(&v["width"]),
                        height: number(&v["height"]),
                    })
                })
                .collect();
            Ok(AccountChoice {
                name,
                channel_handle,
                thumbnails,
                selected,
                disabled,
                selector,
            })
        })
        .collect()
}

impl MusicClient {
    pub fn search_suggestions(&self, query: &str) -> Result<Vec<SearchSuggestion>> {
        crate::operation::ensure(|| {
            nonempty(query, "query")?;
            self.upstream_suggestions(query)
        })
    }
    /// Parameters and continuation tokens come from an earlier feed response.
    pub fn home(&self, params: Option<&str>, continuation: Option<&str>) -> Result<DiscoveryPage> {
        self.discovery_feed("FEmusic_home", params, continuation)
    }
    pub fn explore(
        &self,
        params: Option<&str>,
        continuation: Option<&str>,
    ) -> Result<DiscoveryPage> {
        self.discovery_feed("FEmusic_explore", params, continuation)
    }
    fn discovery_feed(
        &self,
        id: &str,
        params: Option<&str>,
        continuation: Option<&str>,
    ) -> Result<DiscoveryPage> {
        crate::operation::ensure(|| {
            let mut body = json!({"browseId":id});
            if let Some(params) = params {
                nonempty(params, "params")?;
                body["params"] = params.into();
            }
            if let Some(token) = continuation {
                nonempty(token, "continuation")?;
                body["continuation"] = token.into();
            }
            parse_discovery(&self.post("browse", body)?)
        })
    }
    /// Preserve the playlist, index and opaque context supplied by Web. Automix
    /// is returned as an explicit context so hosts decide when to follow it.
    pub fn queue_context(&self, context: &QueueContext) -> Result<QueuePage> {
        crate::operation::ensure(|| {
            let value = self.post("next", context.body()?)?;
            let panel = parse::find(&value, "playlistPanelRenderer")
                .or_else(|| parse::find(&value, "playlistPanelContinuation"));
            let continuation = panel.and_then(direct_continuation);
            let mut actual = context.clone();
            actual.continuation = None;
            let ep = &value["currentVideoEndpoint"]["watchEndpoint"];
            actual.playlist_id = string(&ep["playlistId"])
                .or_else(|| panel.and_then(|p| string(&p["playlistId"])))
                .or(actual.playlist_id);
            actual.video_id = string(&ep["videoId"]).or(actual.video_id);
            actual.params = string(&ep["params"]).or(actual.params);
            actual.index = number(&ep["index"])
                .and_then(|n| u32::try_from(n).ok())
                .or(actual.index);
            actual.queue_context_params =
                string(&value["queueContextParams"]).or(actual.queue_context_params);
            let automix = parse::find(&value, "automixPreviewVideoRenderer")
                .and_then(|v| parse::find(v, "watchPlaylistEndpoint"))
                .and_then(|ep| {
                    Some(QueueContext {
                        video_id: actual.video_id.clone(),
                        playlist_id: Some(string(&ep["playlistId"])?),
                        params: string(&ep["params"]),
                        ..Default::default()
                    })
                });
            Ok(QueuePage {
                page: parse::page(&value)?,
                context: actual,
                continuation,
                lyrics_browse_id: tab_browse(&value, true),
                related_browse_id: tab_browse(&value, false),
                automix,
            })
        })
    }
    /// Use timestamps only when the official Web response supplies them. Web
    /// currently commonly returns plain lyrics; no alternate client is used.
    pub fn timed_lyrics(&self, video_id: &str) -> Result<TimedLyrics> {
        crate::operation::ensure(|| {
            validate_video_id(video_id)?;
            let value = self.post(
                "next",
                json!({"videoId":video_id,"isAudioOnly":true,"enablePersistentPlaylistPanel":true}),
            )?;
            let id = tab_browse(&value, true).ok_or(Error::LyricsUnavailable)?;
            parse_timed(&self.post("browse", json!({"browseId":id}))?, &id)
        })
    }
    /// Enumerate identities exposed by this imported Web session. This is not a
    /// list of every Google account installed in the host browser.
    pub fn accounts(&self) -> Result<Vec<AccountChoice>> {
        crate::operation::ensure(|| {
            let current = self.account()?;
            parse_accounts(&self.post("account/accounts_list", json!({}))?, &current)
        })
    }
    /// Verify a new independent client; the original client's selected identity
    /// and credentials remain intact. Store/export its session separately.
    pub fn select_account(&self, selector: &AccountSelector) -> Result<MusicClient> {
        crate::operation::ensure(|| {
            selector.validate()?;
            if (selector.auth_user != self.config.auth_user
                || selector.delegated_session_id != self.config.delegated_session_id)
                && selector.expected_channel_handle.is_none()
            {
                return Err(Error::InvalidInput(
                    "switching identities requires expected_channel_handle for remote verification"
                        .into(),
                ));
            }
            let session = self
                .browser_session()?
                .ok_or(Error::AuthenticationRequired)?;
            let mut config = self.config.clone();
            session.apply_to(&mut config)?;
            config.auth_user = selector.auth_user;
            config.delegated_session_id = selector.delegated_session_id.clone();
            let client = MusicClient::new(config)?;
            let verified = client.account()?;
            if selector
                .expected_channel_handle
                .as_ref()
                .is_some_and(|expected| verified.channel_handle.as_ref() != Some(expected))
            {
                return Err(Error::AuthenticationRejected);
            }
            Ok(client)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn feed_continuation_never_uses_nested_carousel_token() {
        let v = json!({"contents":{"sectionListRenderer":{"contents":[{"musicCarouselShelfRenderer":{"contents":[],"continuations":[{"nextContinuationData":{"continuation":"shelf"}}]}}],"continuations":[{"nextContinuationData":{"continuation":"feed"}}],"header":{"chipCloudRenderer":{"chips":[{"chipCloudChipRenderer":{"text":{"runs":[{"text":"Relax"}]},"navigationEndpoint":{"browseEndpoint":{"browseId":"FEmusic_home","params":"filter"}},"isSelected":true}}]}}}}});
        let p = parse_discovery(&v).unwrap();
        assert_eq!(p.continuation.as_deref(), Some("feed"));
        assert_eq!(p.filters.len(), 1);
        assert!(p.filters[0].selected);
    }
    #[test]
    fn queue_roundtrips_context_and_rejects_empty_or_invalid_ids() {
        let q = QueueContext {
            video_id: Some("4D7u5KF7SP8".into()),
            playlist_id: Some("PLtest".into()),
            params: Some("context".into()),
            index: Some(7),
            queue_context_params: Some("queue".into()),
            ..Default::default()
        };
        let body = q.body().unwrap();
        assert_eq!(body["index"], 7);
        assert_eq!(body["queueContextParams"], "queue");
        assert!(QueueContext::default().body().is_err());
        assert!(QueueContext {
            video_id: Some("bad".into()),
            ..Default::default()
        }
        .body()
        .is_err());
    }
    #[test]
    fn lyrics_never_invent_timing_for_plain_or_partial_cues() {
        let plain = json!({"musicDescriptionShelfRenderer":{"description":{"runs":[{"text":"One\nTwo"}]},"footer":{"simpleText":"Provider"}}});
        let p = parse_timed(&plain, "MPLYtest").unwrap();
        assert!(!p.timed);
        assert!(p.lines.is_empty());
        assert_eq!(p.timing_availability, "not_provided_by_web");
        let partial = json!({"timedLyricsData":[{"lyricLine":"One","cueRange":{"startTimeMilliseconds":"12","endTimeMilliseconds":"20"}},{"lyricLine":"Two"}]});
        let p = parse_timed(&partial, "MPLYtest").unwrap();
        assert!(!p.timed);
        assert_eq!(p.text, "One\nTwo");
    }
    #[test]
    fn timed_lyrics_validate_ranges_and_order() {
        let valid = json!({"timedLyricsData":[{"lyricLine":"One","cueRange":{"startTimeMilliseconds":"12","endTimeMilliseconds":"20"}},{"lyricLine":"Two","cueRange":{"startTimeMilliseconds":20,"endTimeMilliseconds":40}}],"sourceMessage":"Provider"});
        let p = parse_timed(&valid, "MPLYtest").unwrap();
        assert!(p.timed);
        assert_eq!(p.lines[0].start_ms, 12);
        assert_eq!(p.source.as_deref(), Some("Provider"));
        let mut invalid = valid;
        invalid["timedLyricsData"][1]["cueRange"]["startTimeMilliseconds"] = 5.into();
        assert!(!parse_timed(&invalid, "MPLYtest").unwrap().timed);
    }
    #[test]
    fn accounts_never_use_gaia_id_as_delegated_session() {
        let current = AccountInfo {
            name: "Current".into(),
            channel_handle: Some("@current".into()),
            photo_url: None,
            auth_user: 2,
            delegated_session_id: None,
        };
        let v = json!({"contents":[{"accountItem":{"accountName":{"simpleText":"Current"},"isSelected":true,"channelHandle":{"simpleText":"@current"}}},{"accountItem":{"accountName":{"simpleText":"Other"},"serviceEndpoint":{"selectActiveIdentityEndpoint":{"supportedTokens":[{"accountStateToken":{"obfuscatedGaiaId":"not-a-session-id"}}]}}}},{"accountItem":{"accountName":{"simpleText":"Brand"},"channelHandle":{"simpleText":"@brand"},"serviceEndpoint":{"delegatedSessionId":"brand-session"}}},{"accountItem":{"accountName":{"simpleText":"Disabled"},"isSelected":true,"isDisabled":true}}]});
        let p = parse_accounts(&v, &current).unwrap();
        assert_eq!(p[0].selector.as_ref().unwrap().auth_user, 2);
        assert!(p[1].selector.is_none());
        assert_eq!(
            p[2].selector
                .as_ref()
                .unwrap()
                .delegated_session_id
                .as_deref(),
            Some("brand-session")
        );
        assert!(p[3].selector.is_none());
        let unidentified = json!({"accountItem":{"accountName":{"simpleText":"Brand"},"serviceEndpoint":{"delegatedSessionId":"another-brand"}}});
        assert!(parse_accounts(&unidentified, &current).unwrap()[0]
            .selector
            .is_none());
        assert!(parse_accounts(&json!({"responseContext":{"loggedOut":true}}), &current).is_err());
    }
}
