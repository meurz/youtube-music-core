use crate::{model::*, Error, Result};
use serde_json::Value;

pub(crate) fn text(v: &Value) -> String {
    if let Some(s) = v
        .as_str()
        .or_else(|| v.get("simpleText").and_then(Value::as_str))
    {
        return s.to_owned();
    }
    v.get("runs")
        .and_then(Value::as_array)
        .map(|runs| {
            runs.iter()
                .filter_map(|r| r.get("text").and_then(Value::as_str))
                .collect()
        })
        .unwrap_or_default()
}

pub(crate) fn find<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    match v {
        Value::Object(m) => m.get(key).or_else(|| m.values().find_map(|v| find(v, key))),
        Value::Array(a) => a.iter().find_map(|v| find(v, key)),
        _ => None,
    }
}

fn number(v: &Value) -> Option<u64> {
    v.as_u64().or_else(|| v.as_str()?.parse().ok())
}

pub fn duration(s: &str) -> Option<u64> {
    let parts = s.split(':').collect::<Vec<_>>();
    if !(2..=3).contains(&parts.len()) {
        return None;
    }
    let mut seconds = 0u64;
    for (i, part) in parts.iter().enumerate() {
        let n: u64 = part.parse().ok()?;
        if i > 0 && n >= 60 {
            return None;
        }
        seconds = seconds.checked_mul(60)?.checked_add(n)?;
    }
    Some(seconds)
}

fn thumbnails(v: &Value) -> Vec<Thumbnail> {
    find(v, "thumbnails")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|v| {
            Some(Thumbnail {
                url: v["url"].as_str()?.into(),
                width: v["width"].as_u64(),
                height: v["height"].as_u64(),
            })
        })
        .collect()
}

fn collect_links(v: &Value, links: &mut Vec<(Link, String)>) {
    match v {
        Value::Object(m) => {
            if let (Some(name), Some(ep)) = (
                m.get("text").and_then(Value::as_str),
                m.get("navigationEndpoint")
                    .and_then(|x| x.get("browseEndpoint")),
            ) {
                if let Some(id) = ep["browseId"].as_str() {
                    let link = Link {
                        name: name.into(),
                        browse_id: id.into(),
                    };
                    if !links.iter().any(|(l, _)| *l == link) {
                        links.push((
                            link,
                            find(ep, "pageType")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .into(),
                        ));
                    }
                }
            }
            for v in m.values() {
                collect_links(v, links);
            }
        }
        Value::Array(a) => {
            for v in a {
                collect_links(v, links);
            }
        }
        _ => {}
    }
}

fn item_actions(v: &Value) -> ItemActions {
    fn walk(v: &Value, out: &mut ItemActions) {
        match v {
            Value::Object(m) => {
                if let Some(rating) = m.get("likeStatus").and_then(Value::as_str) {
                    out.rating = match rating {
                        "LIKE" => Some(crate::mutations::Rating::Like),
                        "DISLIKE" => Some(crate::mutations::Rating::Dislike),
                        "INDIFFERENT" => Some(crate::mutations::Rating::Indifferent),
                        _ => out.rating,
                    };
                }
                if let Some(button) = m.get("subscribeButtonRenderer") {
                    out.subscribed = button["subscribed"].as_bool();
                }
                if let Some(editable) = m.get("isEditable").and_then(Value::as_bool) {
                    out.can_edit = Some(editable);
                }
                if m.contains_key("playlistEditorEndpoint")
                    || m.contains_key("musicEditablePlaylistDetailHeaderRenderer")
                {
                    out.can_edit = Some(true);
                }
                if let Some(toggle) = m.get("toggleMenuServiceItemRenderer") {
                    let icon = toggle["defaultIcon"]["iconType"].as_str();
                    if matches!(icon, Some("BOOKMARK" | "BOOKMARK_BORDER")) {
                        let default = find(&toggle["defaultServiceEndpoint"], "feedbackToken")
                            .and_then(Value::as_str)
                            .map(str::to_owned);
                        let toggled = find(&toggle["toggledServiceEndpoint"], "feedbackToken")
                            .and_then(Value::as_str)
                            .map(str::to_owned);
                        let remove_default = icon == Some("BOOKMARK");
                        out.in_library = Some(remove_default || toggle["isToggled"] == true);
                        if remove_default {
                            out.add_library_token = toggled;
                            out.remove_library_token = default;
                        } else {
                            out.add_library_token = default;
                            out.remove_library_token = toggled;
                        }
                    }
                }
                // Header album/playlist save buttons advertise LIKE/INDIFFERENT
                // against a playlist target; song rating is a different state.
                if let Some(button) = m.get("likeButtonRenderer") {
                    if find(button, "playlistId").is_some() {
                        out.in_library = button["likeStatus"].as_str().and_then(|s| match s {
                            "LIKE" => Some(true),
                            "INDIFFERENT" | "DISLIKE" => Some(false),
                            _ => None,
                        });
                    }
                }
                if let Some(button) = m.get("toggleButtonRenderer") {
                    let target = &button["defaultServiceEndpoint"]["likeEndpoint"]["target"];
                    if target["playlistId"].is_string()
                        && matches!(
                            button["defaultIcon"]["iconType"].as_str(),
                            Some("BOOKMARK_BORDER" | "BOOKMARK")
                        )
                    {
                        out.in_library = button["isToggled"].as_bool().map(|toggled| {
                            toggled != (button["defaultIcon"]["iconType"] == "BOOKMARK")
                        });
                    }
                }
                for child in m.values() {
                    walk(child, out);
                }
            }
            Value::Array(a) => {
                for child in a {
                    walk(child, out);
                }
            }
            _ => {}
        }
    }
    let mut out = ItemActions::default();
    walk(v, &mut out);
    out
}

