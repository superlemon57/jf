//! Interface de lecture dessinée par-dessus la vidéo : titre, commandes centrales,
//! barre de progression, bouton « Passer l'intro » et compte à rebours de l'épisode suivant.

use std::f32::consts::PI;
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, PointerButton, Pos2, Rect, Shape, Stroke, Vec2};

use super::browse::{self, clock};
use super::theme::{self, Text};
use crate::images::Images;
use crate::jellyfin::Item;
use crate::player::{Prompt, COUNTDOWN};

/// Délai sans activité avant que l'interface ne s'efface.
const IDLE: Duration = Duration::from_secs(3);
// Longueurs en rem (voir `theme::rem`).
const GUTTER: f32 = 3.0;
const BAR_FROM_BOTTOM: f32 = 4.2;
const SEEK_STEP: f64 = 10.0;

/// Vrai si la souris a bougé depuis l'image précédente. La vitesse calculée par egui
/// (`is_moving`) reste nulle pour un déplacement bref : on lit les événements eux-mêmes.
pub fn pointer_moved(input: &egui::InputState) -> bool {
    input.events.iter().any(|event| matches!(event, egui::Event::PointerMoved(_)))
}

pub enum Action {
    TogglePause,
    SeekTo(f64),
    SeekBy(f64),
    Accept,
    Dismiss,
    ToggleFullscreen,
}

/// Ce que montre l'interface, lu dans le lecteur à chaque image.
pub struct View<'a> {
    pub item: &'a Item,
    pub position: f64,
    pub duration: f64,
    pub paused: bool,
    pub live: bool,
    /// Fin de la partie déjà téléchargée, en secondes.
    pub cache: Option<f64>,
    pub prompt: Option<Prompt<'a>>,
}

#[derive(Clone, Copy, PartialEq)]
enum Target {
    Bar,
    Back,
    PlayPause,
    Forward,
    Accept,
    Dismiss,
    Video,
}

pub struct Osd {
    activity: Instant,
    alpha: f32,
    /// Position choisie en glissant sur la barre, tant que le bouton est enfoncé.
    scrub: Option<f64>,
    /// Élément sous le pointeur au moment de l'appui.
    pressed: Option<Target>,
}

pub struct Painted {
    pub actions: Vec<Action>,
    pub animating: bool,
}

fn white(alpha: f32) -> Color32 {
    Color32::from_white_alpha((alpha.clamp(0.0, 1.0) * 255.0) as u8)
}

/// Rectangle d'un bouton, et l'élément qu'il déclenche.
struct Hit {
    rect: Rect,
    target: Target,
}

struct Layout {
    rem: f32,
    bar: Rect,
    /// Zone sensible de la barre, plus haute que la barre elle-même.
    bar_zone: Rect,
    controls: Vec<Hit>,
}

impl Default for Osd {
    fn default() -> Self {
        Self { activity: Instant::now(), alpha: 1.0, scrub: None, pressed: None }
    }
}

impl Osd {
    /// Montre l'interface (touche pressée, souris déplacée, saut…).
    pub fn poke(&mut self) {
        self.activity = Instant::now();
    }

    fn layout(screen: Rect, live: bool) -> Layout {
        let rem = theme::rem(screen);
        let y = screen.bottom() - BAR_FROM_BOTTOM * rem;
        let bar = Rect::from_min_max(
            Pos2::new(screen.left() + GUTTER * rem, y - 0.14 * rem),
            Pos2::new(screen.right() - GUTTER * rem, y + 0.14 * rem),
        );
        let bar_zone = bar.expand2(Vec2::new(0.6 * rem, 0.9 * rem));
        let center = screen.center();
        let button = |offset: f32, target| Hit {
            rect: Rect::from_center_size(center + Vec2::new(offset * rem, 0.0), Vec2::splat(5.5 * rem)),
            target,
        };
        let controls = if live {
            vec![button(0.0, Target::PlayPause)]
        } else {
            vec![button(-9.0, Target::Back), button(0.0, Target::PlayPause), button(9.0, Target::Forward)]
        };
        Layout { rem, bar, bar_zone, controls }
    }

