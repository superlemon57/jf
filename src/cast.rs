//! Remote control from other Jellyfin apps: jf shows up among the devices of the cast
//! button and in the server's "now playing", and plays what they send. jf declares
//! what it supports, then keeps the server's WebSocket open to receive commands
//! (protocol as in jellyfin-web and jellyfin-mpv-shim).

use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui;
use serde_json::{json, Value};
use tungstenite::client::IntoClientRequest;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

use crate::jellyfin::Client;
use crate::lan::Log;
use crate::player::TrackKind;
use crate::remote::{self, Command, Queue};

/// Commands other apps may send, in addition to playback ones (always accepted).
const SUPPORTED: [&str; 18] = [
    "MoveUp",
    "MoveDown",
    "MoveLeft",
    "MoveRight",
    "Select",
    "Back",
    "GoHome",
    "ToggleContextMenu",
    "ToggleFullscreen",
    "VolumeUp",
    "VolumeDown",
    "Mute",
    "Unmute",
    "ToggleMute",
    "SetVolume",
    "SetAudioStreamIndex",
    "SetSubtitleStreamIndex",
    "DisplayMessage",
];

const SEEK_STEP: f64 = 10.0;
const VOLUME_STEP: f64 = 0.05;
/// Delay before reconnecting, doubled after each failure up to the maximum.
const RETRY: Duration = Duration::from_secs(2);
const RETRY_MAX: Duration = Duration::from_secs(60);

/// Protocol log: `jellyfin-remote.log` in the cache folder.
static LOG: Log = Log::new("jellyfin-remote");

/// Runs while jf is signed in; dropping it closes the connection.
pub struct Handle {
    stop: Arc<AtomicBool>,
}

impl Drop for Handle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

pub fn start(client: Client, commands: Sender<Command>, ui: egui::Context) -> Handle {
    static OPENED: std::sync::Once = std::sync::Once::new();
    OPENED.call_once(|| LOG.open());
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    std::thread::spawn(move || {
        let mut retry = RETRY;
        while !flag.load(Ordering::SeqCst) {
            let started = Instant::now();
            match connect(&client) {
                Ok(socket) => {
                    LOG.line("connected");
                    let result = listen(socket, &commands, &ui, &flag);
                    LOG.line(&format!("disconnected: {}", result.err().unwrap_or_else(|| "closed".into())));
                }
                Err(error) => LOG.line(&format!("connection failed: {error}")),
            }
            // A connection that lasted resets the delay.
            retry = if started.elapsed() > RETRY_MAX { RETRY } else { (retry * 2).min(RETRY_MAX) };
            let waited = Instant::now();
            while waited.elapsed() < retry && !flag.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(250));
            }
        }
    });
    Handle { stop }
}

type Socket = WebSocket<MaybeTlsStream<TcpStream>>;

fn connect(client: &Client) -> Result<Socket, String> {
    client.send_capabilities(&SUPPORTED).map_err(|error| error.to_string())?;
    let (address, authorization) = client.socket();
    let mut request = address.into_client_request().map_err(|error| error.to_string())?;
    let headers = request.headers_mut();
    headers.insert("Authorization", authorization.parse().map_err(|_| "invalid token")?);
    headers.insert("User-Agent", crate::USER_AGENT.parse().expect("valid header"));
    let (socket, _) = tungstenite::connect(request).map_err(|error| error.to_string())?;
    // Short reads, to send keepalives and notice the end of the session.
    let timeout = Some(Duration::from_secs(1));
    match socket.get_ref() {
        MaybeTlsStream::Plain(stream) => stream.set_read_timeout(timeout),
        MaybeTlsStream::Rustls(stream) => stream.get_ref().set_read_timeout(timeout),
        _ => Ok(()),
    }
    .map_err(|error| error.to_string())?;
    Ok(socket)
}

fn listen(mut socket: Socket, commands: &Sender<Command>, ui: &egui::Context, stop: &AtomicBool) -> Result<(), String> {
    // The server asks for a keepalive at half its timeout (ForceKeepAlive).
    let mut keepalive = Duration::from_secs(30);
    let mut sent = Instant::now();
    socket.send(keep_alive()).map_err(|error| error.to_string())?;
    while !stop.load(Ordering::SeqCst) {
        if sent.elapsed() >= keepalive {
            sent = Instant::now();
            socket.send(keep_alive()).map_err(|error| error.to_string())?;
        }
        let text = match socket.read() {
            Ok(Message::Text(text)) => text,
            Ok(Message::Close(_)) => return Err("closed by the server".into()),
            Ok(_) => continue,
            Err(tungstenite::Error::Io(error))
                if matches!(error.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) =>
            {
                continue
            }
            Err(error) => return Err(error.to_string()),
        };
        let Ok(message) = serde_json::from_str::<Value>(&text) else { continue };
        let kind = message["MessageType"].as_str().unwrap_or_default();
        let data = &message["Data"];
        match kind {
            "ForceKeepAlive" => {
                let seconds = data.as_u64().unwrap_or(60).max(10);
                keepalive = Duration::from_secs(seconds / 2);
            }
            "Play" | "Playstate" | "GeneralCommand" => {
                LOG.line(&format!("{kind}: {data}"));
                for command in commands_for(kind, data) {
                    let _ = commands.send(command);
                }
                ui.request_repaint();
            }
            _ => {}
        }
    }
    let _ = socket.close(None);
    Ok(())
}

