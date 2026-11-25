//! Ce que partagent jf et la télécommande Apple TV d'iOS (`atv`) : l'état de la lecture
//! publié par l'interface, les commandes reçues, et le volume du système.

use std::process::Command as Process;
use std::sync::{Arc, Mutex};

use crate::player::{Track, TrackKind};

const SINK: &str = "@DEFAULT_AUDIO_SINK@";

/// État de la lecture, mis à jour par l'interface quatre fois par seconde.
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

/// Commande reçue de la télécommande, exécutée par le fil d'interface.
pub enum Command {
    /// Touche de navigation ou de lecture : `up`, `ok`, `back`, `home`, `menu`, `playpause`…
    Key(String),
    SeekBy(f64),
    Track(TrackKind, Option<i64>),
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
