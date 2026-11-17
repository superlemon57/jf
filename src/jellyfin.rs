//! Client de l'API Jellyfin (requêtes bloquantes, à appeler hors du fil d'interface).

use std::collections::HashMap;
use std::fmt;
use std::io::Read;
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer};
use serde_json::{json, Value};

use crate::config::Config;

const ITEM_QUERY: [(&str, &str); 3] =
    [("Fields", "Overview"), ("ImageTypeLimit", "1"), ("EnableImageTypes", "Primary,Backdrop")];
const TEXT_SUBTITLES: [(&str, &str); 6] =
    [("subrip", "srt"), ("srt", "srt"), ("ass", "ass"), ("ssa", "ssa"), ("webvtt", "vtt"), ("vtt", "vtt")];

#[derive(Debug)]
pub struct Error {
    message: String,
}

impl Error {
    pub fn timeout() -> Self {
        Self { message: "délai dépassé".into() }
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
            ureq::Error::Status(401, _) => Self { message: "accès refusé par le serveur".into() },
            ureq::Error::Status(code, _) => Self { message: format!("le serveur a répondu {code}") },
            ureq::Error::Transport(transport) => {
                Self { message: format!("serveur injoignable ({})", transport.kind()) }
            }
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Self { message: format!("réponse illisible ({error})") }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// Jellyfin envoie parfois `null` là où on attend une valeur : on prend alors le défaut.
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

    /// Position de reprise en secondes, ou 0 s'il n'y a rien à reprendre.
    pub fn resume_seconds(&self) -> f64 {
        if self.user_data.played {
            return 0.0;
        }
        ticks_to_seconds(self.user_data.playback_position_ticks)
    }

    pub fn episode_code(&self) -> Option<String> {
        Some(format!("S{} E{}", self.parent_index_number?, self.index_number?))
    }

    /// Titre complet, pour le lecteur et la télécommande.
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

/// Sous-titre externe à charger dans le lecteur.
pub struct Subtitle {
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

    /// Trouve le serveur à partir d'une adresse saisie, avec ou sans `http(s)://`.
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
        Err(last.expect("au moins une adresse essayée"))
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

    /// En-tête à donner à mpv pour lire un flux du serveur.
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
        // Un paramètre répété remplace la valeur par défaut.
        full.retain(|(name, _)| !query.iter().any(|(given, _)| given == name));
        full.extend_from_slice(query);
        Ok(self.get::<ItemList>(path, &full)?.items)
    }

    // ---------------------------------------------------------------- connexion

    fn accept_session(&mut self, session: &Value) -> Result<()> {
        let token = session["AccessToken"].as_str();
        let user = session["User"]["Id"].as_str();
        match (token, user) {
            (Some(token), Some(user)) => {
                self.token = token.to_owned();
                self.user_id = user.to_owned();
                Ok(())
            }
            _ => Err(Error { message: "réponse de connexion inattendue".into() }),
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

    /// `Ok(false)` tant que le code n'a pas été validé sur un autre appareil.
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

    // ---------------------------------------------------------------- bibliothèque

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

    /// Tout ce qui est lisible sous un élément (série, saison, collection…), dans un ordre aléatoire.
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

    pub fn item_with_sources(&self, id: &str) -> Result<Item> {
        self.items("/Items", &[("Ids", id), ("Fields", "Overview,MediaSources")])?
            .into_iter()
            .next()
            .ok_or(Error { message: "élément introuvable".into() })
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

    /// `event` : "" au démarrage, "/Progress" pendant la lecture, "/Stopped" à la fin.
    pub fn report(&self, event: &str, body: Value) -> Result<()> {
        self.send("POST", &format!("/Sessions/Playing{event}"), &[], Some(body))?;
        Ok(())
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