fn keep_alive() -> Message {
    Message::text(json!({ "MessageType": "KeepAlive" }).to_string())
}

fn ticks(value: &Value) -> Option<f64> {
    let ticks = value.as_i64().or_else(|| value.as_str()?.parse().ok())?;
    Some(ticks as f64 / 1e7)
}

/// Arguments of general commands are strings.
fn number(value: &Value) -> Option<f64> {
    value.as_f64().or_else(|| value.as_str()?.parse().ok())
}

fn key(name: &str) -> Vec<Command> {
    vec![Command::Key(name.to_owned())]
}

fn commands_for(kind: &str, data: &Value) -> Vec<Command> {
    match kind {
        "Play" => {
            let ids: Vec<String> = data["ItemIds"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|id| Some(id.as_str()?.to_owned()))
                .collect();
            if ids.is_empty() {
                return Vec::new();
            }
            let queue = match data["PlayCommand"].as_str() {
                Some("PlayNext") => Queue::Next,
                Some("PlayLast") => Queue::Last,
                _ => Queue::Now,
            };
            let index = data["StartIndex"].as_u64().unwrap_or(0) as usize;
            let start = ticks(&data["StartPositionTicks"]).filter(|start| *start > 0.0);
            let mut commands = vec![Command::Play { ids, index, start, queue }];
            for (field, kind) in [("AudioStreamIndex", TrackKind::Audio), ("SubtitleStreamIndex", TrackKind::Subtitle)]
            {
                if let Some(index) = data[field].as_i64() {
                    commands.push(Command::Stream(kind, index));
                }
            }
            commands
        }
        "Playstate" => match data["Command"].as_str().unwrap_or_default() {
            "Stop" => key("stop"),
            "Pause" => key("pause"),
            "Unpause" => key("play"),
            "PlayPause" => key("playpause"),
            "NextTrack" => key("next"),
            "PreviousTrack" => key("previous"),
            "Seek" => ticks(&data["SeekPositionTicks"]).map(Command::SeekTo).into_iter().collect(),
            "Rewind" => vec![Command::SeekBy(-SEEK_STEP)],
            "FastForward" => vec![Command::SeekBy(SEEK_STEP)],
            _ => Vec::new(),
        },
        _ => general(data["Name"].as_str().unwrap_or_default(), &data["Arguments"]),
    }
}

fn general(name: &str, arguments: &Value) -> Vec<Command> {
    match name {
        "MoveUp" => key("up"),
        "MoveDown" => key("down"),
        "MoveLeft" => key("left"),
        "MoveRight" => key("right"),
        "Select" => key("ok"),
        "Back" => key("back"),
        "GoHome" => key("home"),
        "ToggleContextMenu" => key("menu"),
        "ToggleFullscreen" => vec![Command::Fullscreen],
        // The system volume, as with the phone remotes (no effect where jf cannot set it).
        "VolumeUp" | "VolumeDown" => {
            remote::change_volume(if name == "VolumeUp" { VOLUME_STEP } else { -VOLUME_STEP });
            Vec::new()
        }
        "Mute" | "Unmute" | "ToggleMute" => {
            let muted = remote::muted();
            if name == "ToggleMute" || (name == "Mute") != muted {
                remote::toggle_mute();
            }
            Vec::new()
        }
        "SetVolume" => {
            if let Some(volume) = number(&arguments["Volume"]) {
                remote::set_volume((volume / 100.0).clamp(0.0, 1.0));
            }
            Vec::new()
        }
        "SetAudioStreamIndex" | "SetSubtitleStreamIndex" => {
            let kind = if name == "SetAudioStreamIndex" { TrackKind::Audio } else { TrackKind::Subtitle };
            number(&arguments["Index"]).map(|index| Command::Stream(kind, index as i64)).into_iter().collect()
        }
        "DisplayMessage" => {
            let text = arguments["Text"].as_str().or(arguments["Header"].as_str()).unwrap_or_default();
            if text.is_empty() {
                Vec::new()
            } else {
                vec![Command::Notice(text.to_owned())]
            }
        }
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn play_command() {
        let data = json!({
            "ItemIds": ["a", "b"],
            "StartIndex": 1,
            "StartPositionTicks": 600_000_000i64,
            "PlayCommand": "PlayNow",
            "SubtitleStreamIndex": -1,
        });
        let commands = commands_for("Play", &data);
        let Some(Command::Play { ids, index, start, queue }) = commands.first() else { panic!("no play command") };
        assert_eq!((ids.len(), *index, *start, *queue == Queue::Now), (2, 1, Some(60.0), true));
        assert!(matches!(commands.get(1), Some(Command::Stream(TrackKind::Subtitle, -1))));
    }

    #[test]
    fn seek_and_arguments_as_strings() {
        let seek = commands_for("Playstate", &json!({ "Command": "Seek", "SeekPositionTicks": 1_200_000_000i64 }));
        assert!(matches!(seek.as_slice(), [Command::SeekTo(time)] if *time == 120.0));
        let audio = general("SetAudioStreamIndex", &json!({ "Index": "2" }));
        assert!(matches!(audio.as_slice(), [Command::Stream(TrackKind::Audio, 2)]));
    }
}
