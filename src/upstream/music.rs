//! Adapt RustyPipe's typed Music reads to the stable desktop JSON models.
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rustypipe::{
    model::{self as upstream, paginator::Paginator, MusicItem, TrackType},
    param::search_filter::MusicSearchFilter,
};
use serde::{Deserialize, Serialize};

use crate::{
    discovery::SearchSuggestion,
    library::LibrarySection,
    model::{Item, ItemActions, Link, Lyrics, Page, SearchFilter, Section, Thumbnail},
    Error, MusicClient, Result,
};

const CURSOR_PREFIX: &str = "rustypipe:";
const CURSOR_VERSION: u8 = 1;
const MAX_CURSOR_LENGTH: usize = 1024 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    version: u8,
    session_binding: String,
    paginator: Paginator<MusicItem>,
    title: String,
    playlist_id: Option<String>,
    library: Option<LibrarySection>,
}

impl Cursor {
    fn decode(token: &str) -> Result<Option<Self>> {
        let Some(encoded) = token.strip_prefix(CURSOR_PREFIX) else {
            return Ok(None);
        };
        if encoded.len() > MAX_CURSOR_LENGTH {
            return Err(Error::InvalidInput("continuation is too large".into()));
        }
        let invalid = || Error::InvalidInput("invalid RustyPipe continuation".into());
        let bytes = URL_SAFE_NO_PAD.decode(encoded).map_err(|_| invalid())?;
        let cursor: Self = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
        if cursor.version != CURSOR_VERSION
            || !cursor.paginator.items.is_empty()
            || cursor
                .paginator
                .ctoken
                .as_ref()
                .is_none_or(String::is_empty)
            || !matches!(
                cursor.paginator.endpoint,
                upstream::paginator::ContinuationEndpoint::MusicBrowse
                    | upstream::paginator::ContinuationEndpoint::MusicSearch
                    | upstream::paginator::ContinuationEndpoint::MusicNext
            )
        {
            return Err(invalid());
        }
        Ok(Some(cursor))
    }

    fn encode(&self) -> Result<String> {
        let json = serde_json::to_vec(self)
            .map_err(|_| Error::Protocol("could not encode music continuation".into()))?;
        Ok(format!("{CURSOR_PREFIX}{}", URL_SAFE_NO_PAD.encode(json)))
    }
}

impl MusicClient {
    pub(crate) fn upstream_search(&self, query: &str, filter: SearchFilter) -> Result<Page> {
        let binding = self.upstream.current_binding(self)?;
        let upstream = self.upstream.query(self)?;
        if matches!(filter, SearchFilter::Playlists) {
            // Upstream distinguishes editorial and community playlists. Keep both
            // independent continuations instead of silently dropping either kind.
            let (editorial, community) = super::run(async {
                tokio::try_join!(
                    upstream.music_search_playlists(query, false),
                    upstream.music_search_playlists(query, true)
                )
            })?;
            let mut sections = search_sections(editorial.sections, &binding)?;
            sections.extend(search_sections(community.sections, &binding)?);
            return Ok(Page {
                title: None,
                sections,
                playlist_id: None,
                actions: ItemActions::default(),
            });
        }
        let filter = match filter {
            SearchFilter::All => None,
            SearchFilter::Songs => Some(MusicSearchFilter::Tracks),
            SearchFilter::Videos => Some(MusicSearchFilter::Videos),
            SearchFilter::Albums => Some(MusicSearchFilter::Albums),
            SearchFilter::Artists => Some(MusicSearchFilter::Artists),
            SearchFilter::Playlists => unreachable!(),
        };
        let result = super::run(upstream.music_search::<MusicItem, _>(query, filter))?;
        Ok(Page {
            title: None,
            sections: search_sections(result.sections, &binding)?,
            playlist_id: None,
            actions: ItemActions::default(),
        })
    }