    fn time_at(bar: Rect, x: f32, duration: f64) -> f64 {
        (((x - bar.left()) / bar.width()).clamp(0.0, 1.0) as f64) * duration
    }

    pub fn paint(&mut self, ui: &egui::Ui, view: View, images: &mut Images) -> Painted {
        let ctx = ui.ctx();
        let painter = ui.painter();
        let screen = ui.max_rect();
        let layout = Self::layout(screen, view.live);
        let rem = layout.rem;
        let mut actions = Vec::new();

        let (hover, pressed, released, double, moving, dt) = ctx.input(|input| {
            let pointer = &input.pointer;
            (
                pointer.hover_pos(),
                pointer.primary_pressed(),
                pointer.primary_released(),
                pointer.button_double_clicked(PointerButton::Primary),
                pointer_moved(input),
                input.stable_dt.min(0.05),
            )
        });
        if moving || pressed {
            self.poke();
        }

        let visible = view.paused || self.scrub.is_some() || self.activity.elapsed() < IDLE;
        // Fondu vers l'opacité visée, sans la dépasser ni osciller une fois atteinte.
        let target_alpha = if visible { 1.0 } else { 0.0 };
        let step = dt / 0.2;
        self.alpha += (target_alpha - self.alpha).clamp(-step, step);
        let shown = self.alpha > 0.0;

        // Boutons actifs : ceux de l'interface quand elle est visible, la proposition toujours.
        let mut hits: Vec<Hit> = Vec::new();
        if self.alpha > 0.5 {
            if !view.live && view.duration > 0.0 {
                hits.push(Hit { rect: layout.bar_zone, target: Target::Bar });
            }
            hits.extend(layout.controls.iter().map(|hit| Hit { rect: hit.rect, target: hit.target }));
        }
        let prompt_hits = prompt_layout(screen, rem, view.prompt.as_ref(), painter);
        hits.extend(prompt_hits.iter().map(|hit| Hit { rect: hit.rect, target: hit.target }));
        let hit_at = |position: Pos2| hits.iter().find(|hit| hit.rect.contains(position)).map(|hit| hit.target);
        let hovered = hover.and_then(hit_at);

        // Clics : l'action part au relâchement, sur l'élément où l'appui a commencé.
        if pressed {
            self.pressed = Some(hover.and_then(hit_at).unwrap_or(Target::Video));
        }
        if let (Some(Target::Bar), Some(position)) = (self.pressed, hover) {
            self.scrub = Some(Self::time_at(layout.bar, position.x, view.duration));
        }
        if released {
            let target = self.pressed.take();
            let same = target.is_some() && target == hovered;
            match target {
                Some(Target::Bar) => {
                    if let Some(time) = self.scrub {
                        actions.push(Action::SeekTo(time));
                    }
                }
                Some(Target::Back) if same => actions.push(Action::SeekBy(-SEEK_STEP)),
                Some(Target::Forward) if same => actions.push(Action::SeekBy(SEEK_STEP)),
                Some(Target::PlayPause) if same => actions.push(Action::TogglePause),
                Some(Target::Accept) if same => actions.push(Action::Accept),
                Some(Target::Dismiss) if same => actions.push(Action::Dismiss),
                Some(Target::Video) if hovered.is_none() => actions.push(Action::TogglePause),
                _ => {}
            }
            self.scrub = None;
        }
        // Le double clic a déjà mis en pause puis relancé : il ne reste qu'à basculer le plein écran.
        if double && hovered.is_none() {
            actions.push(Action::ToggleFullscreen);
        }

        if shown {
            let alpha = self.alpha;
            let shown_position = self.scrub.unwrap_or(view.position);
            paint_shades(painter, screen, rem, alpha);
            paint_title(painter, screen, rem, view.item, view.live, alpha);
            for hit in &layout.controls {
                let active = hovered == Some(hit.target);
                let color = white(alpha * if active { 1.0 } else { 0.85 });
                if active {
                    painter.circle_filled(hit.rect.center(), 2.6 * rem, white(alpha * 0.12));
                }
                match hit.target {
                    Target::PlayPause if view.paused => play_icon(painter, hit.rect.center(), 2.8 * rem, color),
                    Target::PlayPause => pause_icon(painter, hit.rect.center(), 2.6 * rem, color),
                    Target::Back => replay_icon(painter, hit.rect.center(), 1.25 * rem, false, color),
                    _ => replay_icon(painter, hit.rect.center(), 1.25 * rem, true, color),
                }
            }
            if view.live {
                paint_live(painter, layout.bar, rem, alpha);
            } else if view.duration > 0.0 {
                let bar_hovered = hovered == Some(Target::Bar) || self.scrub.is_some();
                paint_bar(painter, &layout, &view, shown_position, bar_hovered, alpha);
                if bar_hovered {
                    if let Some(position) = hover {
                        let time = self.scrub.unwrap_or_else(|| Self::time_at(layout.bar, position.x, view.duration));
                        paint_bubble(painter, &layout, position.x, &clock(time), alpha);
                    }
                }
            }
        }

        let animating = paint_prompt(painter, screen, rem, view.prompt.as_ref(), &prompt_hits, hovered, images);
        Painted { actions, animating: animating || (self.alpha > 0.0 && self.alpha < 1.0) || visible }
    }
}

