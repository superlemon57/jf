//! Playback: queue, stream resolution, position reporting to Jellyfin.

use std::sync::mpsc::{channel, Sender};
use std::time::Instant;

use serde_json::{json, Value};

use crate::app::{Message, Tasks};
use crate::i18n::t;
use crate::jellyfin::{ticks_to_seconds, Client, Item, Subtitle};
use crate::mpv::{Event, Mpv};

/// Track language names: (ISO 639-2 code, English, French).
const LANGUAGES: [(&str, &str, &str); 12] = [
    ("fre", "French", "Français"),
    ("fra", "French", "Français"),
    ("eng", "English", "Anglais"),
    ("jpn", "Japanese", "Japonais"),
    ("spa", "Spanish", "Espagnol"),
    ("ger", "German", "Allemand"),
    ("deu", "German", "Allemand"),
    ("ita", "Italian", "Italien"),
    ("por", "Portuguese", "Portugais"),
    ("kor", "Korean", "Coréen"),
    ("chi", "Chinese", "Chinois"),
    ("zho", "Chinese", "Chinois"),
];

/// Length of the countdown before the next episode, in seconds of playback.
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

/// Prompt shown during playback, accepted with Enter or the remote.
#[derive(Clone, Copy)]
pub enum Prompt<'a> {
    /// Skip a segment up to `end`.
    Skip { label: &'static str, end: f64 },
    /// Countdown before the next item in the queue.
    NextUp { item: &'a Item, remaining: f64 },
}

/// Everything needed to start an item in mpv.
pub struct Stream {
    item: Item,
    url: String,
    /// Authentication header, only for streams served by Jellyfin.
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
    /// The user chose to watch the credits: no countdown.
    credits_watched: bool,
    /// mpv id of the file, to ignore events from a previous file.
    entry: Option<i64>,
    loaded: bool,
    position: f64,
    duration: f64,
    paused: bool,
    last_report: Instant,
    /// Position and pause state in the last report, to report a seek or a pause at once.
    reported: (f64, bool),
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
    /// The queue is over: back to browsing.
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
    /// Incremented on each request, to drop resolutions that became stale.
    generation: u64,
    /// Pending request for the rest of the series, for an episode started on its own.
    queue_request: u64,
}

/// `start`: position in seconds, or `None` to resume where the user stopped.
fn resolve(client: &Client, item: &Item, start: Option<f64>) -> Result<Stream, String> {
    if item.is_channel() {
        let sources = client.live_sources(&item.id).map_err(|error| error.to_string())?;
        let source =
            sources.into_iter().next().ok_or(t("this channel has no stream", "cette chaîne n'a pas de flux"))?;
        // Provider stream played directly: the Jellyfin token is not sent to it.
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
    // Optional: a server without segments returns an error or an empty list.
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
        start: start.unwrap_or_else(|| full.resume_seconds()),
        subtitles: client.external_subtitles(&full.id, &source),
        source_id,
        item: full,
        live: false,
    })
}