fn item_playlist_id(v: &Value, browse_id: Option<&str>) -> Option<String> {
    browse_id
        .and_then(|s| s.strip_prefix("VL"))
        .filter(|s| !s.is_empty())
        .or_else(|| v["playlistId"].as_str())
        .or_else(|| find(&v["buttons"], "playlistId").and_then(Value::as_str))
        .or_else(|| find(&v["menu"], "playlistId").and_then(Value::as_str))
        .or_else(|| find(&v["navigationEndpoint"], "playlistId").and_then(Value::as_str))
        .or_else(|| find(&v["overlay"], "playlistId").and_then(Value::as_str))
        .map(str::to_owned)
}

fn parse_item(r: &Value) -> Option<Item> {
    let first = &r["flexColumns"][0]["musicResponsiveListItemFlexColumnRenderer"]["text"];
    let title = if first.is_null() {
        text(&r["title"])
    } else {
        text(first)
    };
    if title.is_empty() {
        return None;
    }
    let endpoint = if r["navigationEndpoint"].is_object() {
        &r["navigationEndpoint"]
    } else {
        &first["runs"][0]["navigationEndpoint"]
    };
    let video_id = r["playlistItemData"]["videoId"]
        .as_str()
        .or_else(|| r["videoId"].as_str())
        .or_else(|| find(endpoint, "videoId").and_then(Value::as_str))
        .or_else(|| find(&r["overlay"], "videoId").and_then(Value::as_str))
        .or_else(|| find(&r["menu"], "removedVideoId").and_then(Value::as_str))
        .map(str::to_owned);
    let browse_id = endpoint["browseEndpoint"]["browseId"]
        .as_str()
        .map(str::to_owned);
    let page_type = find(endpoint, "pageType")
        .and_then(Value::as_str)
        .unwrap_or("");
    let kind = match page_type {
        "MUSIC_PAGE_TYPE_ALBUM" => "album",
        "MUSIC_PAGE_TYPE_ARTIST" | "MUSIC_PAGE_TYPE_USER_CHANNEL" => "artist",
        "MUSIC_PAGE_TYPE_PLAYLIST" => "playlist",
        _ if video_id.is_some() => {
            let music_type = find(endpoint, "musicVideoType")
                .or_else(|| find(&r["overlay"], "musicVideoType"))
                .and_then(Value::as_str);
            if music_type == Some("MUSIC_VIDEO_TYPE_OMV")
                || music_type == Some("MUSIC_VIDEO_TYPE_UGC")
            {
                "video"
            } else {
                "song"
            }
        }
        _ => "unknown",
    }
    .to_owned();
    let mut links = Vec::new();
    for key in [
        "flexColumns",
        "subtitle",
        "longBylineText",
        "shortBylineText",
    ] {
        collect_links(&r[key], &mut links);
    }
    let artists = links
        .iter()
        .filter(|(l, t)| {
            t == "MUSIC_PAGE_TYPE_ARTIST" || (t.is_empty() && l.browse_id.starts_with("UC"))
        })
        .map(|(l, _)| l.clone())
        .collect();
    let album = links
        .iter()
        .find(|(_, t)| t == "MUSIC_PAGE_TYPE_ALBUM")
        .map(|(l, _)| l.clone());
    let duration_seconds = duration(&text(&r["lengthText"]))
        .or_else(|| {
            text(&r["subtitle"])
                .split('•')
                .find_map(|s| duration(s.trim()))
        })
        .or_else(|| {
            ["fixedColumns", "flexColumns"].iter().find_map(|key| {
                r[*key].as_array()?.iter().find_map(|c| {
                    let t = find(c, "text")?;
                    duration(&text(t)).or_else(|| {
                        t["runs"]
                            .as_array()?
                            .iter()
                            .find_map(|run| duration(run["text"].as_str()?))
                    })
                })
            })
        });
    let explicit = r
        .get("badges")
        .is_some_and(|b| b.to_string().contains("MUSIC_EXPLICIT_BADGE"));
    Some(Item {
        title,
        kind,
        video_id,
        playlist_id: item_playlist_id(r, browse_id.as_deref()),
        browse_id,
        artists,
        album,
        duration_seconds,
        thumbnails: thumbnails(&r["thumbnail"]),
        explicit,
        available: r["isPlayable"].as_bool().or_else(|| {
            r["musicItemRendererDisplayPolicy"]
                .as_str()
                .map(|p| p != "MUSIC_ITEM_RENDERER_DISPLAY_POLICY_GREY_OUT")
        }),
        set_video_id: r["playlistItemData"]["playlistSetVideoId"]
            .as_str()
            .or_else(|| r["setVideoId"].as_str())
            .or_else(|| find(&r["menu"], "setVideoId").and_then(Value::as_str))
            .map(str::to_owned),
        actions: item_actions(r),
    })
}