// ------------------------------------------------------------------ éléments

fn paint_shades(painter: &egui::Painter, screen: Rect, rem: f32, alpha: f32) {
    let top = Rect::from_min_max(screen.min, Pos2::new(screen.right(), screen.top() + 11.0 * rem));
    theme::fade(painter, top, false, &[(0.0, 0.7 * alpha), (1.0, 0.0)]);
    let bottom = Rect::from_min_max(Pos2::new(screen.left(), screen.bottom() - 13.0 * rem), screen.max);
    theme::fade(painter, bottom, false, &[(0.0, 0.0), (1.0, 0.85 * alpha)]);
}

fn paint_title(painter: &egui::Painter, screen: Rect, rem: f32, item: &Item, live: bool, alpha: f32) {
    let (title, detail) = match (item.kind.as_str(), &item.series_name) {
        ("Episode", Some(series)) => {
            let detail: Vec<String> = [item.episode_code(), Some(item.name.clone())].into_iter().flatten().collect();
            (series.clone(), detail.join(" · "))
        }
        _ if live => (item.name.clone(), "En direct".to_owned()),
        _ => (item.name.clone(), item.production_year.map(|year| year.to_string()).unwrap_or_default()),
    };
    let width = screen.width() * 0.6;
    let mut position = screen.min + Vec2::new(GUTTER, 2.4) * rem;
    let heading = Text { family: theme::semibold(), ..Text::line(&title, 1.7 * rem, white(alpha), width) };
    position.y += heading.paint(painter, position) + 0.2 * rem;
    Text::line(&detail, 1.05 * rem, theme::DIM.gamma_multiply(alpha), width).paint(painter, position);
}