impl Player {
    pub fn new(mpv: Mpv) -> Self {
        // Reports go out from a dedicated thread, in order, without blocking the interface.
        let (reports, inbox) = channel::<(Client, &'static str, Value)>();
        std::thread::spawn(move || {
            for (client, event, mut body) in inbox {
                // System volume, for the remote controls of other Jellyfin apps.
                if let Some(volume) = crate::remote::volume() {
                    body["VolumeLevel"] = ((volume * 100.0).round() as i64).into();
                    body["IsMuted"] = crate::remote::muted().into();
                }
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

    /// True until the first frame is ready.
    pub fn is_loading(&self) -> bool {
        self.resolving || self.current.as_ref().is_some_and(|current| !current.loaded)
    }

    /// `start`: position in seconds in the first item, or `None` to resume it.
    pub fn play(&mut self, queue: Vec<Item>, index: usize, start: Option<f64>, tasks: &Tasks) {
        self.finish_current();
        self.queue = queue;
        self.load(index, start, tasks);
        self.queue_request += 1;
        // An episode started on its own (Continue watching, Next up, search) goes on with the rest of the series.
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

    /// Extends the queue of an episode started on its own with the following episodes.
    pub fn on_episodes(&mut self, request: u64, result: Result<Vec<Item>, String>) {
        let (Ok(episodes), true) = (result, request == self.queue_request) else { return };
        let [first] = self.queue.as_slice() else { return };
        if let Some(position) = episodes.iter().position(|episode| episode.id == first.id) {
            self.queue.extend(episodes.into_iter().skip(position + 1));
        }
    }

    /// Adds items after the current one (`next`) or at the end of the queue.
    pub fn enqueue(&mut self, items: Vec<Item>, next: bool) {
        let at = if next { (self.index + 1).min(self.queue.len()) } else { self.queue.len() };
        self.queue.splice(at..at, items);
    }

    fn load(&mut self, index: usize, start: Option<f64>, tasks: &Tasks) {
        let (Some(client), Some(item)) = (self.client.clone(), self.queue.get(index).cloned()) else { return };
        self.index = index;
        self.generation += 1;
        self.resolving = true;
        let generation = self.generation;
        tasks.spawn(move || Message::Resolved(generation, resolve(&client, &item, start).map(Box::new)));
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
            reported: (stream.start, false),
        });
        Outcome::Continue
    }

    fn fail(&mut self, message: String) -> Outcome {
        self.stop();
        Outcome::Failed(message)
    }

    /// Tells the server what plays: it keeps the position and shows jf among the devices
    /// playing, which other Jellyfin apps can control.
    fn report(&self, event: &'static str, current: &Current) {
        let Some(client) = &self.client else { return };
        let queue: Vec<Value> =
            self.queue.iter().map(|item| json!({ "Id": item.id, "PlaylistItemId": item.id })).collect();
        let mut body = json!({
            "ItemId": current.item.id,
            "MediaSourceId": current.source_id,
            "PositionTicks": (current.position.max(0.0) * 1e7) as i64,
            "IsPaused": current.paused,
            "CanSeek": !current.live,
            "PlayMethod": "DirectPlay",
            "RepeatMode": "RepeatNone",
            "NowPlayingQueue": queue,
            "PlaylistItemId": current.item.id,
        });
        if let Some(index) = self.stream_index(TrackKind::Audio) {
            body["AudioStreamIndex"] = index.into();
        }
        if let Some(index) = self.stream_index(TrackKind::Subtitle) {
            body["SubtitleStreamIndex"] = index.into();
        }
        let _ = self.reports.send((client.clone(), event, body));
    }

    /// Ends the current item, saving its position on the server.
    fn finish_current(&mut self) {
        if let Some(current) = self.current.take() {
            if current.loaded {
                self.report("/Stopped", &current);
            }
        }
    }

    /// Stops playback; the position is kept to resume later.
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

    /// Moves to the next (+1) or previous (-1) item in the queue.
    pub fn skip(&mut self, offset: isize, tasks: &Tasks) {
        let Some(index) = self.index.checked_add_signed(offset).filter(|index| *index < self.queue.len()) else {
            return;
        };
        self.finish_current();
        self.load(index, None, tasks);
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
                current.reported = (current.position, current.paused);
                let current = self.current.as_ref().expect("playback in progress");
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
                self.load(self.index + 1, None, tasks);
            }
            Event::Failed { entry, message } if current.entry == Some(entry) => {
                return self.fail(format!("{} ({message})", t("playback failed", "lecture impossible")));
            }
            _ => {}
        }
        Outcome::Continue
    }

    /// When the countdown to the next item starts: the end credits if known, else the
    /// last seconds (the end of the file moves on anyway).
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
                SegmentKind::Intro => t("Skip intro", "Passer l'intro"),
                SegmentKind::Recap => t("Skip recap", "Passer le récap"),
                SegmentKind::Outro => return None,
            };
            // Nothing left to skip in the last second of the segment.
            (position >= segment.start && position < segment.end - 1.0)
                .then_some(Prompt::Skip { label, end: segment.end })
        })
    }

    /// Accepts the prompt on screen; returns false if there is none.
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

    /// Dismisses the countdown to watch the credits; returns false if there is none.
    pub fn dismiss(&mut self) -> bool {
        if !matches!(self.prompt(), Some(Prompt::NextUp { .. })) {
            return false;
        }
        if let Some(current) = &mut self.current {
            current.credits_watched = true;
        }
        true
    }

    /// Current item, or the one loading.
    pub fn queued(&self) -> Option<&Item> {
        self.item().or(self.queue.get(self.index))
    }

    /// Reads the position back from mpv, runs the countdown and sends a progress report
    /// every 10 seconds.
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
        // Every 10 seconds, and at once after a pause or a seek.
        let since = current.last_report.elapsed().as_secs_f64();
        let (position, paused) = current.reported;
        let expected = if paused { position } else { position + since };
        let changed = current.paused != paused || (current.position - expected).abs() > 3.0;
        if changed || since >= 10.0 {
            current.last_report = Instant::now();
            current.reported = (current.position, current.paused);
            let current = self.current.as_ref().expect("playback in progress");
            self.report("/Progress", current);
        }

        let current = self.current.as_ref().expect("playback in progress");
        let (position, paused) = (current.position, current.paused);
        // Going back before the credits restarts the countdown next time.
        let rewound = self.next_up_start(current).is_some_and(|start| position < start - 1.0);
        if rewound {
            self.current.as_mut().expect("playback in progress").credits_watched = false;
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
                let known = LANGUAGES.iter().find(|(known, _, _)| *known == code);
                known.map_or(code.clone(), |(_, english, french)| t(english, french).to_owned())
            });
            let title = self.mpv.get_string(&property("title"));
            let label = match (language, title) {
                (Some(language), Some(title)) if title != language => format!("{language} — {title}"),
                (Some(text), _) | (None, Some(text)) => text,
                (None, None) => format!("{} {id}", t("Track", "Piste")),
            };
            tracks.push(Track { id, label, selected: self.mpv.get_flag(&property("selected")).unwrap_or(false) });
        }
        tracks
    }

    /// No `id`: disables the track (hides subtitles, for example).
    pub fn select_track(&self, kind: TrackKind, id: Option<i64>) {
        let property = if kind == TrackKind::Audio { "aid" } else { "sid" };
        self.mpv.set(property, &id.map_or("no".to_owned(), |id| id.to_string()));
    }

    /// Jellyfin stream indices of the current item, in the order of mpv's track ids
    /// (1, 2…): embedded streams in file order, then the external subtitles jf added.
    fn jellyfin_streams(&self, kind: TrackKind) -> Vec<i64> {
        let Some(current) = &self.current else { return Vec::new() };
        let wanted = if kind == TrackKind::Audio { "Audio" } else { "Subtitle" };
        let source = current.item.media_sources.iter().find(|source| source.id == current.source_id);
        let embedded = source.into_iter().flat_map(|source| &source.media_streams);
        let mut streams: Vec<i64> =
            embedded.filter(|stream| stream.kind == wanted && !stream.is_external).map(|stream| stream.index).collect();
        if kind == TrackKind::Subtitle {
            streams.extend(current.subtitles.iter().map(|subtitle| subtitle.index));
        }
        streams
    }

    /// Selects a track by its Jellyfin stream index; a negative index hides subtitles.
    pub fn select_stream(&self, kind: TrackKind, index: i64) {
        if index < 0 {
            return self.select_track(kind, None);
        }
        if let Some(position) = self.jellyfin_streams(kind).iter().position(|stream| *stream == index) {
            self.select_track(kind, Some(position as i64 + 1));
        }
    }

    /// Jellyfin stream index of the selected track; -1 for subtitles turned off.
    fn stream_index(&self, kind: TrackKind) -> Option<i64> {
        let property = if kind == TrackKind::Audio { "aid" } else { "sid" };
        match self.mpv.get_i64(property) {
            Some(id) => self.jellyfin_streams(kind).get(usize::try_from(id - 1).ok()?).copied(),
            None if kind == TrackKind::Subtitle => Some(-1),
            None => None,
        }
    }

    pub fn item(&self) -> Option<&Item> {
        self.current.as_ref().map(|current| &current.item)
    }

    /// (position, duration, paused, live)
    pub fn progress(&self) -> Option<(f64, f64, bool, bool)> {
        let current = self.current.as_ref()?;
        Some((current.position, current.duration, current.paused, current.live))
    }
}
