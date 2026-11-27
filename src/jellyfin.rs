//! Jellyfin API client (blocking requests, to call off the interface thread).

use std::collections::HashMap;
use std::fmt;
use std::io::Read;
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer};
use serde_json::{json, Value};

use crate::config::Config;
use crate::i18n::t;

const ITEM_QUERY: [(&str, &str); 3] =
    [("Fields", "Overview"), ("ImageTypeLimit", "1"), ("EnableImageTypes", "Primary,Backdrop,Logo")];
const TEXT_SUBTITLES: [(&str, &str); 6] =
    [("subrip", "srt"), ("srt", "srt"), ("ass", "ass"), ("ssa", "ssa"), ("webvtt", "vtt"), ("vtt", "vtt")];

#[derive(Debug)]
pub struct Error {
    message: String,
}

impl Error {
    pub fn timeout() -> Self {
        Self { message: t("timed out", "délai dépassé").into() }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl From<ureq::Error> for Error {
    fn from(error: ureq::Error) -> Self {
        match error {
            ureq::Error::Status(401, _) => {
                Self { message: t("access denied by the server", "accès refusé par le serveur").into() }
            }
            ureq::Error::Status(code, _) => {
                Self { message: format!("{} {code}", t("the server answered", "le serveur a répondu")) }
            }
            ureq::Error::Transport(transport) => {
                Self { message: format!("{} ({})", t("server unreachable", "serveur injoignable"), transport.kind()) }
            }
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Self { message: format!("{} ({error})", t("unreadable answer", "réponse illisible")) }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// Jellyfin sometimes sends `null` where a value is expected: use the default then.
fn or_default<'de, D: Deserializer<'de>, T: Default + Deserialize<'de>>(
    deserializer: D,
) -> std::result::Result<T, D::Error> {
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct UserData {
    #[serde(deserialize_with = "or_default")]
    pub played: bool,
    #[serde(deserialize_with = "or_default")]
    pub playback_position_ticks: i64,
    pub unplayed_item_count: Option<i64>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct MediaStream {
    #[serde(rename = "Type", deserialize_with = "or_default")]
    pub kind: String,
    pub codec: Option<String>,
    #[serde(deserialize_with = "or_default")]
    pub index: i64,
    #[serde(deserialize_with = "or_default")]
    pub is_external: bool,
    pub language: Option<String>,
    pub display_title: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct MediaSource {
    #[serde(deserialize_with = "or_default")]
    pub id: String,
    pub path: Option<String>,
    pub protocol: Option<String>,
    pub transcoding_url: Option<String>,
    pub live_stream_id: Option<String>,
    #[serde(deserialize_with = "or_default")]
    pub required_http_headers: HashMap<String, String>,
    #[serde(deserialize_with = "or_default")]
    pub media_streams: Vec<MediaStream>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct Program {
    #[serde(deserialize_with = "or_default")]
    pub name: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct Item {
    #[serde(deserialize_with = "or_default")]
    pub id: String,
    #[serde(deserialize_with = "or_default")]
    pub name: String,
    #[serde(rename = "Type", deserialize_with = "or_default")]
    pub kind: String,
    pub collection_type: Option<String>,
    #[serde(deserialize_with = "or_default")]
    pub is_folder: bool,
    pub media_type: Option<String>,
    pub overview: Option<String>,
    pub production_year: Option<i64>,
    pub run_time_ticks: Option<i64>,
    pub index_number: Option<i64>,
    pub parent_index_number: Option<i64>,
    pub series_name: Option<String>,
    pub series_id: Option<String>,
    pub series_primary_image_tag: Option<String>,
    pub album_id: Option<String>,
    pub album_primary_image_tag: Option<String>,
    pub parent_backdrop_item_id: Option<String>,
    pub parent_logo_item_id: Option<String>,
    pub parent_logo_image_tag: Option<String>,
    pub channel_number: Option<String>,
    pub current_program: Option<Program>,
    #[serde(deserialize_with = "or_default")]
    pub image_tags: HashMap<String, String>,
    #[serde(deserialize_with = "or_default")]
    pub backdrop_image_tags: Vec<String>,
    #[serde(deserialize_with = "or_default")]
    pub user_data: UserData,
    #[serde(deserialize_with = "or_default")]
    pub media_sources: Vec<MediaSource>,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
struct ItemList {
    #[serde(deserialize_with = "or_default")]
    items: Vec<Item>,
}

/// Segment found in an episode (intro, recap, end credits).
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct MediaSegment {
    #[serde(rename = "Type", deserialize_with = "or_default")]
    pub kind: String,
    #[serde(deserialize_with = "or_default")]
    pub start_ticks: i64,
    #[serde(deserialize_with = "or_default")]
    pub end_ticks: i64,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
struct SegmentList {
    #[serde(deserialize_with = "or_default")]
    items: Vec<MediaSegment>,
}

pub fn ticks_to_seconds(ticks: i64) -> f64 {
    ticks as f64 / 1e7
}

impl Item {
    pub fn is_channel(&self) -> bool {
        self.kind == "TvChannel"
    }

    pub fn is_playable(&self) -> bool {
        !self.is_folder && matches!(self.media_type.as_deref(), Some("Video" | "Audio"))
    }

    pub fn is_audio(&self) -> bool {
        self.media_type.as_deref() == Some("Audio")
    }

    pub fn runtime_seconds(&self) -> f64 {
        ticks_to_seconds(self.run_time_ticks.unwrap_or(0))
    }

    /// Resume position in seconds, or 0 if there is nothing to resume.
    pub fn resume_seconds(&self) -> f64 {
        if self.user_data.played {
            return 0.0;
        }
        ticks_to_seconds(self.user_data.playback_position_ticks)
    }

    pub fn episode_code(&self) -> Option<String> {
        Some(format!("S{} E{}", self.parent_index_number?, self.index_number?))
    }

    /// Full title, for the player and the remote.
    pub fn play_title(&self) -> String {
        match (&self.series_name, self.kind.as_str()) {
            (Some(series), "Episode") => match self.episode_code() {
                Some(code) => format!("{series} · {code} · {}", self.name),
                None => format!("{series} · {}", self.name),
            },
            _ => self.name.clone(),
        }
    }
}

/// External subtitle to load in the player.
pub struct Subtitle {
    /// Jellyfin stream index.
    pub index: i64,
    pub url: String,
    pub title: String,
    pub language: String,
}

pub struct QuickConnect {
    pub secret: String,
    pub code: String,
}

#[derive(Clone)]
pub struct Client {
    agent: ureq::Agent,
    pub server: String,
    pub token: String,
    pub user_id: String,
    device_id: String,
}

impl Client {
    pub fn new(server: &str, device_id: &str) -> Self {
        Self {
            agent: ureq::AgentBuilder::new().timeout(Duration::from_secs(20)).user_agent(crate::USER_AGENT).build(),
            server: server.trim_end_matches('/').to_owned(),
            token: String::new(),
            user_id: String::new(),
            device_id: device_id.to_owned(),
        }
    }

    pub fn from_config(config: &Config) -> Self {
        Self {
            token: config.token.clone(),
            user_id: config.user_id.clone(),
            ..Self::new(&config.server, &config.device_id)
        }
    }

    /// Finds the server from a typed address, with or without `http(s)://`.
    pub fn discover(address: &str, device_id: &str) -> Result<Self> {
        let address = address.trim();
        let candidates = if address.contains("://") {
            vec![address.to_owned()]
        } else {
            vec![format!("https://{address}"), format!("http://{address}")]
        };
        let mut last = None;
        for candidate in candidates {
            let client = Self::new(&candidate, device_id);
            match client.get::<Value>("/System/Info/Public", &[]) {
                Ok(_) => return Ok(client),
                Err(error) => last = Some(error),
            }
        }
        Err(last.expect("at least one address tried"))
    }

    fn authorization(&self) -> String {
        let mut header = format!(
            "MediaBrowser Client=\"jf\", Device=\"{}\", DeviceId=\"{}\", Version=\"{}\"",
            hostname(),
            self.device_id,
            env!("CARGO_PKG_VERSION"),
        );
        if !self.token.is_empty() {
            header.push_str(&format!(", Token=\"{}\"", self.token));
        }
        header
    }

    /// Header to give mpv to play a stream from the server.
    pub fn stream_header(&self) -> String {
        format!("Authorization: MediaBrowser Token=\"{}\"", self.token)
    }

    fn request(&self, method: &str, path: &str, query: &[(&str, &str)]) -> ureq::Request {
        let mut request =
            self.agent.request(method, &format!("{}{path}", self.server)).set("Authorization", &self.authorization());
        for (name, value) in query {
            request = request.query(name, value);
        }
        request
    }

    fn get<T: DeserializeOwned>(&self, path: &str, query: &[(&str, &str)]) -> Result<T> {
        Ok(self.request("GET", path, query).call()?.into_json()?)
    }

    fn send(&self, method: &str, path: &str, query: &[(&str, &str)], body: Option<Value>) -> Result<Value> {
        let request = self.request(method, path, query);
        let response = match body {
            Some(body) => request.send_json(body)?,
            None => request.call()?,
        };
        let text = response.into_string()?;
        Ok(serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    fn items(&self, path: &str, query: &[(&str, &str)]) -> Result<Vec<Item>> {
        let mut full = vec![("userId", self.user_id.as_str())];
        full.extend_from_slice(&ITEM_QUERY);
        // A repeated parameter replaces the default value.
        full.retain(|(name, _)| !query.iter().any(|(given, _)| given == name));
        full.extend_from_slice(query);
        Ok(self.get::<ItemList>(path, &full)?.items)
    }

    // ---------------------------------------------------------------- sign-in

    fn accept_session(&mut self, session: &Value) -> Result<()> {
        let token = session["AccessToken"].as_str();
        let user = session["User"]["Id"].as_str();
        match (token, user) {
            (Some(token), Some(user)) => {
                self.token = token.to_owned();
                self.user_id = user.to_owned();
                Ok(())
            }
            _ => Err(Error { message: t("unexpected sign-in answer", "réponse de connexion inattendue").into() }),
        }
    }

    pub fn login(&mut self, username: &str, password: &str) -> Result<()> {
        let body = json!({ "Username": username, "Pw": password });
        let session = self.send("POST", "/Users/AuthenticateByName", &[], Some(body))?;
        self.accept_session(&session)
    }

    pub fn quick_connect_start(&self) -> Result<QuickConnect> {
        let reply = self.send("POST", "/QuickConnect/Initiate", &[], None)?;
        match (reply["Secret"].as_str(), reply["Code"].as_str()) {
            (Some(secret), Some(code)) => Ok(QuickConnect { secret: secret.into(), code: code.into() }),
            _ => Err(Error { message: "Quick Connect indisponible".into() }),
        }
    }

    /// `Ok(false)` until the code is approved on another device.
    pub fn quick_connect_finish(&mut self, secret: &str) -> Result<bool> {
        let state: Value = self.get("/QuickConnect/Connect", &[("secret", secret)])?;
        if state["Authenticated"].as_bool() != Some(true) {
            return Ok(false);
        }
        let session =
            self.send("POST", "/Users/AuthenticateWithQuickConnect", &[], Some(json!({ "Secret": secret })))?;
        self.accept_session(&session)?;
        Ok(true)
    }

    // ---------------------------------------------------------------- library

    pub fn views(&self) -> Result<Vec<Item>> {
        Ok(self.get::<ItemList>("/UserViews", &[("userId", &self.user_id)])?.items)
    }

    pub fn resume(&self) -> Result<Vec<Item>> {
        self.items("/UserItems/Resume", &[("MediaTypes", "Video"), ("Limit", "50")])
    }

    pub fn next_up(&self) -> Result<Vec<Item>> {
        self.items("/Shows/NextUp", &[("Limit", "50")])
    }

    pub fn search(&self, term: &str) -> Result<Vec<Item>> {
        self.items(
            "/Items",
            &[
                ("SearchTerm", term),
                ("Recursive", "true"),
                ("Limit", "100"),
                ("IncludeItemTypes", "Movie,Series,Episode,BoxSet,MusicAlbum,Audio,Video,MusicVideo"),
            ],
        )
    }

    pub fn channels(&self) -> Result<Vec<Item>> {
        self.items("/LiveTv/Channels", &[("AddCurrentProgram", "true"), ("EnableImageTypes", "Primary")])
    }

    pub fn children(&self, parent: &Item) -> Result<Vec<Item>> {
        let id = parent.id.as_str();
        let collection = parent.collection_type.as_deref().unwrap_or("");
        match (parent.kind.as_str(), collection, &parent.series_id) {
            ("Series", _, _) => self.items(&format!("/Shows/{id}/Seasons"), &[]),
            ("Season", _, Some(series)) => self.items(&format!("/Shows/{series}/Episodes"), &[("seasonId", id)]),
            (_, "livetv", _) => self.channels(),
            ("CollectionFolder" | "UserView", library, _) => {
                let kind = match library {
                    "movies" => "Movie",
                    "tvshows" => "Series",
                    "music" => "MusicAlbum",
                    "boxsets" => "BoxSet",
                    "musicvideos" => "MusicVideo",
                    _ => return self.items("/Items", &[("ParentId", id), ("SortBy", "SortName")]),
                };
                self.items(
                    "/Items",
                    &[("ParentId", id), ("Recursive", "true"), ("IncludeItemTypes", kind), ("SortBy", "SortName")],
                )
            }
            ("MusicAlbum", _, _) => {
                self.items("/Items", &[("ParentId", id), ("SortBy", "ParentIndexNumber,IndexNumber,SortName")])
            }
            ("Playlist", _, _) => self.items("/Items", &[("ParentId", id)]),
            _ => self.items("/Items", &[("ParentId", id), ("SortBy", "SortName")]),
        }
    }

    /// Everything playable under an item (series, season, collection…), in random order.
    pub fn shuffled(&self, parent: &Item) -> Result<Vec<Item>> {
        self.items(
            "/Items",
            &[
                ("ParentId", &parent.id),
                ("Recursive", "true"),
                ("IncludeItemTypes", "Episode,Movie,Video,MusicVideo,Audio"),
                ("SortBy", "Random"),
                ("Limit", "300"),
            ],
        )
    }

    /// The given episode and the ones after it in the series.
    pub fn episodes_from(&self, series_id: &str, episode_id: &str) -> Result<Vec<Item>> {
        self.items(&format!("/Shows/{series_id}/Episodes"), &[("startItemId", episode_id), ("Limit", "50")])
    }

    /// Segments found by the server (Jellyfin 10.10 and later, with a plugin such as Intro Skipper).
    pub fn segments(&self, id: &str) -> Result<Vec<MediaSegment>> {
        Ok(self.get::<SegmentList>(&format!("/MediaSegments/{id}"), &[])?.items)
    }

    pub fn item_with_sources(&self, id: &str) -> Result<Item> {
        self.items("/Items", &[("Ids", id), ("Fields", "Overview,MediaSources")])?
            .into_iter()
            .next()
            .ok_or(Error { message: t("item not found", "élément introuvable").into() })
    }

    pub fn live_sources(&self, channel_id: &str) -> Result<Vec<MediaSource>> {
        #[derive(Default, Deserialize)]
        #[serde(rename_all = "PascalCase", default)]
        struct PlaybackInfo {
            #[serde(deserialize_with = "or_default")]
            media_sources: Vec<MediaSource>,
        }
        let body = json!({ "UserId": self.user_id, "AutoOpenLiveStream": false });
        let reply = self
            .request("POST", &format!("/Items/{channel_id}/PlaybackInfo"), &[("userId", &self.user_id)])
            .send_json(body)?;
        Ok(reply.into_json::<PlaybackInfo>()?.media_sources)
    }

    pub fn external_subtitles(&self, item_id: &str, source: &MediaSource) -> Vec<Subtitle> {
        let mut subtitles = Vec::new();
        for stream in &source.media_streams {
            let codec = stream.codec.as_deref().unwrap_or("").to_lowercase();
            let extension = TEXT_SUBTITLES.iter().find(|(name, _)| *name == codec).map(|(_, ext)| ext);
            if let (true, true, Some(extension)) = (stream.kind == "Subtitle", stream.is_external, extension) {
                subtitles.push(Subtitle {
                    index: stream.index,
                    url: format!(
                        "{}/Videos/{item_id}/{}/Subtitles/{}/Stream.{extension}",
                        self.server, source.id, stream.index
                    ),
                    title: stream.display_title.clone().unwrap_or_default(),
                    language: stream.language.clone().unwrap_or_default(),
                });
            }
        }
        subtitles
    }

    pub fn set_played(&self, id: &str, played: bool) -> Result<()> {
        let method = if played { "POST" } else { "DELETE" };
        self.send(method, &format!("/UserPlayedItems/{id}"), &[("userId", &self.user_id)], None)?;
        Ok(())
    }

    /// `event`: "" at start, "/Progress" during playback, "/Stopped" at the end.
    pub fn report(&self, event: &str, body: Value) -> Result<()> {
        self.send("POST", &format!("/Sessions/Playing{event}"), &[], Some(body))?;
        Ok(())
    }

    /// Items by id, in the given order (the server returns them sorted its own way).
    pub fn items_by_ids(&self, ids: &[String]) -> Result<Vec<Item>> {
        let mut items = self.items("/Items", &[("Ids", &ids.join(","))])?;
        items.sort_by_key(|item| ids.iter().position(|id| *id == item.id));
        Ok(items)
    }

    // ---------------------------------------------------------------- remote control

    /// Declares jf as a device other Jellyfin apps can play on and control.
    pub fn send_capabilities(&self, commands: &[&str]) -> Result<()> {
        let body = json!({
            "PlayableMediaTypes": ["Video", "Audio"],
            "SupportedCommands": commands,
            "SupportsMediaControl": true,
            "SupportsPersistentIdentifier": true,
        });
        self.send("POST", "/Sessions/Capabilities/Full", &[], Some(body))?;
        Ok(())
    }

    /// Address and authorization header of the server's WebSocket. The token also goes
    /// in the address, as jellyfin-web does, for servers that only read it there.
    pub fn socket(&self) -> (String, String) {
        let address = match self.server.split_once("://") {
            Some(("https", rest)) => format!("wss://{rest}"),
            Some((_, rest)) => format!("ws://{rest}"),
            None => format!("ws://{}", self.server),
        };
        let address = format!("{address}/socket?api_key={}&deviceId={}", self.token, self.device_id);
        (address, self.authorization())
    }

    pub fn image(&self, id: &str, kind: &str, width: u32) -> Result<Vec<u8>> {
        let response = self
            .request(
                "GET",
                &format!("/Items/{id}/Images/{kind}"),
                &[("maxWidth", &width.to_string()), ("quality", "85")],
            )
            .call()?;
        let mut bytes = Vec::new();
        response.into_reader().take(16 << 20).read_to_end(&mut bytes)?;
        Ok(bytes)
    }
}

fn hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|name| name.trim().to_owned())
        .unwrap_or_else(|_| "linux".into())
}