    pub(crate) fn upstream_browse(&self, browse_id: &str) -> Result<Option<Page>> {
        let binding = self.upstream.current_binding(self)?;
        if let Some(id) = browse_id.strip_prefix("VL") {
            let upstream = self.upstream.query(self)?;
            let playlist = super::run(upstream.music_playlist(id))?;
            return playlist_page(playlist, None, &binding).map(Some);
        }
        if browse_id.starts_with("MPRE") {
            let upstream = self.upstream.query(self)?;
            let album = super::run(upstream.music_album(browse_id))?;
            let mut tracks = Paginator::default();
            tracks.items = album.tracks;
            let mut result = page(tracks, Some(album.name), album.playlist_id, None, &binding)?;
            result.actions = map_actions(album.metadata);
            return Ok(Some(result));
        }
        if browse_id.starts_with("UC") {
            let upstream = self.upstream.query(self)?;
            let artist = super::run(upstream.music_artist(browse_id, false))?;
            let sections = artist_sections(artist.sections, &binding)?;
            return Ok(Some(Page {
                title: Some(artist.name),
                sections,
                playlist_id: None,
                actions: map_actions(artist.metadata),
            }));
        }
        Ok(None)
    }

    pub(crate) fn upstream_continue(
        &self,
        token: &str,
        expected: crate::client::ContinuationEndpoint,
    ) -> Result<Option<Page>> {
        let Some(cursor) = Cursor::decode(token)? else {
            return Ok(None);
        };
        let matches = matches!(
            (expected, cursor.paginator.endpoint),
            (
                crate::client::ContinuationEndpoint::Browse,
                upstream::paginator::ContinuationEndpoint::MusicBrowse
            ) | (
                crate::client::ContinuationEndpoint::Search,
                upstream::paginator::ContinuationEndpoint::MusicSearch
            ) | (
                crate::client::ContinuationEndpoint::Next,
                upstream::paginator::ContinuationEndpoint::MusicNext
            )
        );
        if !matches {
            return Err(Error::InvalidInput(
                "continuation belongs to a different endpoint".into(),
            ));
        }
        self.upstream_continue_cursor(cursor).map(Some)
    }

    fn upstream_continue_cursor(&self, cursor: Cursor) -> Result<Page> {
        let binding = self.upstream.current_binding(self)?;
        if cursor.session_binding != binding {
            return Err(Error::InvalidInput(
                "continuation belongs to a different session".into(),
            ));
        }
        let upstream = self.upstream.query(self)?;
        let mut next = super::run(cursor.paginator.next(&upstream))?
            .ok_or_else(|| Error::InvalidInput("music continuation is exhausted".into()))?;
        // RustyPipe's continuation parser does not always retain these request
        // properties. They must remain sticky across every page of a library.
        next.authenticated |= cursor.paginator.authenticated;
        if next.visitor_data.is_none() {
            next.visitor_data = cursor.paginator.visitor_data;
        }
        Ok(Page {
            title: None,
            sections: vec![section(
                next,
                &cursor.title,
                cursor.playlist_id.clone(),
                cursor.library,
                &binding,
            )?],
            playlist_id: cursor.playlist_id,
            actions: ItemActions::default(),
        })
    }

    pub(crate) fn upstream_queue(&self, video_id: &str) -> Result<Page> {
        let binding = self.upstream.current_binding(self)?;
        let upstream = self.upstream.query(self)?;
        let tracks = super::run(upstream.music_radio_track(video_id))?;
        page(tracks, None, None, None, &binding)
    }

    pub(crate) fn upstream_lyrics(&self, video_id: &str) -> Result<Lyrics> {
        let upstream = self.upstream.query(self)?;
        let details = super::run(upstream.music_details(video_id))?;
        let id = details.lyrics_id.ok_or(Error::LyricsUnavailable)?;
        let lyrics = super::run(upstream.music_lyrics(&id))?;
        Ok(Lyrics {
            browse_id: id,
            text: lyrics.body,
            source: (!lyrics.footer.is_empty()).then_some(lyrics.footer),
        })
    }

