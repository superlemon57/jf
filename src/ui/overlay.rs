//! Elements drawn over the browser: options menu and messages.

use eframe::egui::{self, Color32, Pos2, Rect, Vec2};

use super::theme::{self, Text};
use crate::i18n::t;
use crate::jellyfin::Item;

#[derive(Clone, Copy, PartialEq)]
pub enum Action {
    Play,
    Restart,
    Open,
    Shuffle,
    TogglePlayed,
}

/// Options offered for the selected item.
pub struct Menu {
    pub item: Item,
    entries: Vec<(String, Action)>,
    focus: usize,
}

impl Menu {
    pub fn new(item: &Item) -> Self {
        let mut entries = Vec::new();
        let played = if item.user_data.played {
            t("Mark as unwatched", "Marquer comme non vu")
        } else {
            t("Mark as watched", "Marquer comme vu")
        };
        if item.is_channel() {
            entries.push((t("Watch", "Regarder").to_owned(), Action::Play));
        } else if item.is_playable() {
            let resume = item.resume_seconds();
            if resume > 0.0 {
                entries.push((
                    format!("{} {}", t("Resume from", "Reprendre à"), super::browse::clock(resume)),
                    Action::Play,
                ));
                entries.push((t("Play from the start", "Lire depuis le début").to_owned(), Action::Restart));
            } else {
                entries.push((t("Play", "Lire").to_owned(), Action::Play));
            }
            entries.push((played.to_owned(), Action::TogglePlayed));
        } else {
            entries.push((t("Open", "Ouvrir").to_owned(), Action::Open));
            let library = matches!(item.kind.as_str(), "CollectionFolder" | "UserView");
            if item.collection_type.as_deref() != Some("livetv") {
                entries.push((t("Shuffle", "Lecture aléatoire").to_owned(), Action::Shuffle));
            }
            if !library {
                entries.push((played.to_owned(), Action::TogglePlayed));
            }
        }
        Self { item: item.clone(), entries, focus: 0 }
    }

    pub fn move_focus(&mut self, down: bool) {
        let last = self.entries.len() - 1;
        self.focus = if down { (self.focus + 1).min(last) } else { self.focus.saturating_sub(1) };
    }

    pub fn selected(&self) -> Action {
        self.entries[self.focus].1
    }

    /// Draws the menu; returns the clicked entry, if any.
    pub fn paint(&mut self, painter: &egui::Painter, screen: Rect, click: Option<Pos2>) -> Option<Action> {
        let rem = theme::rem(screen);
        painter.rect_filled(screen, 0.0, Color32::from_black_alpha(170));

        let (padding, row) = (1.2 * rem, 2.8 * rem);
        let width = 24.0 * rem;
        let height = 2.0 * padding + 2.4 * rem + self.entries.len() as f32 * row;
        let panel = Rect::from_center_size(screen.center(), Vec2::new(width, height));
        painter.rect_filled(panel, theme::corner(0.6 * rem), theme::PANEL);

        let inner = panel.shrink(padding);
        Text::line(&self.item.name, rem, theme::DIM, inner.width() - 1.6 * rem)
            .paint(painter, inner.min + Vec2::new(0.8 * rem, 0.2 * rem));

        let mut clicked = None;
        for (index, (label, action)) in self.entries.iter().enumerate() {
            let rect = Rect::from_min_size(
                Pos2::new(inner.left(), inner.top() + 2.4 * rem + index as f32 * row),
                Vec2::new(inner.width(), row),
            );
            if click.is_some_and(|position| rect.contains(position)) {
                self.focus = index;
                clicked = Some(*action);
            }
            let focused = index == self.focus;
            if focused {
                painter.rect_filled(rect, theme::corner(0.35 * rem), theme::TEXT);
            }
            let color = if focused { theme::BACKGROUND } else { theme::TEXT };
            let text = Text::line(label, 1.15 * rem, color, rect.width() - 1.6 * rem);
            let text_height = text.layout(painter).size().y;
            text.paint(painter, Pos2::new(rect.left() + 0.8 * rem, rect.center().y - text_height / 2.0));
        }
        clicked
    }
}

/// Code to enter on the iPhone to pair it as a remote.
pub fn pairing_code(ctx: &egui::Context, code: u32) {
    let screen = ctx.content_rect();
    let rem = theme::rem(screen);
    let layer = egui::LayerId::new(egui::Order::Foreground, egui::Id::unique("pairing"));
    let painter = ctx.layer_painter(layer);
    painter.rect_filled(screen, 0.0, Color32::from_black_alpha(170));
    let panel = Rect::from_center_size(screen.center(), Vec2::new(26.0, 15.0) * rem);
    painter.rect_filled(panel, theme::corner(0.6 * rem), theme::PANEL);
    let lines = [
        (t("Pair the remote", "Jumelage de la télécommande"), rem, theme::DIM, egui::FontFamily::Proportional),
        (&*format!("{code:04}"), 4.0 * rem, theme::TEXT, theme::semibold()),
        (
            t("Enter this code on the iPhone.", "Saisissez ce code sur l'iPhone."),
            rem,
            theme::DIM,
            egui::FontFamily::Proportional,
        ),
    ];
    let mut y = panel.top() + 2.0 * rem;
    for (text, size, color, family) in lines {
        let text = Text { family, centered: true, ..Text::line(text, size, color, panel.width() - 2.0 * rem) };
        y += text.paint(&painter, Pos2::new(panel.center().x, y)) + 0.8 * rem;
    }
}

/// Short message at the bottom of the screen.
pub fn toast(ctx: &egui::Context, message: &str) {
    let screen = ctx.content_rect();
    let rem = theme::rem(screen);
    let layer = egui::LayerId::new(egui::Order::Foreground, egui::Id::unique("toast"));
    let painter = ctx.layer_painter(layer);
    let text = Text::line(message, rem, theme::BACKGROUND, screen.width() * 0.7).layout(&painter);
    let size = text.size() + Vec2::new(2.4, 1.2) * rem;
    let rect = Rect::from_center_size(Pos2::new(screen.center().x, screen.bottom() - 5.0 * rem), size);
    painter.rect_filled(rect, theme::corner(0.35 * rem), theme::TEXT);
    painter.galley(rect.center() - text.size() / 2.0, text, theme::BACKGROUND);
}
