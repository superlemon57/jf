//! Écran de lecture : la vidéo de mpv remplit la fenêtre, et le clavier
//! comme la souris lui sont transmis (ses raccourcis et son interface s'appliquent).

use std::sync::Arc;

use eframe::egui::{self, Event, Key, Modifiers, PointerButton};
use eframe::glow::{self, HasContext};

use super::theme::{self, Text};
use crate::mpv::Mpv;

pub enum Input {
    None,
    /// L'utilisateur a demandé l'arrêt (Échap, Retour arrière ou q).
    Stop,
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
    let (events, scale) = ctx.input(|input| (input.events.clone(), input.pixels_per_point));
    let press = |name: &str| {
        let _ = mpv.command(&["keypress", name]);
    };
    for event in events {
        match event {
            Event::Key { key: Key::Escape | Key::Backspace, pressed: true, .. } => return Input::Stop,
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
                        'q' | 'Q' => return Input::Stop,
                        ' ' => press("SPACE"),
                        '#' => press("SHARP"),
                        _ => press(&character.to_string()),
                    }
                }
            }
            Event::PointerMoved(position) => {
                let (x, y) = ((position.x * scale) as i64, (position.y * scale) as i64);
                let _ = mpv.command(&["mouse", &x.to_string(), &y.to_string()]);
            }
            Event::PointerButton { button, pressed, .. } => {
                let name = match button {
                    PointerButton::Primary => "MBTN_LEFT",
                    PointerButton::Secondary => "MBTN_RIGHT",
                    PointerButton::Middle => "MBTN_MID",
                    _ => continue,
                };
                let _ = mpv.command(&[if pressed { "keydown" } else { "keyup" }, name]);
            }
            Event::MouseWheel { delta, .. } if delta.y != 0.0 => {
                press(if delta.y > 0.0 { "WHEEL_UP" } else { "WHEEL_DOWN" });
            }
            _ => {}
        }
    }
    Input::None
}