    pub(crate) fn upstream_library(
        &self,
        library: LibrarySection,
        continuation: Option<&str>,
    ) -> Result<Option<Page>> {
        let binding = self.upstream.current_binding(self)?;
        if let Some(token) = continuation {
            let Some(cursor) = Cursor::decode(token)? else {
                return Ok(None);
            };
            if cursor.library != Some(library) {
                return Err(Error::InvalidInput(
                    "continuation belongs to a different library section".into(),
                ));
            }
            return self.upstream_continue_cursor(cursor).map(Some);
        }
        if matches!(library, LibrarySection::Artists) {
            // Library artists differ from subscribed artists. Upstream currently
            // implements only the latter; keep our small Web extension for this.
            return Ok(None);
        }
        let upstream = self.upstream.query(self)?;
        let result = match library {
            LibrarySection::Playlists => page(
                super::run(upstream.music_saved_playlists())?,
                None,
                None,
                Some(library),
                &binding,
            ),
            LibrarySection::Songs => page(
                super::run(upstream.music_saved_tracks())?,
                None,
                None,
                Some(library),
                &binding,
            ),
            LibrarySection::Albums => page(
                super::run(upstream.music_saved_albums())?,
                None,
                None,
                Some(library),
                &binding,
            ),
            LibrarySection::Subscriptions => page(
                super::run(upstream.music_saved_artists())?,
                None,
                None,
                Some(library),
                &binding,
            ),
            LibrarySection::Likes => playlist_page(
                super::run(upstream.music_liked_tracks())?,
                Some(library),
                &binding,
            ),
            LibrarySection::Artists => unreachable!(),
        };
        result.map(Some)
    }

    pub(crate) fn upstream_suggestions(&self, query: &str) -> Result<Vec<SearchSuggestion>> {
        let upstream = self.upstream.query(self)?;
        let suggestions = super::run(upstream.music_search_suggestion(query))?;
        Ok(suggestions
            .terms
            .into_iter()
            .enumerate()
            .map(|(index, query)| SearchSuggestion {
                query,
                from_history: suggestions
                    .from_history
                    .get(index)
                    .copied()
                    .unwrap_or(false),
            })
            .collect())
    }
}

fn playlist_page(
    playlist: upstream::MusicPlaylist,
    library: Option<LibrarySection>,
    binding: &str,
) -> Result<Page> {
    let mut result = page(
        playlist.tracks,
        Some(playlist.name),
        Some(playlist.id),
        library,
        binding,
    )?;
    result.actions = map_actions(playlist.metadata);
    Ok(result)
}

fn page<T: Into<MusicItem>>(
    paginator: Paginator<T>,
    title: Option<String>,
    playlist_id: Option<String>,
    library: Option<LibrarySection>,
    binding: &str,
) -> Result<Page> {
    Ok(Page {
        title,
        sections: vec![section(
            paginator,
            "",
            playlist_id.clone(),
            library,
            binding,
        )?],
        playlist_id,
        actions: ItemActions::default(),
    })
}

fn section<T: Into<MusicItem>>(
    paginator: Paginator<T>,
    title: &str,
    playlist_id: Option<String>,
    library: Option<LibrarySection>,
    binding: &str,
) -> Result<Section> {
    let mut context = Paginator::default();
    context.count = paginator.count;
    context.ctoken = paginator.ctoken;
    context.visitor_data = paginator.visitor_data;
    context.endpoint = paginator.endpoint;
    context.authenticated = paginator.authenticated || library.is_some();
    let cursor = Cursor {
        version: CURSOR_VERSION,
        session_binding: binding.to_owned(),
        paginator: context,
        title: title.to_owned(),
        playlist_id: playlist_id.clone(),
        library,
    };
    let continuation = cursor
        .paginator
        .ctoken
        .as_ref()
        .map(|_| cursor.encode())
        .transpose()?;
    let items = paginator
        .items
        .into_iter()
        .map(|value| {
            let mut item = map_item(value.into());
            if item.video_id.is_some() {
                item.playlist_id = playlist_id.clone();
            }
            item
        })
        .collect();
    Ok(Section {
        title: title.into(),
        items,
        continuation,
    })
}