fn paint_bar(painter: &egui::Painter, layout: &Layout, view: &View, position: f64, hovered: bool, alpha: f32) {
    let rem = layout.rem;
    let mut bar = layout.bar;
    if hovered {
        bar = bar.expand2(Vec2::new(0.0, 0.1 * rem));
    }
    let x_of = |time: f64| bar.left() + bar.width() * (time / view.duration).clamp(0.0, 1.0) as f32;
    let radius = theme::corner(bar.height() / 2.0);
    painter.rect_filled(bar, radius, white(alpha * 0.25));
    if let Some(cache) = view.cache.filter(|cache| *cache > position) {
        let cached = Rect::from_min_max(Pos2::new(x_of(position), bar.top()), Pos2::new(x_of(cache), bar.bottom()));
        painter.rect_filled(cached, radius, white(alpha * 0.4));
    }
    let played = Rect::from_min_max(bar.min, Pos2::new(x_of(position), bar.bottom()));
    painter.rect_filled(played, radius, white(alpha));
    let knob = if hovered { 0.65 } else { 0.45 };
    painter.circle_filled(Pos2::new(x_of(position), bar.center().y), knob * rem, white(alpha));

    let y = layout.bar.bottom() + 0.9 * rem;
    let size = 1.05 * rem;
    Text::line(&clock(position), size, white(alpha), 10.0 * rem).paint(painter, Pos2::new(layout.bar.left(), y));
    let remaining = format!("-{}", clock(view.duration - position));
    let text = Text::line(&remaining, size, theme::DIM.gamma_multiply(alpha), 10.0 * rem);
    let width = text.layout(painter).size().x;
    text.paint(painter, Pos2::new(layout.bar.right() - width, y));
}

/// Temps visé, affiché au-dessus de la barre sous le pointeur.
fn paint_bubble(painter: &egui::Painter, layout: &Layout, x: f32, label: &str, alpha: f32) {
    let rem = layout.rem;
    let text = Text::line(label, 0.95 * rem, theme::BACKGROUND, 8.0 * rem).layout(painter);
    let size = text.size() + Vec2::new(1.2, 0.5) * rem;
    let x = x.clamp(layout.bar.left() + size.x / 2.0, layout.bar.right() - size.x / 2.0);
    let rect = Rect::from_center_size(Pos2::new(x, layout.bar.top() - 1.4 * rem), size);
    painter.rect_filled(rect, theme::corner(0.3 * rem), white(alpha));
    painter.galley(rect.center() - text.size() / 2.0, text, theme::BACKGROUND);
}

fn paint_live(painter: &egui::Painter, bar: Rect, rem: f32, alpha: f32) {
    let text = Text { family: theme::semibold(), ..Text::line("EN DIRECT", 0.9 * rem, white(alpha), 10.0 * rem) };
    let galley = text.layout(painter);
    let rect = Rect::from_min_size(
        Pos2::new(bar.left(), bar.center().y - 0.8 * rem),
        Vec2::new(galley.size().x + 2.4 * rem, 1.6 * rem),
    );
    painter.rect_filled(rect, theme::corner(0.3 * rem), Color32::from_black_alpha((150.0 * alpha) as u8));
    let dot = Pos2::new(rect.left() + 0.8 * rem, rect.center().y);
    painter.circle_filled(dot, 0.25 * rem, Color32::from_rgb(229, 57, 53).gamma_multiply(alpha));
    painter.galley(Pos2::new(rect.left() + 1.4 * rem, rect.center().y - galley.size().y / 2.0), galley, theme::TEXT);
}

// ------------------------------------------------------------------ icônes (dessinées : aucune police requise)

fn play_icon(painter: &egui::Painter, center: Pos2, height: f32, color: Color32) {
    let half = height / 2.0;
    let points = vec![
        center + Vec2::new(-0.38 * height, -half),
        center + Vec2::new(-0.38 * height, half),
        center + Vec2::new(0.5 * height, 0.0),
    ];
    painter.add(Shape::convex_polygon(points, color, Stroke::NONE));
}

fn pause_icon(painter: &egui::Painter, center: Pos2, height: f32, color: Color32) {
    let (width, gap) = (0.26 * height, 0.24 * height);
    for side in [-1.0, 1.0] {
        let x = center.x + side * (gap + width) / 2.0;
        let rect = Rect::from_center_size(Pos2::new(x, center.y), Vec2::new(width, height));
        painter.rect_filled(rect, theme::corner(0.05 * height), color);
    }
}

