//! Lecture : file d'attente, résolution des flux, suivi de position vers Jellyfin.

use std::sync::mpsc::{channel, Sender};
use std::time::Instant;

use serde_json::{json, Value};

use crate::app::{Message, Tasks};
use crate::jellyfin::{ticks_to_seconds, Client, Item, Subtitle};
use crate::mpv::{Event, Mpv};

const LANGUAGES: [(&str, &str); 12] = [
    ("fre", "Français"),
    ("fra", "Français"),
    ("eng", "Anglais"),
    ("jpn", "Japonais"),
    ("spa", "Espagnol"),
    ("ger", "Allemand"),
    ("deu", "Allemand"),
    ("ita", "Italien"),
    ("por", "Portugais"),
    ("kor", "Coréen"),
    ("chi", "Chinois"),
    ("zho", "Chinois"),
];

/// Durée du compte à rebours avant l'épisode suivant, en secondes de lecture.
pub const COUNTDOWN: f64 = 10.0;

#[derive(Clone, Copy, PartialEq)]
enum SegmentKind {
    Intro,
    Recap,
    Outro,
}

#[derive(Clone, Copy)]
struct Segment {
    kind: SegmentKind,
    start: f64,
    end: f64,
}

/// Proposition affichée pendant la lecture, validée par Entrée ou la télécommande.
#[derive(Clone, Copy)]
pub enum Prompt<'a> {
    /// Sauter un passage jusqu'à `end`.
    Skip { label: &'static str, end: f64 },
    /// Compte à rebours avant l'élément suivant de la file.
    NextUp { item: &'a Item, remaining: f64 },
}

/// Tout ce qu'il faut pour lancer un élément dans mpv.
pub struct Stream {
    item: Item,
    url: String,
    /// En-tête d'authentification, pour les flux servis par Jellyfin uniquement.
    header: Option<String>,
    user_agent: Option<String>,
    start: f64,
    source_id: String,
    subtitles: Vec<Subtitle>,
    segments: Vec<Segment>,
    live: bool,
}

struct Current {
    item: Item,
    source_id: String,
    live: bool,
    subtitles: Vec<Subtitle>,
    segments: Vec<Segment>,
    /// L'utilisateur a choisi de regarder le générique : pas de compte à rebours.
    credits_watched: bool,
    /// Identifiant mpv du fichier, pour ignorer les événements d'un fichier précédent.
    entry: Option<i64>,
    loaded: bool,
    position: f64,
    duration: f64,
    paused: bool,
    last_report: Instant,
}

#[derive(Clone, PartialEq)]
pub struct Track {
    pub id: i64,
    pub label: String,
    pub selected: bool,
}

#[derive(Clone, Copy, PartialEq)]
pub enum TrackKind {
    Audio,
    Subtitle,
}

pub enum Outcome {
    Continue,
    /// La file est terminée : retour à la navigation.
    Ended,
    Failed(String),
}

pub struct Player {
    pub mpv: Mpv,
    client: Option<Client>,
    reports: Sender<(Client, &'static str, Value)>,
    queue: Vec<Item>,
    index: usize,
    current: Option<Current>,
    resolving: bool,
    /// Incrémenté à chaque demande, pour écarter les résolutions devenues obsolètes.
    generation: u64,
    /// Demande en cours de la suite de la série, pour un épisode lancé seul.
    queue_request: u64,
}

fn resolve(client: &Client, item: &Item, from_start: bool) -> Result<Stream, String> {
    if item.is_channel() {
        let sources = client.live_sources(&item.id).map_err(|error| error.to_string())?;
        let source = sources.into_iter().next().ok_or("cette chaîne n'a pas de flux")?;
        // Flux du fournisseur lu directement : le jeton Jellyfin ne lui est pas envoyé.
        let direct = source.path.as_deref().filter(|path| path.starts_with("http"));
        let (url, header) = match direct {
            Some(path) => (path.to_owned(), None),
            None => (
                format!("{}/Videos/{}/stream?static=true&mediaSourceId={}", client.server, item.id, source.id),
                Some(client.stream_header()),
            ),
        };
        let user_agent = source
            .required_http_headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("user-agent"))
            .map(|(_, value)| value.clone());
        return Ok(Stream {
            item: item.clone(),
            url,
            header,
            user_agent,
            start: 0.0,
            source_id: source.id,
            subtitles: Vec::new(),
            segments: Vec::new(),
            live: true,
        });
    }

    let full = client.item_with_sources(&item.id).map_err(|error| error.to_string())?;
    let source = full.media_sources.first().cloned().unwrap_or_default();
    let source_id = if source.id.is_empty() { full.id.clone() } else { source.id.clone() };
    let kind = if full.is_audio() { "Audio" } else { "Videos" };
    // Facultatif : un serveur sans segments renvoie une erreur ou une liste vide.
    let segments = client.segments(&full.id).unwrap_or_default();
    let segments = segments
        .into_iter()
        .filter_map(|segment| {
            let kind = match segment.kind.as_str() {
                "Intro" => SegmentKind::Intro,
                "Recap" => SegmentKind::Recap,
                "Outro" => SegmentKind::Outro,
                _ => return None,
            };
            let (start, end) = (ticks_to_seconds(segment.start_ticks), ticks_to_seconds(segment.end_ticks));
            (end > start).then_some(Segment { kind, start, end })
        })
        .collect();
    Ok(Stream {
        segments,
        url: format!("{}/{kind}/{}/stream?static=true&mediaSourceId={source_id}", client.server, full.id),
        header: Some(client.stream_header()),
        user_agent: None,
        start: if from_start { 0.0 } else { full.resume_seconds() },
        subtitles: client.external_subtitles(&full.id, &source),
        source_id,
        item: full,
        live: false,
    })
}

impl Player {
    pub fn new(mpv: Mpv) -> Self {
        // Les rapports partent d'un fil dédié, dans l'ordre, sans bloquer l'interface.
        let (reports, inbox) = channel::<(Client, &'static str, Value)>();
        std::thread::spawn(move || {
            for (client, event, body) in inbox {
                let _ = client.report(event, body);
            }
        });
        Self {
            mpv,
            client: None,
            reports,
            queue: Vec::new(),
            index: 0,
            current: None,
            resolving: false,
            generation: 0,
            queue_request: 0,
        }
    }

    pub fn set_client(&mut self, client: Option<Client>) {
        self.client = client;
    }

    pub fn is_active(&self) -> bool {
        self.resolving || self.current.is_some()
    }

    /// Vrai tant que la première image n'est pas prête.
    pub fn is_loading(&self) -> bool {
        self.resolving || self.current.as_ref().is_some_and(|current| !current.loaded)
    }

    pub fn play(&mut self, queue: Vec<Item>, index: usize, from_start: bool, tasks: &Tasks) {
        self.finish_current();
        self.queue = queue;
        self.load(index, from_start, tasks);
        self.queue_request += 1;
        // Un épisode lancé seul (Reprendre, À suivre, recherche) enchaîne sur la suite de la série.
        if let ([item], Some(client)) = (self.queue.as_slice(), self.client.clone()) {
            if let (true, Some(series)) = (item.kind == "Episode", item.series_id.clone()) {
                let (request, id) = (self.queue_request, item.id.clone());
                tasks.spawn(move || {
                    let episodes = client.episodes_from(&series, &id).map_err(|error| error.to_string());
                    Message::Episodes(request, episodes)
                });
            }
        }
    }

    /// Complète la file d'un épisode lancé seul avec les épisodes suivants.
    pub fn on_episodes(&mut self, request: u64, result: Result<Vec<Item>, String>) {
        let (Ok(episodes), true) = (result, request == self.queue_request) else { return };
        let [first] = self.queue.as_slice() else { return };
        if let Some(position) = episodes.iter().position(|episode| episode.id == first.id) {
            self.queue.extend(episodes.into_iter().skip(position + 1));
        }
    }

    fn load(&mut self, index: usize, from_start: bool, tasks: &Tasks) {
        let (Some(client), Some(item)) = (self.client.clone(), self.queue.get(index).cloned()) else { return };
        self.index = index;
        self.generation += 1;
        self.resolving = true;
        let generation = self.generation;
        tasks.spawn(move || Message::Resolved(generation, resolve(&client, &item, from_start).map(Box::new)));
    }

    pub fn on_resolved(&mut self, generation: u64, result: Result<Box<Stream>, String>) -> Outcome {
        if generation != self.generation {
            return Outcome::Continue;
        }
        self.resolving = false;
        let stream = match result {
            Ok(stream) => *stream,
            Err(message) => return self.fail(message),
        };
        self.mpv.set("http-header-fields", stream.header.as_deref().unwrap_or(""));
        self.mpv.set("user-agent", stream.user_agent.as_deref().unwrap_or(crate::USER_AGENT));
        let start = if stream.start > 5.0 { format!("{:.0}", stream.start) } else { "none".to_owned() };
        self.mpv.set("start", &start);
        self.mpv.set("force-media-title", &stream.item.play_title());
        self.mpv.set("pause", "no");
        if let Err(message) = self.mpv.command(&["loadfile", &stream.url, "replace"]) {
            return self.fail(message);
        }
        self.current = Some(Current {
            duration: stream.item.runtime_seconds(),
            item: stream.item,
            source_id: stream.source_id,
            live: stream.live,
            subtitles: stream.subtitles,
            segments: stream.segments,
            credits_watched: false,
            entry: None,
            loaded: false,
            position: stream.start,
            paused: false,
            last_report: Instant::now(),
        });
        Outcome::Continue
    }

    fn fail(&mut self, message: String) -> Outcome {
        self.stop();
        Outcome::Failed(message)
    }

    fn report(&self, event: &'static str, current: &Current) {
        // Le direct n'a pas de position à retenir.
        let (Some(client), false) = (&self.client, current.live) else { return };
        let body = json!({
            "ItemId": current.item.id,
            "MediaSourceId": current.source_id,
            "PositionTicks": (current.position.max(0.0) * 1e7) as i64,
            "IsPaused": current.paused,
            "CanSeek": true,
            "PlayMethod": "DirectPlay",
        });
        let _ = self.reports.send((client.clone(), event, body));
    }

    /// Clôt l'élément en cours en enregistrant sa position sur le serveur.
    fn finish_current(&mut self) {
        if let Some(current) = self.current.take() {
            if current.loaded {
                self.report("/Stopped", &current);
            }
        }
    }

    /// Arrête la lecture ; la position est conservée pour une reprise ultérieure.
    pub fn stop(&mut self) {
        self.generation += 1;
        self.resolving = false;
        self.finish_current();
        self.queue.clear();
        let _ = self.mpv.command(&["stop"]);
    }

    pub fn has_next(&self) -> bool {
        self.index + 1 < self.queue.len()
    }

    /// Passe à l'élément suivant (+1) ou précédent (-1) de la file.
    pub fn skip(&mut self, offset: isize, tasks: &Tasks) {
        let Some(index) = self.index.checked_add_signed(offset).filter(|index| *index < self.queue.len()) else {
            return;
        };
        self.finish_current();
        self.load(index, false, tasks);
    }

    pub fn handle(&mut self, event: Event, tasks: &Tasks) -> Outcome {
        let Some(current) = &mut self.current else { return Outcome::Continue };
        match event {
            Event::StartFile { entry } if current.entry.is_none() => current.entry = Some(entry),
            Event::FileLoaded if !current.loaded => {
                current.loaded = true;
                for subtitle in &current.subtitles {
                    let _ = self.mpv.command(&["sub-add", &subtitle.url, "auto", &subtitle.title, &subtitle.language]);
                }
                current.last_report = Instant::now();
                let current = self.current.as_ref().expect("lecture en cours");
                self.report("", current);
            }
            Event::Finished { entry } if current.entry == Some(entry) => {
                if current.duration > 0.0 {
                    current.position = current.duration;
                }
                self.finish_current();
                if !self.has_next() {
                    self.stop();
                    return Outcome::Ended;
                }
                self.load(self.index + 1, false, tasks);
            }
            Event::Failed { entry, message } if current.entry == Some(entry) => {
                return self.fail(format!("lecture impossible ({message})"));
            }
            _ => {}
        }
        Outcome::Continue
    }

    /// Moment où commence le compte à rebours de l'élément suivant : le générique de fin
    /// s'il est connu, sinon les dernières secondes (la fin du fichier enchaîne de toute façon).
    fn next_up_start(&self, current: &Current) -> Option<f64> {
        if current.live || !self.has_next() || current.duration <= 0.0 {
            return None;
        }
        let outro = current.segments.iter().find(|segment| segment.kind == SegmentKind::Outro);
        match outro {
            Some(outro) if current.duration - outro.start > 2.0 => Some(outro.start),
            _ if current.duration > 2.0 * COUNTDOWN => Some(current.duration - COUNTDOWN),
            _ => None,
        }
    }

    pub fn prompt(&self) -> Option<Prompt<'_>> {
        let current = self.current.as_ref().filter(|current| current.loaded)?;
        let position = current.position;
        if let (Some(start), false) = (self.next_up_start(current), current.credits_watched) {
            if position >= start {
                let item = self.queue.get(self.index + 1)?;
                return Some(Prompt::NextUp { item, remaining: (start + COUNTDOWN - position).max(0.0) });
            }
        }
        current.segments.iter().find_map(|segment| {
            let label = match segment.kind {
                SegmentKind::Intro => "Passer l'intro",
                SegmentKind::Recap => "Passer le récap",
                SegmentKind::Outro => return None,
            };
            // Plus rien à sauter dans la dernière seconde du passage.
            (position >= segment.start && position < segment.end - 1.0)
                .then_some(Prompt::Skip { label, end: segment.end })
        })
    }

    /// Valide la proposition affichée ; renvoie faux s'il n'y en a pas.
    pub fn accept(&mut self, tasks: &Tasks) -> bool {
        match self.prompt() {
            Some(Prompt::Skip { end, .. }) => {
                let _ = self.mpv.command(&["seek", &end.to_string(), "absolute"]);
                true
            }
            Some(Prompt::NextUp { .. }) => {
                self.skip(1, tasks);
                true
            }
            None => false,
        }
    }

    /// Écarte le compte à rebours pour regarder le générique ; renvoie faux s'il n'y en a pas.
    pub fn dismiss(&mut self) -> bool {
        if !matches!(self.prompt(), Some(Prompt::NextUp { .. })) {
            return false;
        }
        if let Some(current) = &mut self.current {
            current.credits_watched = true;
        }
        true
    }

    /// Élément en cours, ou en cours de chargement.
    pub fn queued(&self) -> Option<&Item> {
        self.item().or(self.queue.get(self.index))
    }

    /// Relit la position depuis mpv, gère le compte à rebours et envoie un rapport
    /// de progression toutes les 10 secondes.
    pub fn tick(&mut self, tasks: &Tasks) {
        let Some(current) = &mut self.current else { return };
        if !current.loaded {
            return;
        }
        if let Some(position) = self.mpv.get_f64("time-pos") {
            current.position = position;
        }
        if let Some(duration) = self.mpv.get_f64("duration").filter(|duration| *duration > 0.0) {
            current.duration = duration;
        }
        current.paused = self.mpv.get_flag("pause").unwrap_or(false);
        if current.last_report.elapsed().as_secs() >= 10 {
            current.last_report = Instant::now();
            let current = self.current.as_ref().expect("lecture en cours");
            self.report("/Progress", current);
        }

        let current = self.current.as_ref().expect("lecture en cours");
        let (position, paused) = (current.position, current.paused);
        // Revenir avant le générique relance le compte à rebours la fois suivante.
        let rewound = self.next_up_start(current).is_some_and(|start| position < start - 1.0);
        if rewound {
            self.current.as_mut().expect("lecture en cours").credits_watched = false;
        }
        if let Some(Prompt::NextUp { remaining, .. }) = self.prompt() {
            if remaining <= 0.0 && !paused {
                self.skip(1, tasks);
            }
        }
    }

    pub fn tracks(&self, kind: TrackKind) -> Vec<Track> {
        let wanted = if kind == TrackKind::Audio { "audio" } else { "sub" };
        let count = self.mpv.get_i64("track-list/count").unwrap_or(0);
        let mut tracks = Vec::new();
        for index in 0..count {
            let property = |name: &str| format!("track-list/{index}/{name}");
            if self.mpv.get_string(&property("type")).as_deref() != Some(wanted) {
                continue;
            }
            let Some(id) = self.mpv.get_i64(&property("id")) else { continue };
            let language = self.mpv.get_string(&property("lang")).map(|code| {
                let known = LANGUAGES.iter().find(|(known, _)| *known == code);
                known.map_or(code.clone(), |(_, name)| (*name).to_owned())
            });
            let title = self.mpv.get_string(&property("title"));
            let label = match (language, title) {
                (Some(language), Some(title)) if title != language => format!("{language} — {title}"),
                (Some(text), _) | (None, Some(text)) => text,
                (None, None) => format!("Piste {id}"),
            };
            tracks.push(Track { id, label, selected: self.mpv.get_flag(&property("selected")).unwrap_or(false) });
        }
        tracks
    }

    /// `id` absent : désactive la piste (sous-titres masqués, par exemple).
    pub fn select_track(&self, kind: TrackKind, id: Option<i64>) {
        let property = if kind == TrackKind::Audio { "aid" } else { "sid" };
        self.mpv.set(property, &id.map_or("no".to_owned(), |id| id.to_string()));
    }

    pub fn item(&self) -> Option<&Item> {
        self.current.as_ref().map(|current| &current.item)
    }

    /// (position, durée, en pause, en direct)
    pub fn progress(&self) -> Option<(f64, f64, bool, bool)> {
        let current = self.current.as_ref()?;
        Some((current.position, current.duration, current.paused, current.live))
    }
}
