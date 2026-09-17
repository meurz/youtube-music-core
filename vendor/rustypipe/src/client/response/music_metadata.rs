//! Small compatibility extension for Web Music actions omitted by upstream models.
use crate::model::MusicMetadata;
use serde_json::Value;

fn find<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    match value {
        Value::Object(map) => map
            .get(key)
            .or_else(|| map.values().find_map(|v| find(v, key))),
        Value::Array(items) => items.iter().find_map(|v| find(v, key)),
        _ => None,
    }
}

pub(crate) fn extract(value: &Value) -> MusicMetadata {
    let mut metadata = actions(value);
    metadata.set_video_id = value["playlistItemData"]["playlistSetVideoId"]
        .as_str()
        .or_else(|| value["setVideoId"].as_str())
        .or_else(|| find(&value["menu"], "setVideoId").and_then(Value::as_str))
        .map(str::to_owned);
    metadata.playlist_id = value["playlistId"]
        .as_str()
        .or_else(|| find(&value["buttons"], "playlistId").and_then(Value::as_str))
        .or_else(|| find(&value["menu"], "playlistId").and_then(Value::as_str))
        .or_else(|| find(&value["navigationEndpoint"], "playlistId").and_then(Value::as_str))
        .or_else(|| find(&value["overlay"], "playlistId").and_then(Value::as_str))
        .map(str::to_owned);
    metadata.explicit = contains_explicit(&value["badges"]);
    metadata.available = value["isPlayable"].as_bool().or_else(|| {
        value["musicItemRendererDisplayPolicy"]
            .as_str()
            .map(|policy| policy != "MUSIC_ITEM_RENDERER_DISPLAY_POLICY_GREY_OUT")
    });
    metadata
}

pub(crate) fn header(value: &Value) -> MusicMetadata {
    let renderer = [
        "musicEditablePlaylistDetailHeaderRenderer",
        "musicDetailHeaderRenderer",
        "musicResponsiveHeaderRenderer",
        "musicImmersiveHeaderRenderer",
        "musicVisualHeaderRenderer",
    ]
    .iter()
    .find_map(|key| find(value, key));
    let Some(renderer) = renderer else {
        return MusicMetadata::default();
    };
    // Preserve editor permissions and controls on the enclosing editable header,
    // not merely the nested title/cover renderer. The caller supplies header scope.
    let mut metadata = actions(value);
    metadata.playlist_id = find(renderer, "playlistId")
        .and_then(Value::as_str)
        .map(str::to_owned);
    metadata.explicit = find(renderer, "badges").is_some_and(contains_explicit);
    metadata
}