/// Flèche circulaire avec « 10 » au centre ; `forward` la fait tourner dans le sens horaire.
fn replay_icon(painter: &egui::Painter, center: Pos2, radius: f32, forward: bool, color: Color32) {
    let mirror = if forward { 1.0 } else { -1.0 };
    let point = |angle: f32| center + Vec2::new(mirror * radius * angle.cos(), radius * angle.sin());
    let (from, to) = (-PI / 3.0, 1.5 * PI);
    let steps = 40;
    let arc: Vec<Pos2> = (0..=steps).map(|step| point(from + (to - from) * step as f32 / steps as f32)).collect();
    painter.add(Shape::line(arc, Stroke::new(0.13 * radius * 1.2, color)));
    // Pointe de flèche à l'extrémité, au sommet du cercle.
    let tip = point(to);
    let size = 0.42 * radius;
    let head = vec![
        tip + Vec2::new(mirror * size * 0.9, 0.0),
        tip + Vec2::new(-mirror * size * 0.3, -size * 0.6),
        tip + Vec2::new(-mirror * size * 0.3, size * 0.6),
    ];
    painter.add(Shape::convex_polygon(head, color, Stroke::NONE));
    let label = Text {
        family: theme::semibold(),
        centered: true,
        ..Text::line("10", 0.62 * radius * 1.4, color, 4.0 * radius)
    };
    let height = label.layout(painter).size().y;
    label.paint(painter, Pos2::new(center.x, center.y - height / 2.0));
}

// ------------------------------------------------------------------ propositions

const CARD_WIDTH: f32 = 26.0;
const THUMB_WIDTH: f32 = 10.5;
const BUTTON_HEIGHT: f32 = 2.8;

/// Rectangles des boutons de la proposition, calculés avant le dessin pour le clic.
fn prompt_layout(screen: Rect, rem: f32, prompt: Option<&Prompt>, painter: &egui::Painter) -> Vec<Hit> {
    let right = screen.right() - GUTTER * rem;
    let bottom = screen.bottom() - 7.6 * rem;
    match prompt {
        None => Vec::new(),
        Some(Prompt::Skip { label, .. }) => {
            let text =
                Text { family: theme::semibold(), ..Text::line(label, 1.1 * rem, theme::BACKGROUND, 20.0 * rem) };
            let size = text.layout(painter).size() + Vec2::new(2.8, 1.6) * rem;
            let rect = Rect::from_min_max(Pos2::new(right - size.x, bottom - size.y), Pos2::new(right, bottom));
            vec![Hit { rect, target: Target::Accept }]
        }
        Some(Prompt::NextUp { .. }) => {
            let card = card_rect(right, bottom, rem);
            let inner = card.shrink(rem);
            let half = (inner.width() - 0.6 * rem) / 2.0;
            let top = inner.bottom() - BUTTON_HEIGHT * rem;
            let accept = Rect::from_min_size(Pos2::new(inner.left(), top), Vec2::new(half, BUTTON_HEIGHT * rem));
            let dismiss =
                Rect::from_min_size(Pos2::new(inner.right() - half, top), Vec2::new(half, BUTTON_HEIGHT * rem));
            vec![Hit { rect: accept, target: Target::Accept }, Hit { rect: dismiss, target: Target::Dismiss }]
        }
    }
}

fn card_rect(right: f32, bottom: f32, rem: f32) -> Rect {
    let thumb_height = THUMB_WIDTH * 9.0 / 16.0;
    let height = 1.0 + thumb_height + 0.9 + BUTTON_HEIGHT + 1.0;
    Rect::from_min_max(Pos2::new(right - CARD_WIDTH * rem, bottom - height * rem), Pos2::new(right, bottom))
}

