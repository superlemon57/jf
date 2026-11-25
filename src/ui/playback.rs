//! Playback screen: mpv's video fills the window and the keyboard is passed to it (its
//! shortcuts and the user's `input.conf` apply). The mouse drives jf's playback
//! interface (`osd`).

use std::sync::Arc;

use eframe::egui::{self, Event, Key, Modifiers};
use eframe::glow::{self, HasContext};

use super::theme::{self, Text};
use crate::i18n::t;
use crate::mpv::Mpv;

/// Keys jf keeps for itself instead of passing them to mpv.
#[derive(Default)]
pub struct Input {
    /// Escape, Backspace or q: dismiss the countdown, else stop.
    pub back: bool,
    /// Enter: accept the prompt on screen, else pause.
    pub ok: bool,
    /// A key was pressed: the playback interface shows.
    pub activity: bool,
    /// `c`: "Subtitles and audio" panel.
    pub tracks: bool,
}

pub fn paint_video(ui: &egui::Ui, mpv: &Mpv, loading: bool) {
    let rect = ui.max_rect();
    ui.painter().rect_filled(rect, 0.0, egui::Color32::BLACK);
    let renderer = mpv.renderer();
    let callback = eframe::egui_glow::CallbackFn::new(move |info, painter| {
        let framebuffer = unsafe { painter.gl().get_parameter_i32(glow::FRAMEBUFFER_BINDING) };
        let [width, height] = info.screen_size_px;
        renderer.render(framebuffer, width as i32, height as i32);
    });
    ui.painter().add(egui::PaintCallback { rect, callback: Arc::new(callback) });
    if loading {
        let rem = theme::rem(rect);
        let text = Text {
            centered: true,
            ..Text::line(t("Loading…", "Chargement…"), 1.2 * rem, theme::DIM, rect.width())
        };
        text.paint(ui.painter(), rect.center());
    }
}

/// mpv name of a key without a printable character.
fn special(key: Key) -> Option<&'static str> {
    Some(match key {
        Key::ArrowLeft => "LEFT",
        Key::ArrowRight => "RIGHT",
        Key::ArrowUp => "UP",
        Key::ArrowDown => "DOWN",
        Key::Enter => "ENTER",
        Key::Tab => "TAB",
        Key::PageUp => "PGUP",
        Key::PageDown => "PGDWN",
        Key::Home => "HOME",
        Key::End => "END",
        Key::Insert => "INS",
        Key::Delete => "DEL",
        Key::F1 => "F1",
        Key::F2 => "F2",
        Key::F3 => "F3",
        Key::F4 => "F4",
        Key::F5 => "F5",
        Key::F6 => "F6",
        Key::F7 => "F7",
        Key::F8 => "F8",
        Key::F9 => "F9",
        Key::F10 => "F10",
        Key::F12 => "F12",
        _ => return None,
    })
}

fn with_modifiers(modifiers: Modifiers, name: &str) -> String {
    let mut full = String::new();
    for (held, prefix) in [(modifiers.ctrl, "Ctrl+"), (modifiers.alt, "Alt+"), (modifiers.shift, "Shift+")] {
        if held {
            full.push_str(prefix);
        }
    }
    full + name
}

pub fn forward_input(ctx: &egui::Context, mpv: &Mpv) -> Input {
    let events = ctx.input(|input| input.events.clone());
    let press = |name: &str| {
        let _ = mpv.command(&["keypress", name]);
    };
    let mut input = Input::default();
    for event in events {
        match event {
            Event::Key { pressed: true, .. } | Event::Text(_) => input.activity = true,
            _ => {}
        }
        match event {
            Event::Key { key: Key::Escape | Key::Backspace, pressed: true, .. } => input.back = true,
            Event::Key { key: Key::Enter, pressed: true, modifiers, .. } if modifiers.is_none() => input.ok = true,
            Event::Key { key, pressed: true, modifiers, .. } => {
                if let Some(name) = special(key) {
                    press(&with_modifiers(modifiers, name));
                } else if modifiers.ctrl || modifiers.alt {
                    // Without Ctrl or Alt, characters arrive as `Event::Text`.
                    let plain = Modifiers { shift: false, ..modifiers };
                    press(&with_modifiers(plain, &key.name().to_lowercase()));
                }
            }
            Event::Text(text) => {
                for character in text.chars() {
                    match character {
                        'q' | 'Q' => input.back = true,
                        'c' => input.tracks = true,
                        ' ' => press("SPACE"),
                        '#' => press("SHARP"),
                        _ => press(&character.to_string()),
                    }
                }
            }
            Event::MouseWheel { delta, .. } if delta.y != 0.0 => {
                press(if delta.y > 0.0 { "WHEEL_UP" } else { "WHEEL_DOWN" });
            }
            _ => {}
        }
    }
    input
}