fn items(v: &Value, out: &mut Vec<Item>) {
    match v {
        Value::Object(m) => {
            for key in [
                "musicResponsiveListItemRenderer",
                "musicTwoRowItemRenderer",
                "playlistPanelVideoRenderer",
            ] {
                if let Some(r) = m.get(key) {
                    if let Some(item) = parse_item(r) {
                        out.push(item);
                    }
                    return;
                }
            }
            for v in m.values() {
                items(v, out);
            }
        }
        Value::Array(a) => {
            for v in a {
                items(v, out);
            }
        }
        _ => {}
    }
}

fn continuation(v: &Value) -> Option<String> {
    for key in [
        "nextContinuationData",
        "nextRadioContinuationData",
        "continuationCommand",
    ] {
        if let Some(s) = find(v, key)
            .and_then(|x| {
                x.get(if key == "continuationCommand" {
                    "token"
                } else {
                    "continuation"
                })
            })
            .and_then(Value::as_str)
        {
            return Some(s.into());
        }
    }
    None
}

fn sections(v: &Value, out: &mut Vec<Section>) {
    match v {
        Value::Object(m) => {
            for key in [
                "musicShelfRenderer",
                "musicShelfContinuation",
                "musicPlaylistShelfRenderer",
                "musicPlaylistShelfContinuation",
                "musicCarouselShelfRenderer",
                "playlistPanelRenderer",
                "playlistPanelContinuation",
                "gridRenderer",
                "gridContinuation",
            ] {
                if let Some(r) = m.get(key) {
                    let mut entries = Vec::new();
                    items(r, &mut entries);
                    let title = if r["title"].is_null() {
                        find(&r["header"], "title").map(text).unwrap_or_default()
                    } else {
                        text(&r["title"])
                    };
                    out.push(Section {
                        title,
                        items: entries,
                        continuation: continuation(r),
                    });
                    return;
                }
            }
            for v in m.values() {
                sections(v, out);
            }
        }
        Value::Array(a) => {
            for v in a {
                sections(v, out);
            }
        }
        _ => {}
    }
}

