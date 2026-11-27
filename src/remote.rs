//! What jf shares with its remotes (iOS Apple TV remote, Android TV remote, other
//! Jellyfin apps): the playback state published by the interface, the commands
//! received, and the system volume.

use std::process::Command as Process;
use std::sync::{Arc, Mutex};

use crate::player::{Track, TrackKind};

const SINK: &str = "@DEFAULT_AUDIO_SINK@";

/// Playback state, updated by the interface four times per second.
#[derive(Clone, Default)]
pub struct State {
    pub playing: bool,
    pub paused: bool,
    pub live: bool,
    pub title: String,
    pub duration: f64,
    pub subtitles: Vec<Track>,
}

pub type Shared = Arc<Mutex<State>>;

/// Command received from a remote, run on the interface thread.
pub enum Command {
    /// Navigation or playback key: `up`, `ok`, `back`, `home`, `menu`, `playpause`…
    Key(String),
    SeekBy(f64),
    SeekTo(f64),
    /// mpv track id; `None` turns the track off.
    Track(TrackKind, Option<i64>),
    /// Jellyfin stream index of the current item; negative turns subtitles off.
    Stream(TrackKind, i64),
    /// Items sent from another Jellyfin app, played from `index` at `start` seconds
    /// (`None` resumes).
    Play {
        ids: Vec<String>,
        index: usize,
        start: Option<f64>,
        queue: Queue,
    },
    Fullscreen,
    Notice(String),
}

/// Where items sent to jf go.
#[derive(Clone, Copy, PartialEq)]
pub enum Queue {
    Now,
    Next,
    Last,
}

// System volume through PipeWire (`wpctl`) on Linux. Elsewhere the remote's volume
// buttons do nothing: the system and the TV keep their own controls.

/// Changes the system volume and returns the new level (0 to 1).
pub(crate) fn change_volume(delta: f64) -> Option<f64> {
    let step = format!("{:.0}%{}", delta.abs().min(0.5) * 100.0, if delta < 0.0 { '-' } else { '+' });
    wpctl(&["set-volume", "--limit", "1.0", SINK, &step])?;
    volume()
}

/// System volume, from 0 to 1.
pub(crate) fn volume() -> Option<f64> {
    let output = wpctl(&["get-volume", SINK])?;
    String::from_utf8_lossy(&output.stdout).split_whitespace().nth(1)?.parse().ok()
}

pub(crate) fn muted() -> bool {
    wpctl(&["get-volume", SINK]).is_some_and(|output| String::from_utf8_lossy(&output.stdout).contains("MUTED"))
}

pub(crate) fn set_volume(volume: f64) {
    wpctl(&["set-volume", "--limit", "1.0", SINK, &format!("{volume:.3}")]);
}

pub(crate) fn toggle_mute() {
    wpctl(&["set-mute", SINK, "toggle"]);
}

fn wpctl(arguments: &[&str]) -> Option<std::process::Output> {
    if cfg!(target_os = "linux") {
        Process::new("wpctl").args(arguments).output().ok().filter(|output| output.status.success())
    } else {
        None
    }
}