/// Bouton plein (validé par Entrée) ou discret (Échap).
fn button(painter: &egui::Painter, rect: Rect, rem: f32, label: &str, primary: bool, hovered: bool) {
    let (fill, text) = match (primary, hovered) {
        (true, false) => (theme::TEXT, theme::BACKGROUND),
        (true, true) => (Color32::WHITE, theme::BACKGROUND),
        (false, false) => (white(0.12), theme::TEXT),
        (false, true) => (white(0.22), theme::TEXT),
    };
    painter.rect_filled(rect, theme::corner(0.35 * rem), fill);
    let label = Text { family: theme::semibold(), centered: true, ..Text::line(label, rem, text, rect.width() - rem) };
    let height = label.layout(painter).size().y;
    label.paint(painter, Pos2::new(rect.center().x, rect.center().y - height / 2.0));
}

fn paint_prompt(
    painter: &egui::Painter,
    screen: Rect,
    rem: f32,
    prompt: Option<&Prompt>,
    hits: &[Hit],
    hovered: Option<Target>,
    images: &mut Images,
) -> bool {
    let is = |target| hovered == Some(target);
    match prompt {
        None => false,
        Some(Prompt::Skip { label, .. }) => {
            let rect = hits[0].rect;
            painter.rect_filled(
                rect.translate(Vec2::new(0.0, 0.15 * rem)),
                theme::corner(0.35 * rem),
                Color32::from_black_alpha(90),
            );
            button(painter, rect, 1.1 * rem, label, true, is(Target::Accept));
            false
        }
        Some(Prompt::NextUp { item, remaining }) => {
            let right = screen.right() - GUTTER * rem;
            let card = card_rect(right, screen.bottom() - 7.6 * rem, rem);
            painter.rect_filled(card, theme::corner(0.6 * rem), Color32::from_rgba_unmultiplied(20, 20, 22, 240));
            let inner = card.shrink(rem);

            let thumb = Rect::from_min_size(inner.min, Vec2::new(THUMB_WIDTH, THUMB_WIDTH * 9.0 / 16.0) * rem);
            painter.rect_filled(thumb, theme::corner(0.35 * rem), theme::CARD);
            let mut animating = false;
            if let Some(loaded) = browse::still(item).and_then(|image| images.get(&image)) {
                let appear = (loaded.age / 0.25).min(1.0);
                animating = appear < 1.0;
                theme::image(painter, thumb, loaded.texture, 0.35 * rem, appear, 0.5, false);
            }

            let text_left = thumb.right() + rem;
            let width = inner.right() - text_left;
            let mut y = thumb.top();
            y += Text::line("Épisode suivant", 0.85 * rem, theme::DIM, width).paint(painter, Pos2::new(text_left, y));
            y += 0.2 * rem;
            let name = match item.index_number {
                Some(number) => format!("{number}. {}", item.name),
                None => item.name.clone(),
            };
            let title =
                Text { family: theme::semibold(), max_rows: 2, ..Text::line(&name, 1.05 * rem, theme::TEXT, width) };
            y += title.paint(painter, Pos2::new(text_left, y)) + 0.2 * rem;
            if let Some(code) = item.episode_code() {
                Text::line(&code, 0.85 * rem, theme::DIM, width).paint(painter, Pos2::new(text_left, y));
            }

            let seconds = remaining.ceil().max(1.0) as i64;
            let (accept, dismiss) = (hits[0].rect, hits[1].rect);
            button(painter, accept, rem, &format!("Lecture dans {seconds} s"), true, is(Target::Accept));
            // Le compte à rebours se lit aussi comme une barre qui se remplit dans le bouton.
            let progress = (1.0 - remaining / COUNTDOWN).clamp(0.0, 1.0) as f32;
            let track = Rect::from_min_max(Pos2::new(accept.left(), accept.bottom() - 0.22 * rem), accept.max);
            let filled =
                Rect::from_min_max(track.min, Pos2::new(track.left() + track.width() * progress, track.bottom()));
            let clipped = painter.with_clip_rect(filled.intersect(painter.clip_rect()));
            clipped.rect_filled(accept, theme::corner(0.35 * rem), Color32::from_black_alpha(110));
            button(painter, dismiss, rem, "Voir le générique", false, is(Target::Dismiss));
            animating
        }
    }
}