pub fn page(v: &Value) -> Result<Page> {
    if !v.is_object() {
        return Err(Error::Protocol("expected a JSON object".into()));
    }
    let mut parsed = Vec::new();
    sections(v, &mut parsed);
    if parsed.is_empty() {
        let mut entries = Vec::new();
        items(v, &mut entries);
        if !entries.is_empty() || continuation(v).is_some() {
            parsed.push(Section {
                title: String::new(),
                items: entries,
                continuation: continuation(v),
            });
        }
    }
    let header = [
        "musicDetailHeaderRenderer",
        "musicResponsiveHeaderRenderer",
        "musicImmersiveHeaderRenderer",
        "musicVisualHeaderRenderer",
    ]
    .iter()
    .find_map(|key| find(v, key));
    let title = header.map(|r| text(&r["title"])).filter(|s| !s.is_empty());
    if parsed.is_empty()
        && v.get("contents").is_none()
        && v.get("continuationContents").is_none()
        && v.get("onResponseReceivedActions").is_none()
    {
        return Err(Error::Protocol("missing page contents".into()));
    }
    Ok(Page {
        title,
        sections: parsed,
        playlist_id: header.and_then(|h| item_playlist_id(h, None)),
        actions: header.map(item_actions).unwrap_or_default(),
    })
}

fn byte_range(value: &Value) -> Option<ByteRange> {
    let start = number(&value["start"])?;
    let end = number(&value["end"])?;
    (start <= end).then_some(ByteRange { start, end })
}

pub fn player(v: &Value) -> Result<Player> {
    parse_player(v, false)
}

/// Only called after every Web audio challenge has been resolved locally.
pub(crate) fn resolved_web_player(v: &Value) -> Result<Player> {
    parse_player(v, true)
}

fn parse_player(v: &Value, resolved: bool) -> Result<Player> {
    let status = v["playabilityStatus"]["status"]
        .as_str()
        .ok_or_else(|| Error::Protocol("missing playabilityStatus".into()))?
        .to_owned();
    let details = &v["videoDetails"];
    let track = details["videoId"].as_str().map(|id| Track {
        video_id: id.into(),
        title: text(&details["title"]),
        author: details["author"].as_str().map(str::to_owned),
        channel_id: details["channelId"].as_str().map(str::to_owned),
        duration_seconds: number(&details["lengthSeconds"]),
        thumbnails: thumbnails(details),
    });
    let mut audio_streams = Vec::new();
    let mut unresolved_audio_formats = 0;
    for format in v["streamingData"]["adaptiveFormats"]
        .as_array()
        .into_iter()
        .flatten()
    {
        let Some(mime) = format["mimeType"]
            .as_str()
            .filter(|m| m.starts_with("audio/"))
        else {
            continue;
        };
        if crate::parse::is_encrypted_format(format) {
            continue;
        }
        // URLs with an n challenge are not ready for playback. Do not claim otherwise.
        let url = format["url"]
            .as_str()
            .and_then(|u| reqwest::Url::parse(u).ok())
            .filter(|u| {
                u.scheme() == "https" && (resolved || !u.query_pairs().any(|(key, _)| key == "n"))
            });
        let (Some(url), Some(itag)) = (url, format["itag"].as_u64()) else {
            unresolved_audio_formats += 1;
            continue;
        };
        audio_streams.push(AudioStream {
            itag,
            expires_at: url
                .query_pairs()
                .find_map(|(key, value)| (key == "expire").then(|| value.parse().ok()).flatten()),
            url: url.into(),
            mime_type: mime.into(),
            bitrate: number(&format["bitrate"]),
            content_length: number(&format["contentLength"]),
            audio_quality: format["audioQuality"].as_str().map(str::to_owned),
            init_range: byte_range(&format["initRange"]),
            index_range: byte_range(&format["indexRange"]),
            duration_ms: number(&format["approxDurationMs"]),
            audio_sample_rate: number(&format["audioSampleRate"]).and_then(|n| n.try_into().ok()),
            audio_channels: number(&format["audioChannels"]).and_then(|n| n.try_into().ok()),
            http_headers: Default::default(),
            source_client: None,
            verification: None,
        });
    }
    audio_streams.sort_by_key(|s| std::cmp::Reverse(s.bitrate.unwrap_or(0)));
    Ok(Player {
        sabr: crate::delivery::parse_sabr(v, resolved),
        source_client: None,
        track,
        status,
        reason: v["playabilityStatus"]["reason"].as_str().map(str::to_owned),
        expires_in_seconds: number(&v["streamingData"]["expiresInSeconds"]),
        audio_streams,
        unresolved_audio_formats,
        resolution_error: None,
    })
}

pub fn lyrics(v: &Value, browse_id: &str) -> Result<Lyrics> {
    let r = find(v, "musicDescriptionShelfRenderer").ok_or(Error::LyricsUnavailable)?;
    let text = text(&r["description"]);
    if text.is_empty() {
        return Err(Error::LyricsUnavailable);
    }
    let source = self::text(&r["footer"]);
    Ok(Lyrics {
        browse_id: browse_id.into(),
        text,
        source: (!source.is_empty()).then_some(source),
    })
}

