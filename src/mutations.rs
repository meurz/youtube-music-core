//! Authenticated Web mutations. Writes are sent once: callers must reconcile an
//! uncertain network outcome before retrying a non-idempotent playlist edit.
use crate::{
    client::{nonempty, validate_video_id},
    Error, MusicClient, Result,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, clap::ValueEnum, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Rating {
    Like,
    Dislike,
    Indifferent,
}
impl Rating {
    fn endpoint(self) -> &'static str {
        match self {
            Self::Like => "like/like",
            Self::Dislike => "like/dislike",
            Self::Indifferent => "like/removelike",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, clap::ValueEnum, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PlaylistPrivacy {
    #[default]
    Private,
    Public,
    Unlisted,
}
impl PlaylistPrivacy {
    fn wire(self) -> &'static str {
        match self {
            Self::Private => "PRIVATE",
            Self::Public => "PUBLIC",
            Self::Unlisted => "UNLISTED",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreatePlaylist {
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub privacy: PlaylistPrivacy,
    #[serde(default)]
    pub video_ids: Vec<String>,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EditPlaylist {
    pub title: Option<String>,
    pub description: Option<String>,
    pub privacy: Option<PlaylistPrivacy>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PlaylistEntry {
    pub video_id: String,
    /// The server's unique playlist entry ID, not the video ID. Duplicate songs
    /// have different entry IDs, so removing one never removes all duplicates.
    pub set_video_id: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MutationResult {
    /// `succeeded` is explicit server confirmation; `accepted` means the Web
    /// endpoint acknowledged the request without a separate status field.
    pub status: String,
    pub playlist_id: Option<String>,
    pub added_items: Vec<PlaylistEntry>,
}

fn identifier(value: &str, name: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 512
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err(Error::InvalidInput(format!(
            "{name} must be an opaque YouTube identifier"
        )));
    }
    Ok(())
}
fn playlist_id(value: &str) -> Result<&str> {
    let value = value.strip_prefix("VL").unwrap_or(value);
    identifier(value, "playlist_id")?;
    Ok(value)
}
fn batch<T>(values: &[T], name: &str) -> Result<()> {
    if values.is_empty() || values.len() > 100 {
        return Err(Error::InvalidInput(format!(
            "{name} must contain between 1 and 100 entries"
        )));
    }
    Ok(())
}
fn library_tokens(tokens: &[String]) -> Result<()> {
    batch(tokens, "feedback_tokens")?;
    for token in tokens {
        nonempty(token, "feedback_token")?;
        if token.len() > 32768 || token.chars().any(char::is_control) {
            return Err(Error::InvalidInput("invalid feedback token".into()));
        }
    }
    Ok(())
}
fn channel_id(id: &str) -> Result<()> {
    identifier(id, "channel_id")?;
    if !id.starts_with("UC") {
        return Err(Error::InvalidInput(
            "channel_id must be a UC channel identifier".into(),
        ));
    }
    Ok(())
}

/// Validate every write before one-shot clients can bootstrap or send credentials.
pub(crate) fn validate_request(request: &crate::Request) -> Result<()> {
    use crate::Request;
    match request {
        Request::RateSong { video_id, .. } => validate_video_id(video_id),
        Request::RatePlaylist {
            playlist_id: id, ..
        }
        | Request::DeletePlaylist { playlist_id: id } => playlist_id(id).map(|_| ()),
        Request::EditLibrary { feedback_tokens } => library_tokens(feedback_tokens),
        Request::Subscribe { channel_id: id, .. } => channel_id(id),
        Request::CreatePlaylist { options } => options.validate(),
        Request::EditPlaylist {
            playlist_id: id,
            options,
        } => {
            playlist_id(id)?;
            options.validate()
        }
        Request::AddPlaylistItems {
            playlist_id: id,
            video_ids,
            allow_duplicates,
        } => {
            playlist_id(id)?;
            added_actions(video_ids, *allow_duplicates).map(|_| ())
        }
        Request::RemovePlaylistItems {
            playlist_id: id,
            entries,
        } => {
            playlist_id(id)?;
            removed_actions(entries).map(|_| ())
        }
        Request::MovePlaylistItem {
            playlist_id: id,
            set_video_id,
            before_set_video_id,
        } => {
            playlist_id(id)?;
            move_action(set_video_id, before_set_video_id.as_deref()).map(|_| ())
        }
        _ => Ok(()),
    }
}
fn title(value: &str) -> Result<()> {
    nonempty(value, "title")?;
    if value.chars().count() > 150
        || value.contains(['<', '>'])
        || value.chars().any(char::is_control)
    {
        return Err(Error::InvalidInput("playlist title must be at most 150 characters without angle brackets or control characters".into()));
    }
    Ok(())
}
fn description(value: &str) -> Result<()> {
    if value.chars().count() > 5000 || value.contains(['<', '>']) || value.contains('\0') {
        return Err(Error::InvalidInput(
            "playlist description must be at most 5000 plain-text characters".into(),
        ));
    }
    Ok(())
}
impl CreatePlaylist {
    pub fn validate(&self) -> Result<()> {
        self.body().map(|_| ())
    }
    fn body(&self) -> Result<Value> {
        title(&self.title)?;
        description(&self.description)?;
        if self.video_ids.len() > 100 {
            return Err(Error::InvalidInput(
                "video_ids must contain at most 100 entries".into(),
            ));
        }
        for id in &self.video_ids {
            validate_video_id(id)?;
        }
        Ok(
            json!({"title":self.title,"description":self.description,"privacyStatus":self.privacy.wire(),"videoIds":self.video_ids}),
        )
    }
}
impl EditPlaylist {
    pub fn validate(&self) -> Result<()> {
        self.actions().map(|_| ())
    }
    fn actions(&self) -> Result<Vec<Value>> {
        let mut actions = Vec::new();
        if let Some(value) = &self.title {
            title(value)?;
            actions.push(json!({"action":"ACTION_SET_PLAYLIST_NAME","playlistName":value}));
        }
        if let Some(value) = &self.description {
            description(value)?;
            actions.push(
                json!({"action":"ACTION_SET_PLAYLIST_DESCRIPTION","playlistDescription":value}),
            );
        }
        if let Some(value) = self.privacy {
            actions.push(
                json!({"action":"ACTION_SET_PLAYLIST_PRIVACY","playlistPrivacy":value.wire()}),
            );
        }
        batch(&actions, "playlist edits")?;
        Ok(actions)
    }
}
fn added_actions(ids: &[String], duplicates: bool) -> Result<Vec<Value>> {
    batch(ids, "video_ids")?;
    ids.iter()
        .map(|id| {
            validate_video_id(id)?;
            let mut action = json!({"action":"ACTION_ADD_VIDEO","addedVideoId":id});
            if duplicates {
                action["dedupeOption"] = "DEDUPE_OPTION_SKIP".into();
            }
            Ok(action)
        })
        .collect()
}
fn removed_actions(entries: &[PlaylistEntry]) -> Result<Vec<Value>> {
    batch(entries, "entries")?;
    entries.iter().map(|entry| {
        validate_video_id(&entry.video_id)?; identifier(&entry.set_video_id,"set_video_id")?;
        Ok(json!({"action":"ACTION_REMOVE_VIDEO","removedVideoId":entry.video_id,"setVideoId":entry.set_video_id}))
    }).collect()
}
fn move_action(entry: &str, before: Option<&str>) -> Result<Value> {
    identifier(entry, "set_video_id")?;
    let mut action = json!({"action":"ACTION_MOVE_VIDEO_BEFORE","setVideoId":entry});
    if let Some(before) = before {
        identifier(before, "before_set_video_id")?;
        if before == entry {
            return Err(Error::InvalidInput(
                "cannot move an entry before itself".into(),
            ));
        }
        action["movedSetVideoIdSuccessor"] = before.into();
    }
    Ok(action)
}
fn response(value: Value, expected_status: bool) -> Result<MutationResult> {
    fn uncertain(message: &str) -> Error {
        Error::MutationUncertain(Box::new(Error::Protocol(message.into())))
    }
    if value["actions"].as_array().is_some_and(|actions| {
        actions
            .iter()
            .any(|action| action.get("showEngagementPanelEndpoint").is_some())
    }) {
        return Err(Error::Protocol(
            "YouTube requires account interaction in its official website before this write can complete".into(),
        ));
    }
    if !value.is_object() {
        return Err(uncertain("write endpoint returned an invalid response"));
    }
    if value.get("error").is_some() {
        return Err(Error::Protocol(
            "write endpoint rejected the request".into(),
        ));
    }
    let status = match value["status"].as_str() {
        Some("STATUS_SUCCEEDED" | "SUCCEEDED") => "succeeded",
        Some(_) => {
            return Err(Error::Protocol(
                "YouTube did not confirm the requested write; reload its state before retrying"
                    .into(),
            ))
        }
        None if expected_status => return Err(uncertain("write response is missing its status")),
        None if value.get("responseContext").is_some() => "accepted",
        None => return Err(uncertain("write response is missing its acknowledgement")),
    };
    if value["feedbackResponses"]
        .as_array()
        .is_some_and(|a| a.iter().any(|v| v["isProcessed"] == false))
    {
        return Err(Error::Protocol(
            "YouTube did not process a library action; reload its state before retrying".into(),
        ));
    }
    let added_items = value["playlistEditResults"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|r| {
            let data = &r["playlistEditVideoAddedResultData"];
            Some(PlaylistEntry {
                video_id: data["videoId"].as_str()?.into(),
                set_video_id: data["setVideoId"].as_str()?.into(),
            })
        })
        .collect();
    Ok(MutationResult {
        status: status.into(),
        playlist_id: value["playlistId"].as_str().map(str::to_owned),
        added_items,
    })
}
impl MusicClient {
    fn mutation(&self, endpoint: &str, body: Value, status: bool) -> Result<MutationResult> {
        crate::operation::ensure(|| {
            self.account()?;
            response(self.post_write(endpoint, body)?, status)
        })
    }
    pub fn rate_song(&self, video_id: &str, rating: Rating) -> Result<MutationResult> {
        validate_video_id(video_id)?;
        self.mutation(
            rating.endpoint(),
            json!({"target":{"videoId":video_id}}),
            false,
        )
    }
    /// Albums expose a playlist ID in their header actions. Pass that ID, not
    /// the album's MPRE browse ID, to save/remove an album or playlist.
    pub fn rate_playlist(&self, id: &str, rating: Rating) -> Result<MutationResult> {
        self.mutation(
            rating.endpoint(),
            json!({"target":{"playlistId":playlist_id(id)?}}),
            false,
        )
    }
    /// Apply current server-provided add/remove-library feedback tokens. Tokens
    /// are account/state-specific; refresh the page before repeating an action.
    pub fn edit_library(&self, feedback_tokens: &[String]) -> Result<MutationResult> {
        library_tokens(feedback_tokens)?;
        self.mutation("feedback", json!({"feedbackTokens":feedback_tokens}), false)
    }
    pub fn subscribe_artist(&self, id: &str, subscribed: bool) -> Result<MutationResult> {
        channel_id(id)?;
        self.mutation(
            if subscribed {
                "subscription/subscribe"
            } else {
                "subscription/unsubscribe"
            },
            json!({"channelIds":[id]}),
            false,
        )
    }
    pub fn create_playlist(&self, input: &CreatePlaylist) -> Result<MutationResult> {
        let result = self.mutation("playlist/create", input.body()?, false)?;
        if result.playlist_id.is_none() {
            return Err(Error::MutationUncertain(Box::new(Error::Protocol(
                "playlist creation returned no playlist ID".into(),
            ))));
        }
        Ok(result)
    }
    fn edit_playlist_actions(&self, id: &str, actions: Vec<Value>) -> Result<MutationResult> {
        self.mutation(
            "browse/edit_playlist",
            json!({"playlistId":playlist_id(id)?,"actions":actions}),
            true,
        )
    }
    pub fn edit_playlist(&self, id: &str, input: &EditPlaylist) -> Result<MutationResult> {
        self.edit_playlist_actions(id, input.actions()?)
    }
    pub fn delete_playlist(&self, id: &str) -> Result<MutationResult> {
        self.mutation(
            "playlist/delete",
            json!({"playlistId":playlist_id(id)?}),
            false,
        )
    }
    pub fn add_playlist_items(
        &self,
        id: &str,
        video_ids: &[String],
        allow_duplicates: bool,
    ) -> Result<MutationResult> {
        self.edit_playlist_actions(id, added_actions(video_ids, allow_duplicates)?)
    }
    pub fn remove_playlist_items(
        &self,
        id: &str,
        entries: &[PlaylistEntry],
    ) -> Result<MutationResult> {
        self.edit_playlist_actions(id, removed_actions(entries)?)
    }
    /// Move an entry before another unique entry, or to the end when `None`.
    pub fn move_playlist_item(
        &self,
        id: &str,
        entry: &str,
        before: Option<&str>,
    ) -> Result<MutationResult> {
        self.edit_playlist_actions(id, vec![move_action(entry, before)?])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn duplicate_removal_addresses_the_unique_entry() {
        let entries = [PlaylistEntry {
            video_id: "4D7u5KF7SP8".into(),
            set_video_id: "entry-2".into(),
        }];
        assert_eq!(
            removed_actions(&entries).unwrap(),
            vec![
                json!({"action":"ACTION_REMOVE_VIDEO","removedVideoId":"4D7u5KF7SP8","setVideoId":"entry-2"})
            ]
        );
        assert_eq!(
            move_action("entry-2", Some("entry-1")).unwrap()["movedSetVideoIdSuccessor"],
            "entry-1"
        );
        assert!(move_action("entry-1", Some("entry-1")).is_err());
        assert!(move_action("entry-1", None)
            .unwrap()
            .get("movedSetVideoIdSuccessor")
            .is_none());
    }
    #[test]
    fn explicit_duplicate_policy_and_empty_description() {
        let ids = ["4D7u5KF7SP8".into()];
        assert!(added_actions(&ids, false).unwrap()[0]
            .get("dedupeOption")
            .is_none());
        assert_eq!(
            added_actions(&ids, true).unwrap()[0]["dedupeOption"],
            "DEDUPE_OPTION_SKIP"
        );
        let edit = EditPlaylist {
            description: Some(String::new()),
            ..Default::default()
        };
        assert_eq!(
            edit.actions().unwrap(),
            vec![json!({"action":"ACTION_SET_PLAYLIST_DESCRIPTION","playlistDescription":""})]
        );
        assert!(EditPlaylist::default().validate().is_err());
    }
    #[test]
    fn creation_defaults_private_and_validates_before_sending() {
        let create: CreatePlaylist = serde_json::from_value(json!({"title":"test"})).unwrap();
        assert_eq!(create.body().unwrap()["privacyStatus"], "PRIVATE");
        assert!(CreatePlaylist {
            title: "<invalid>".into(),
            ..create
        }
        .validate()
        .is_err());
        assert!(added_actions(&[], false).is_err());
        assert!(removed_actions(&[]).is_err());
        assert!(playlist_id("VL").is_err());
        assert_eq!(playlist_id("VLPLtest").unwrap(), "PLtest");
    }
    #[test]
    fn mutation_failure_is_not_reported_as_success() {
        assert!(response(json!({"responseContext":{},"actions":[{"showEngagementPanelEndpoint":{"identifier":{"tag":"fixture-dialog"}}}]}),false).is_err());
        assert!(response(json!({"status":"STATUS_FAILED"}), true).is_err());
        assert!(matches!(
            response(json!({"responseContext":{}}), true),
            Err(Error::MutationUncertain(_))
        ));
        assert!(matches!(
            response(json!({}), false),
            Err(Error::MutationUncertain(_))
        ));
        assert!(response(
            json!({"responseContext":{},"feedbackResponses":[{"isProcessed":false}]}),
            false
        )
        .is_err());
        let ok=response(json!({"status":"STATUS_SUCCEEDED","playlistEditResults":[{"playlistEditVideoAddedResultData":{"videoId":"4D7u5KF7SP8","setVideoId":"unique-1"}}]}),true).unwrap();
        assert_eq!(ok.status, "succeeded");
        assert_eq!(ok.added_items[0].set_video_id, "unique-1");
        assert_eq!(
            response(json!({"responseContext":{}}), false)
                .unwrap()
                .status,
            "accepted"
        );
        // A successful creation can include menus with future confirmation
        // dialogs. They are not evidence that this request was gated.
        assert!(response(json!({"responseContext":{},"playlistId":"PLfixture","actions":[{"navigateAction":{"menu":{"confirmDialogRenderer":{}}}}]}),false).is_ok());
    }
}