fn search_sections(
    sections: Vec<upstream::MusicSearchSection>,
    binding: &str,
) -> Result<Vec<Section>> {
    sections
        .into_iter()
        .map(|shelf| section(shelf.items, &shelf.title, None, None, binding))
        .collect()
}

fn artist_sections(
    sections: Vec<upstream::MusicArtistSection>,
    binding: &str,
) -> Result<Vec<Section>> {
    sections
        .into_iter()
        .map(|shelf| {
            // A browse destination is deliberately not treated as an API continuation.
            section(shelf.items, &shelf.title, None, None, binding)
        })
        .collect()
}

fn thumbnails(thumbnails: Vec<upstream::Thumbnail>) -> Vec<Thumbnail> {
    thumbnails
        .into_iter()
        .map(|thumbnail| Thumbnail {
            url: thumbnail.url,
            width: (thumbnail.width != 0).then_some(u64::from(thumbnail.width)),
            height: (thumbnail.height != 0).then_some(u64::from(thumbnail.height)),
        })
        .collect()
}

fn artists(artists: Vec<upstream::ArtistId>) -> Vec<Link> {
    artists
        .into_iter()
        .map(|artist| Link {
            name: artist.name,
            browse_id: artist.id.unwrap_or_default(),
        })
        .collect()
}

fn map_item(value: MusicItem) -> Item {
    let metadata = match &value {
        MusicItem::Track(value) => value.metadata.clone(),
        MusicItem::Album(value) => value.metadata.clone(),
        MusicItem::Artist(value) => value.metadata.clone(),
        MusicItem::Playlist(value) => value.metadata.clone(),
        MusicItem::User(_) => upstream::MusicMetadata::default(),
    };
    let mut item = Item {
        title: String::new(),
        kind: String::new(),
        video_id: None,
        browse_id: None,
        artists: Vec::new(),
        album: None,
        duration_seconds: None,
        thumbnails: Vec::new(),
        explicit: metadata.explicit,
        available: metadata.available,
        playlist_id: metadata.playlist_id.clone(),
        set_video_id: metadata.set_video_id.clone(),
        actions: map_actions(metadata),
    };
    match value {
        MusicItem::Track(track) => {
            item.title = track.name;
            item.kind = match track.track_type {
                TrackType::Track => "song",
                TrackType::Video | TrackType::Episode => "video",
            }
            .into();
            item.video_id = Some(track.id);
            item.artists = artists(track.artists);
            item.album = track.album.map(|album| Link {
                name: album.name,
                browse_id: album.id,
            });
            item.duration_seconds = track.duration.map(u64::from);
            item.thumbnails = thumbnails(track.cover);
            // An absent availability marker is not a positive guarantee.
            item.available = item
                .available
                .or_else(|| track.unavailable.then_some(false));
        }
        MusicItem::Album(album) => {
            item.title = album.name;
            item.kind = "album".into();
            item.browse_id = Some(album.id);
            item.artists = artists(album.artists);
            item.thumbnails = thumbnails(album.cover);
        }
        MusicItem::Artist(artist) => {
            item.title = artist.name;
            item.kind = "artist".into();
            item.browse_id = Some(artist.id);
            item.thumbnails = thumbnails(artist.avatar);
        }
        MusicItem::Playlist(playlist) => {
            item.title = playlist.name;
            item.kind = "playlist".into();
            item.browse_id = Some(format!("VL{}", playlist.id));
            item.playlist_id = Some(playlist.id);
            item.thumbnails = thumbnails(playlist.thumbnail);
        }
        MusicItem::User(user) => {
            item.title = user.name;
            item.kind = "artist".into();
            item.browse_id = Some(user.id);
            item.thumbnails = thumbnails(user.avatar);
        }
    }
    item
}