// Filtering unsupported encrypted formats is not a playback or license feature.
// Unknown or malformed nonempty markers must not become clear audio candidates.
pub(crate) fn is_encrypted_format(format: &Value) -> bool {
    match format.get("drmFamilies") {
        None | Some(Value::Null) => false,
        Some(Value::Array(values)) => !values.is_empty(),
        Some(_) => true,
    }
}

#[cfg(test)]
mod mutation_tests {
    use super::*;
    use serde_json::json;
    fn toggle(icon: &str, toggled: bool) -> Value {
        json!({"toggleMenuServiceItemRenderer":{
            "defaultIcon":{"iconType":icon},"isToggled":toggled,
            "defaultServiceEndpoint":{"feedbackEndpoint":{"feedbackToken":"fixture-default"}},
            "toggledServiceEndpoint":{"feedbackEndpoint":{"feedbackToken":"fixture-toggled"}}
        }})
    }
    #[test]
    fn tokens_follow_server_actions_not_localized_labels() {
        let add = item_actions(&json!({"menu":{"items":[toggle("BOOKMARK_BORDER",true)]}}));
        assert_eq!(add.in_library, Some(true));
        assert_eq!(add.add_library_token.as_deref(), Some("fixture-default"));
        assert_eq!(add.remove_library_token.as_deref(), Some("fixture-toggled"));
        let remove = item_actions(&toggle("BOOKMARK", false));
        assert_eq!(remove.in_library, Some(true));
        assert_eq!(
            remove.remove_library_token.as_deref(),
            Some("fixture-default")
        );
        assert_eq!(item_actions(&toggle("KEEP", false)).in_library, None);
    }
    #[test]
    fn unavailable_duplicate_song_retains_remove_identifiers() {
        let item=parse_item(&json!({
            "title":{"simpleText":"Unavailable"},
            "musicItemRendererDisplayPolicy":"MUSIC_ITEM_RENDERER_DISPLAY_POLICY_GREY_OUT",
            "menu":{"menuRenderer":{"items":[{"menuServiceItemRenderer":{"serviceEndpoint":{
                "playlistEditEndpoint":{"playlistId":"PLfixture","actions":[{"action":"ACTION_REMOVE_VIDEO","removedVideoId":"4D7u5KF7SP8","setVideoId":"entry-2"}]}
            }}}]}}
        })).unwrap();
        assert_eq!(item.available, Some(false));
        assert_eq!(item.video_id.as_deref(), Some("4D7u5KF7SP8"));
        assert_eq!(item.set_video_id.as_deref(), Some("entry-2"));
        assert_eq!(item.playlist_id.as_deref(), Some("PLfixture"));
    }
    #[test]
    fn page_actions_do_not_leak_the_first_songs_state() {
        let page=page(&json!({
            "header":{"musicResponsiveHeaderRenderer":{"title":{"simpleText":"Album"},"buttons":[{"likeButtonRenderer":{"likeStatus":"INDIFFERENT","target":{"playlistId":"OLAKfixture"}}}]}},
            "contents":{"musicShelfRenderer":{"contents":[{"musicResponsiveListItemRenderer":{"title":{"simpleText":"Song"},"menu":{"items":[toggle("BOOKMARK",false)]}}}]}}
        })).unwrap();
        assert_eq!(page.playlist_id.as_deref(), Some("OLAKfixture"));
        assert_eq!(page.actions.in_library, Some(false));
        assert!(page.actions.remove_library_token.is_none());
        assert_eq!(page.sections[0].items[0].actions.in_library, Some(true));
    }
    #[test]
    fn modern_header_save_toggle_and_editor_are_exposed() {
        let header = json!({"buttons":[
            {"toggleButtonRenderer":{"isToggled":true,"defaultIcon":{"iconType":"BOOKMARK_BORDER"},"defaultServiceEndpoint":{"likeEndpoint":{"status":"LIKE","target":{"playlistId":"OLAKfixture"}}}}},
            {"buttonRenderer":{"navigationEndpoint":{"playlistEditorEndpoint":{"playlistId":"PLfixture"}}}}
        ]});
        let actions = item_actions(&header);
        assert_eq!(actions.in_library, Some(true));
        assert_eq!(actions.can_edit, Some(true));
        assert_eq!(item_actions(&json!({})).can_edit, None);
    }
}