fn actions(v: &Value) -> MusicMetadata {
    fn walk(v: &Value, out: &mut MusicMetadata) {
        match v {
            Value::Object(m) => {
                if let Some(rating) = m.get("likeStatus").and_then(Value::as_str) {
                    out.rating = match rating {
                        "LIKE" => Some("LIKE".into()),
                        "DISLIKE" => Some("DISLIKE".into()),
                        "INDIFFERENT" => Some("INDIFFERENT".into()),
                        _ => out.rating.clone(),
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
                    out.can_edit.get_or_insert(true);
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
                for (key, child) in m {
                    // A search card can contain tracks with unrelated actions.
                    if key != "contents" {
                        walk(child, out);
                    }
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
    let mut out = MusicMetadata::default();
    walk(v, &mut out);
    out
}

fn contains_explicit(value: &Value) -> bool {
    match value {
        Value::Object(map) => {
            map.get("iconType").and_then(Value::as_str) == Some("MUSIC_EXPLICIT_BADGE")
                || map.values().any(contains_explicit)
        }
        Value::Array(items) => items.iter().any(contains_explicit),
        _ => false,
    }
}

pub(crate) fn header_node(
    root: &crate::json::JsonNode<'_>,
) -> Result<MusicMetadata, crate::error::ExtractionError> {
    use crate::json::ytq;
    let candidate = root.first_of(&[
        ytq!(.header),
        ytq!(.contents.twoColumnBrowseResultsRenderer.tabs[0].tabRenderer.content.sectionListRenderer.contents[0]),
        ytq!(.contents.twoColumnBrowseResultsRenderer.contents[0].tabRenderer.content.sectionListRenderer.contents[0]),
    ]);
    candidate
        .map(|node| node.deserialize::<Value>().map(|value| header(&value)))
        .transpose()
        .map(Option::unwrap_or_default)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn retains_duplicate_entry_ids_and_server_states() {
        let metadata = extract(&json!({
            "playlistItemData":{"videoId":"video000001","playlistSetVideoId":"second-entry"},
            "musicItemRendererDisplayPolicy":"MUSIC_ITEM_RENDERER_DISPLAY_POLICY_GREY_OUT",
            "badges":[{"icon":{"iconType":"OTHER"}},{"icon":{"iconType":"MUSIC_EXPLICIT_BADGE"}}],
            "menu":{"toggleMenuServiceItemRenderer":{
                "defaultIcon":{"iconType":"BOOKMARK_BORDER"},"isToggled":false,
                "defaultServiceEndpoint":{"feedbackEndpoint":{"feedbackToken":"add-fixture"}},
                "toggledServiceEndpoint":{"feedbackEndpoint":{"feedbackToken":"remove-fixture"}}
            }}
        }));
        assert_eq!(metadata.set_video_id.as_deref(), Some("second-entry"));
        assert_eq!(metadata.available, Some(false));
        assert!(metadata.explicit);
        assert_eq!(metadata.in_library, Some(false));
        assert_eq!(metadata.add_library_token.as_deref(), Some("add-fixture"));
        assert_eq!(
            metadata.remove_library_token.as_deref(),
            Some("remove-fixture")
        );
    }

    #[test]
    fn wrapped_editable_header_retains_outer_permissions_without_track_actions() {
        let value = json!({"musicEditablePlaylistDetailHeaderRenderer":{
            "isEditable":true,
            "header":{"musicDetailHeaderRenderer":{
                "title":{"simpleText":"My playlist"},
                "buttons":[{"likeButtonRenderer":{"likeStatus":"INDIFFERENT","target":{"playlistId":"PLfixture"}}}]
            }},
            "editHeader":{"musicPlaylistEditHeaderRenderer":{"editTitle":{"playlistEditorEndpoint":{"playlistId":"PLfixture"}}}},
            "contents":[{"musicResponsiveListItemRenderer":{"likeStatus":"LIKE"}}]
        }});
        let metadata = header(&value);
        assert_eq!(metadata.can_edit, Some(true));
        assert_eq!(metadata.playlist_id.as_deref(), Some("PLfixture"));
        assert_eq!(metadata.in_library, Some(false));
        assert_eq!(metadata.rating.as_deref(), Some("INDIFFERENT"));
        let without_flag = json!({"musicEditablePlaylistDetailHeaderRenderer":{
            "header":{"musicDetailHeaderRenderer":{"title":{"simpleText":"Playlist"}}}
        }});
        assert_eq!(header(&without_flag).can_edit, Some(true));
        let explicitly_disabled = json!({"musicEditablePlaylistDetailHeaderRenderer":{
            "isEditable":false,
            "header":{"musicDetailHeaderRenderer":{"title":{"simpleText":"Playlist"}}}
        }});
        assert_eq!(header(&explicitly_disabled).can_edit, Some(false));
    }

    #[test]
    fn card_actions_do_not_inherit_nested_tracks() {
        let metadata = extract(&json!({
            "contents":[{"musicResponsiveListItemRenderer":{"likeStatus":"LIKE"}}]
        }));
        assert!(metadata.rating.is_none());
        assert!(metadata.in_library.is_none());
    }
}

/// Unavailable playlist entries sometimes retain their video ID only in the
/// remove action. Normalize these two fields before the usual typed parser.
pub(crate) fn normalize_unavailable_entry(value: &mut Value) {
    if value["playlistItemData"]["videoId"].is_null() {
        if let Some(id) = find(&value["menu"], "removedVideoId")
            .and_then(Value::as_str)
            .map(str::to_owned)
        {
            value["playlistItemData"] = serde_json::json!({"videoId":id});
        }
    }
    if value["flexColumns"].is_null() && !value["title"].is_null() {
        value["flexColumns"] = serde_json::json!([{
            "musicResponsiveListItemFlexColumnRenderer":{"text":value["title"].clone()}
        }]);
    }
}