fn map_actions(metadata: upstream::MusicMetadata) -> ItemActions {
    ItemActions {
        rating: metadata.rating.as_deref().and_then(|status| match status {
            "LIKE" => Some(crate::mutations::Rating::Like),
            "DISLIKE" => Some(crate::mutations::Rating::Dislike),
            "INDIFFERENT" => Some(crate::mutations::Rating::Indifferent),
            _ => None,
        }),
        in_library: metadata.in_library,
        subscribed: metadata.subscribed,
        can_edit: metadata.can_edit,
        add_library_token: metadata.add_library_token,
        remove_library_token: metadata.remove_library_token,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn track(unavailable: bool) -> upstream::TrackItem {
        serde_json::from_value(json!({
            "id":"video000001", "name":"A track", "duration":123, "cover":[],
            "artists":[{"id":null,"name":"Unlinked artist"}], "artist_id":null,
            "album":null, "view_count":null, "track_type":"track", "track_nr":null,
            "by_va":false, "unavailable":unavailable
        }))
        .unwrap()
    }

    #[test]
    fn unavailable_tracks_and_unknown_actions_remain_distinct() {
        let item = map_item(MusicItem::Track(track(true)));
        assert_eq!(item.available, Some(false));
        assert_eq!(item.kind, "song");
        assert_eq!(item.artists[0].name, "Unlinked artist");
        assert!(item.artists[0].browse_id.is_empty());
        assert!(item.set_video_id.is_none());
        assert!(item.actions.rating.is_none());
        assert!(item.actions.in_library.is_none());
        assert!(map_item(MusicItem::Track(track(false))).available.is_none());
    }

    #[test]
    fn mapped_metadata_keeps_duplicate_ids_and_actions_without_inference() {
        let mut track = track(false);
        track.metadata.set_video_id = Some("second-entry".into());
        track.metadata.playlist_id = Some("PLfixture".into());
        track.metadata.explicit = true;
        track.metadata.available = Some(true);
        track.metadata.rating = Some("LIKE".into());
        track.metadata.add_library_token = Some("fixture-add".into());
        let item = map_item(MusicItem::Track(track));
        assert_eq!(item.set_video_id.as_deref(), Some("second-entry"));
        assert_eq!(item.playlist_id.as_deref(), Some("PLfixture"));
        assert_eq!(item.available, Some(true));
        assert!(item.explicit);
        assert!(matches!(
            item.actions.rating,
            Some(crate::mutations::Rating::Like)
        ));
        assert_eq!(
            item.actions.add_library_token.as_deref(),
            Some("fixture-add")
        );
        assert!(item.actions.in_library.is_none());
    }

    #[test]
    fn search_shelves_keep_independent_search_continuations() {
        let shelves: Vec<upstream::MusicSearchSection> = serde_json::from_value(json!([
            {"title":"Carousel artists", "items":{
                "count":null,"items":[],"ctoken":"carousel-next","endpoint":"music_search"
            }},
            {"title":"Shelf artists", "items":{
                "count":null,"items":[],"ctoken":"shelf-next","endpoint":"music_search"
            }}
        ]))
        .unwrap();
        let mapped = search_sections(shelves, "test-binding").unwrap();
        assert_eq!(mapped.len(), 2);
        for (index, token) in ["carousel-next", "shelf-next"].iter().enumerate() {
            let cursor = Cursor::decode(mapped[index].continuation.as_deref().unwrap())
                .unwrap()
                .unwrap();
            assert_eq!(cursor.paginator.ctoken.as_deref(), Some(*token));
            assert_eq!(
                cursor.paginator.endpoint,
                upstream::paginator::ContinuationEndpoint::MusicSearch
            );
        }
    }

    #[test]
    fn artist_sections_keep_real_tokens_and_do_not_invent_more_tokens() {
        let shelves: Vec<upstream::MusicArtistSection> = serde_json::from_value(json!([
            {"title":"Localized songs", "items":{
                "count":null,"items":[],"ctoken":"real-next-page","endpoint":"music_browse"
            },"browse_id":"VLPLfixture","browse_params":"browse-params"},
            {"title":"Localized albums", "items":{
                "count":0,"items":[],"ctoken":null,"endpoint":"music_browse"
            },"browse_id":"MPADfixture","browse_params":"album-params"}
        ]))
        .unwrap();
        let mapped = artist_sections(shelves, "test-binding").unwrap();
        assert_eq!(mapped[0].title, "Localized songs");
        let cursor = Cursor::decode(mapped[0].continuation.as_deref().unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(cursor.paginator.ctoken.as_deref(), Some("real-next-page"));
        assert!(mapped[1].continuation.is_none());
    }

    #[test]
    fn cursor_rejects_wrong_endpoint_and_changed_session_before_network() {
        let client = MusicClient::new(crate::Config {
            client_version: Some("fixture".into()),
            ..Default::default()
        })
        .unwrap();
        let mut paginator = Paginator::default();
        paginator.ctoken = Some("next-page".into());
        paginator.endpoint = upstream::paginator::ContinuationEndpoint::MusicSearch;
        let cursor = Cursor {
            version: CURSOR_VERSION,
            session_binding: "different-session".into(),
            paginator,
            title: String::new(),
            playlist_id: None,
            library: None,
        };
        let token = cursor.encode().unwrap();
        assert!(matches!(
            client.upstream_continue(&token, crate::client::ContinuationEndpoint::Browse),
            Err(Error::InvalidInput(_))
        ));
        assert!(matches!(
            client.upstream_continue(&token, crate::client::ContinuationEndpoint::Search),
            Err(Error::InvalidInput(_))
        ));
        assert!(matches!(
            client.upstream_library(LibrarySection::Likes, Some(&token)),
            Err(Error::InvalidInput(_))
        ));
    }

    #[test]
    fn cursor_preserves_endpoint_authentication_and_playlist_without_old_items() {
        let mut paginator = Paginator::default();
        paginator.items = vec![track(false), track(false)];
        paginator.ctoken = Some("next-page".into());
        paginator.visitor_data = Some("test-visitor".into());
        paginator.endpoint = upstream::paginator::ContinuationEndpoint::MusicBrowse;
        let section = section(
            paginator,
            "",
            Some("LM".into()),
            Some(LibrarySection::Likes),
            "test-binding",
        )
        .unwrap();
        assert_eq!(section.items.len(), 2);
        assert_eq!(section.items[1].playlist_id.as_deref(), Some("LM"));
        let cursor = Cursor::decode(section.continuation.as_deref().unwrap())
            .unwrap()
            .unwrap();
        assert!(cursor.paginator.authenticated);
        assert!(cursor.paginator.items.is_empty());
        assert_eq!(
            cursor.paginator.visitor_data.as_deref(),
            Some("test-visitor")
        );
        assert_eq!(cursor.library, Some(LibrarySection::Likes));
        assert_eq!(cursor.paginator.ctoken.as_deref(), Some("next-page"));
    }

    #[test]
    fn cursor_rejects_corruption_version_drift_and_non_music_endpoints() {
        assert!(Cursor::decode("ordinary-youtube-token").unwrap().is_none());
        assert!(Cursor::decode("rustypipe:not-json").is_err());
        let mut paginator = Paginator::default();
        paginator.ctoken = Some("next-page".into());
        paginator.endpoint = upstream::paginator::ContinuationEndpoint::MusicSearch;
        let mut cursor = Cursor {
            version: 2,
            session_binding: "test-binding".into(),
            paginator,
            title: String::new(),
            playlist_id: None,
            library: None,
        };
        assert!(Cursor::decode(&cursor.encode().unwrap()).is_err());
        cursor.version = CURSOR_VERSION;
        cursor.paginator.endpoint = upstream::paginator::ContinuationEndpoint::Browse;
        assert!(Cursor::decode(&cursor.encode().unwrap()).is_err());
    }
}
