//! Écran de lecture : la vidéo de mpv remplit la fenêtre et le clavier lui est transmis
//! (ses raccourcis et l'`input.conf` de l'utilisateur s'appliquent). La souris est
//! gérée par l'interface de lecture de jf (`osd`).

use std::sync::Arc;

use eframe::egui::{self, Event, Key, Modifiers};
use eframe::glow::{self, HasContext};

use super::theme::{self, Text};
use crate::mpv::Mpv;

/// Touches que jf garde pour lui au lieu de les passer à mpv.
#[derive(Default)]
pub struct Input {
    /// Échap, Retour arrière ou q : écarter le compte à rebours, sinon arrêter.
    pub back: bool,
    /// Entrée : valider la proposition affichée, sinon pause.
    pub ok: bool,
    /// Une touche a été pressée : l'interface de lecture se montre.
    pub activity: bool,
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
        let text = Text { centered: true, ..Text::line("Chargement…", 1.2 * rem, theme::DIM, rect.width()) };
        text.paint(ui.painter(), rect.center());
    }
}

/// Nom mpv d'une touche sans caractère imprimable.
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
                    // Sans Ctrl ni Alt, les caractères arrivent par `Event::Text`.
                    let plain = Modifiers { shift: false, ..modifiers };
                    press(&with_modifiers(plain, &key.name().to_lowercase()));
                }
            }
            Event::Text(text) => {
                for character in text.chars() {
                    match character {
                        'q' | 'Q' => input.back = true,
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
