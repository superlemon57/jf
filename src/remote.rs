//! API HTTP locale pour la télécommande (application « jf Remote »).
//!
//! | Requête                                   | Effet                                  |
//! |-------------------------------------------|----------------------------------------|
//! | `GET  /api/state`                         | état de la lecture et pistes           |
//! | `GET  /api/artwork`                       | affiche de l'élément en cours          |
//! | `POST /api/key    {"key": "up"}`          | touche de navigation ou de lecture ; `skip` valide la proposition affichée |
//! | `POST /api/seek   {"position": 120}`      | saut absolu, ou `{"offset": -10}`      |
//! | `POST /api/track  {"kind": "audio", "id": 2}` | piste audio ou `"subtitle"` (`id` nul : aucune) |
//! | `POST /api/volume {"delta": 0.05}`        | volume du système                      |

use std::io::Read;
use std::process::Command as Process;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use eframe::egui;
use serde::Serialize;
use serde_json::{json, Value};
use tiny_http::{Header, Method, Request, Response, Server};

use crate::images::ImageRef;
use crate::jellyfin::Client;
use crate::player::{Track, TrackKind};

const WORKERS: usize = 3;
const SINK: &str = "@DEFAULT_AUDIO_SINK@";

#[derive(Clone, Default, Serialize)]
pub struct State {
    pub playing: bool,
    pub paused: bool,
    pub live: bool,
    pub title: String,
    pub position: f64,
    pub duration: f64,
    pub has_next: bool,
    pub has_previous: bool,
    pub audio: Vec<Track>,
    pub subtitles: Vec<Track>,
    /// Proposition à l'écran (« Passer l'intro », « Épisode suivant »), validée par la touche `skip`.
    pub prompt: Option<RemotePrompt>,
    /// Affiche de l'élément en cours et client pour aller la chercher.
    #[serde(skip)]
    pub artwork: Option<(ImageRef, Client)>,
}

#[derive(Clone, Serialize)]
pub struct RemotePrompt {
    /// `skip` (passage à sauter) ou `next` (compte à rebours de l'épisode suivant).
    pub kind: &'static str,
    pub label: String,
}

pub enum Command {
    Key(String),
    SeekTo(f64),
    SeekBy(f64),
    Track(TrackKind, Option<i64>),
}

pub type Shared = Arc<Mutex<State>>;

struct Context {
    state: Shared,
    commands: Sender<Command>,
    ui: egui::Context,
}

pub fn start(address: &str, state: Shared, commands: Sender<Command>, ui: egui::Context) -> Result<(), String> {
    let server = Arc::new(Server::http(address).map_err(|error| error.to_string())?);
    for _ in 0..WORKERS {
        let server = server.clone();
        let context = Context { state: state.clone(), commands: commands.clone(), ui: ui.clone() };
        std::thread::spawn(move || {
            for request in server.incoming_requests() {
                handle(request, &context);
            }
        });
    }
    Ok(())
}

fn reply(request: Request, status: u16, body: Value) {
    let header = Header::from_bytes("Content-Type", "application/json").expect("en-tête valide");
    let _ = request.respond(Response::from_string(body.to_string()).with_status_code(status).with_header(header));
}

fn handle(mut request: Request, context: &Context) {
    let path = request.url().split('?').next().unwrap_or("").to_owned();
    match (request.method(), path.as_str()) {
        (Method::Get, "/api/state") => {
            let state = context.state.lock().unwrap().clone();
            reply(request, 200, serde_json::to_value(state).unwrap_or_default());
        }
        (Method::Get, "/api/artwork") => {
            let artwork = context.state.lock().unwrap().artwork.clone();
            let bytes = artwork.and_then(|(image, client)| client.image(&image.id, image.kind, image.width).ok());
            match bytes {
                Some(bytes) => {
                    let _ = request.respond(Response::from_data(bytes));
                }
                None => reply(request, 404, json!({ "error": "pas d'image" })),
            }
        }
        (Method::Post, _) => {
            // JSON obligatoire : une page web tierce ne peut pas envoyer ce
            // Content-Type sans preflight CORS, auquel on ne répond pas.
            let is_json = request.headers().iter().any(|header| {
                header.field.equiv("Content-Type") && header.value.as_str().starts_with("application/json")
            });
            let mut text = String::new();
            let read = request.as_reader().take(4096).read_to_string(&mut text).is_ok();
            let body = serde_json::from_str::<Value>(&text).ok().filter(|_| is_json && read);
            match body.and_then(|body| post(&path, &body, context)) {
                Some(result) => reply(request, 200, result),
                None => reply(request, 400, json!({ "error": "requête invalide" })),
            }
        }
        _ => reply(request, 404, json!({ "error": "introuvable" })),
    }
}

fn post(path: &str, body: &Value, context: &Context) -> Option<Value> {
    let command = match path {
        "/api/key" => Command::Key(body["key"].as_str()?.to_owned()),
        "/api/seek" => match (body["position"].as_f64(), body["offset"].as_f64()) {
            (Some(position), _) => Command::SeekTo(position),
            (None, Some(offset)) => Command::SeekBy(offset),
            _ => return None,
        },
        "/api/track" => {
            let kind = match body["kind"].as_str()? {
                "audio" => TrackKind::Audio,
                "subtitle" => TrackKind::Subtitle,
                _ => return None,
            };
            Command::Track(kind, body["id"].as_i64())
        }
        "/api/volume" => return Some(json!({ "volume": change_volume(body["delta"].as_f64()?) })),
        _ => return None,
    };
    context.commands.send(command).ok()?;
    context.ui.request_repaint();
    Some(json!({ "ok": true }))
}

/// Règle le volume du système via PipeWire et renvoie le nouveau niveau (0 à 1).
pub(crate) fn change_volume(delta: f64) -> Option<f64> {
    let step = format!("{:.0}%{}", delta.abs().min(0.5) * 100.0, if delta < 0.0 { '-' } else { '+' });
    Process::new("wpctl").args(["set-volume", "--limit", "1.0", SINK, &step]).status().ok()?;
    volume()
}

/// Volume du système, de 0 à 1.
pub(crate) fn volume() -> Option<f64> {
    let output = Process::new("wpctl").args(["get-volume", SINK]).output().ok()?;
    String::from_utf8_lossy(&output.stdout).split_whitespace().nth(1)?.parse().ok()
}

pub(crate) fn set_volume(volume: f64) {
    let _ = Process::new("wpctl").args(["set-volume", "--limit", "1.0", SINK, &format!("{volume:.3}")]).status();
}

pub(crate) fn toggle_mute() {
    let _ = Process::new("wpctl").args(["set-mute", SINK, "toggle"]).status();
}
