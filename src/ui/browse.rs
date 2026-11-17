//! Écran de navigation : bandeau d'information, image de fond et grilles de cartes.

use std::time::Instant;

use eframe::egui::{self, Color32, Pos2, Rect, Stroke, StrokeKind, Vec2};

use super::theme::{self, Text};
use crate::images::{ImageRef, Images};
use crate::jellyfin::{Client, Item};

// Longueurs en rem (voir `theme::rem`).
const GUTTER: f32 = 4.0;
const INFO_HEIGHT: f32 = 14.0;
const BACKDROP_HEIGHT: f32 = 24.0;
const GAP_X: f32 = 1.0;
const GAP_Y: f32 = 1.4;
const NAME_SIZE: f32 = 0.95;
const SUB_SIZE: f32 = 0.8;
const CAPTION_HEIGHT: f32 = 0.5 + (NAME_SIZE + SUB_SIZE) * 1.3;
const RADIUS: f32 = 0.35;

#[derive(Clone, Copy, PartialEq)]
pub enum Shape {
    Poster,
    Wide,
    Square,
}

impl Shape {
    fn of(items: &[Item]) -> Self {
        let all = |kinds: &[&str]| items.iter().all(|item| kinds.contains(&item.kind.as_str()));
        if all(&["Episode"]) {
            Self::Wide
        } else if all(&["MusicAlbum", "Audio", "MusicArtist", "TvChannel"]) {
            Self::Square
        } else {
            Self::Poster
        }
    }

    fn width(self) -> f32 {
        match self {
            Self::Poster => 10.0,
            Self::Wide => 18.0,
            Self::Square => 11.0,
        }
    }

    fn ratio(self) -> f32 {
        match self {
            Self::Poster => 2.0 / 3.0,
            Self::Wide => 16.0 / 9.0,
            Self::Square => 1.0,
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
pub enum Layout {
    Row,
    Grid,
}

#[derive(Clone, Copy, PartialEq)]
pub enum Direction {
    Up,
    Down,
    Left,
    Right,
}

pub struct Section {
    pub title: String,
    pub items: Vec<Item>,
    pub layout: Layout,
    shape: Shape,
    /// La liste mélange plusieurs séries : les épisodes doivent nommer la leur.
    pub mixed: bool,
    scroll: f32,
}

impl Section {
    fn new(title: impl Into<String>, items: Vec<Item>, layout: Layout, shape: Option<Shape>, mixed: bool) -> Self {
        let shape = shape.unwrap_or_else(|| Shape::of(&items));
        Self { title: title.into(), items, layout, shape, mixed, scroll: 0.0 }
    }
}

#[derive(Clone)]
pub enum Source {
    Home,
    Children(Box<Item>),
    Search(String),
}

impl Source {
    /// Requêtes réseau : à appeler hors du fil d'interface.
    pub fn fetch(&self, client: &Client) -> Result<Vec<Section>, String> {
        let sections = match self {
            Self::Home => {
                let (resume, next_up, views) = std::thread::scope(|scope| {
                    let resume = scope.spawn(|| client.resume());
                    let next_up = scope.spawn(|| client.next_up());
                    (resume.join().unwrap(), next_up.join().unwrap(), client.views())
                });
                let wide = Some(Shape::Wide);
                vec![
                    Section::new("Reprendre", resume.unwrap_or_default(), Layout::Row, wide, true),
                    Section::new("À suivre", next_up.unwrap_or_default(), Layout::Row, wide, true),
                    Section::new("Bibliothèques", views.map_err(|e| e.to_string())?, Layout::Row, wide, false),
                ]
            }
            Self::Children(parent) => {
                let items = client.children(parent).map_err(|e| e.to_string())?;
                let title = match (&parent.series_name, parent.kind.as_str()) {
                    (Some(series), "Season") => format!("{series} — {}", parent.name),
                    _ => parent.name.clone(),
                };
                vec![Section::new(title, items, Layout::Grid, None, false)]
            }
            Self::Search(query) => {
                let items = client.search(query).map_err(|e| e.to_string())?;
                vec![Section::new(format!("Résultats pour « {query} »"), items, Layout::Grid, None, true)]
            }
        };
        Ok(sections.into_iter().filter(|section| !section.items.is_empty()).collect())
    }
}

pub struct Page {
    pub id: u64,
    pub source: Source,
    pub sections: Vec<Section>,
    /// (section, carte)
    pub focus: (usize, usize),
    pub loading: bool,
    /// Les données ont pu changer (lecture, « vu ») depuis le chargement.
    pub stale: bool,
    pub error: Option<String>,
    scroll: f32,
    /// Prochain dessin sans animation de défilement.
    snap: bool,
}

/// Position verticale d'une section dans le contenu, et taille de ses cartes.
struct Geometry {
    title_y: f32,
    cards_y: f32,
    card: Vec2,
    thumb_height: f32,
    columns: usize,
}

impl Geometry {
    fn pitch(&self, rem: f32) -> Vec2 {
        self.card + Vec2::new(GAP_X, GAP_Y) * rem
    }
}

fn geometry(sections: &[Section], width: f32, rem: f32) -> (Vec<Geometry>, f32) {
    let mut y = 0.0;
    let mut result = Vec::with_capacity(sections.len());
    for section in sections {
        let title_y = y + 1.2 * rem;
        let cards_y = title_y + (1.05 * 1.3 + 0.8) * rem;
        let wanted = section.shape.width() * rem;
        let gap = GAP_X * rem;
        let (columns, card_width) = match section.layout {
            Layout::Row => (section.items.len().max(1), wanted),
            Layout::Grid => {
                let columns = (((width + gap) / (wanted + gap)).floor() as usize).max(1);
                (columns, (width - gap * (columns - 1) as f32) / columns as f32)
            }
        };
        let thumb_height = card_width / section.shape.ratio();
        let card = Vec2::new(card_width, thumb_height + CAPTION_HEIGHT * rem);
        let rows = section.items.len().div_ceil(columns).max(1);
        y = cards_y + rows as f32 * card.y + (rows - 1) as f32 * GAP_Y * rem + 0.6 * rem;
        result.push(Geometry { title_y, cards_y, card, thumb_height, columns });
    }
    (result, y + 5.0 * rem)
}

impl Page {
    pub fn new(id: u64, source: Source) -> Self {
        Self {
            id,
            source,
            sections: Vec::new(),
            focus: (0, 0),
            loading: true,
            stale: false,
            error: None,
            scroll: 0.0,
            snap: true,
        }
    }

    pub fn focused(&self) -> Option<&Item> {
        self.sections.get(self.focus.0)?.items.get(self.focus.1)
    }

    pub fn focused_mut(&mut self) -> Option<&mut Item> {
        self.sections.get_mut(self.focus.0)?.items.get_mut(self.focus.1)
    }

    /// Remplace le contenu en gardant la sélection sur le même élément si possible.
    pub fn set_sections(&mut self, mut sections: Vec<Section>) {
        let previous = self.focused().map(|item| item.id.clone());
        for (section, old) in sections.iter_mut().zip(&self.sections) {
            section.scroll = old.scroll;
        }
        self.snap |= self.sections.is_empty();
        self.sections = sections;
        if let Some(id) = previous {
            let position = |section: &Section| section.items.iter().position(|item| item.id == id);
            let same = self.sections.get(self.focus.0).and_then(position).map(|index| (self.focus.0, index));
            let elsewhere = || {
                let mut sections = self.sections.iter().enumerate();
                sections.find_map(|(index, section)| Some((index, position(section)?)))
            };
            if let Some(found) = same.or_else(elsewhere) {
                self.focus = found;
            }
        }
        let section = self.focus.0.min(self.sections.len().saturating_sub(1));
        let count = self.sections.get(section).map_or(0, |section| section.items.len());
        self.focus = (section, self.focus.1.min(count.saturating_sub(1)));
    }

    pub fn move_focus(&mut self, direction: Direction, screen: Rect) {
        let rem = theme::rem(screen);
        let width = screen.width() - 2.0 * GUTTER * rem;
        let (geometries, _) = geometry(&self.sections, width, rem);
        let (section_index, index) = self.focus;
        let Some(section) = self.sections.get(section_index) else { return };
        let geo = &geometries[section_index];
        let count = section.items.len();

        let down = match direction {
            Direction::Left => {
                self.focus.1 = index.saturating_sub(1);
                return;
            }
            Direction::Right => {
                self.focus.1 = (index + 1).min(count - 1);
                return;
            }
            Direction::Down => true,
            Direction::Up => false,
        };
        if section.layout == Layout::Grid {
            let columns = geo.columns;
            if down && index + columns < count {
                self.focus.1 = index + columns;
                return;
            }
            if !down && index >= columns {
                self.focus.1 = index - columns;
                return;
            }
            // Dernière ligne incomplète : descendre quand même sur sa dernière carte.
            if down && index / columns < (count - 1) / columns {
                self.focus.1 = count - 1;
                return;
            }
        }

        // Sinon passer à la section voisine, sur la carte la plus proche horizontalement.
        let target = if down { section_index + 1 } else { section_index.wrapping_sub(1) };
        let Some(next) = self.sections.get(target) else { return };
        let x_of = |section: &Section, geo: &Geometry, index: usize| match section.layout {
            Layout::Row => index as f32 * geo.pitch(rem).x - section.scroll,
            Layout::Grid => (index % geo.columns) as f32 * geo.pitch(rem).x,
        };
        let x = x_of(section, geo, index);
        let next_geo = &geometries[target];
        let candidates = match next.layout {
            Layout::Row => 0..next.items.len(),
            Layout::Grid if down => 0..next_geo.columns.min(next.items.len()),
            Layout::Grid => (next.items.len() - 1) / next_geo.columns * next_geo.columns..next.items.len(),
        };
        let nearest = candidates.min_by(|a, b| {
            let distance = |index: &usize| (x_of(next, next_geo, *index) - x).abs();
            distance(a).total_cmp(&distance(b))
        });
        self.focus = (target, nearest.unwrap_or(0));
    }
}

// ------------------------------------------------------------------ contenu des cartes

fn runtime(item: &Item) -> Option<String> {
    let minutes = (item.runtime_seconds() / 60.0) as i64;
    match minutes {
        0 => None,
        1..=59 => Some(format!("{minutes} min")),
        _ => Some(format!("{} h {:02}", minutes / 60, minutes % 60)),
    }
}

pub fn clock(seconds: f64) -> String {
    let seconds = seconds.max(0.0) as i64;
    let (hours, minutes, seconds) = (seconds / 3600, seconds % 3600 / 60, seconds % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

fn caption(item: &Item, mixed: bool) -> (String, String) {
    match item.kind.as_str() {
        "Episode" if mixed => {
            let detail: Vec<String> = [item.episode_code(), Some(item.name.clone())].into_iter().flatten().collect();
            (item.series_name.clone().unwrap_or_default(), detail.join(" · "))
        }
        "Episode" => {
            let name = match item.index_number {
                Some(number) => format!("{number}. {}", item.name),
                None => item.name.clone(),
            };
            (name, runtime(item).unwrap_or_default())
        }
        "TvChannel" => {
            let program = item.current_program.as_ref().map(|program| program.name.clone());
            (item.name.clone(), program.or(item.channel_number.clone()).unwrap_or_default())
        }
        _ => (item.name.clone(), item.production_year.map(|year| year.to_string()).unwrap_or_default()),
    }
}

fn reference(id: &str, kind: &'static str, width: u32, tag: Option<&String>) -> Option<ImageRef> {
    Some(ImageRef { id: id.to_owned(), kind, width, tag: tag.cloned().unwrap_or_default() })
}

fn thumbnail(item: &Item, shape: Shape) -> Option<ImageRef> {
    let primary = item.image_tags.get("Primary");
    if shape == Shape::Wide {
        if item.kind != "Episode" && !item.backdrop_image_tags.is_empty() {
            return reference(&item.id, "Backdrop", 640, item.backdrop_image_tags.first());
        }
        if primary.is_some() {
            return reference(&item.id, "Primary", 640, primary);
        }
        return reference(item.parent_backdrop_item_id.as_ref()?, "Backdrop", 640, None);
    }
    if let (Some(series), Some(tag)) = (&item.series_id, &item.series_primary_image_tag) {
        return reference(series, "Primary", 400, Some(tag));
    }
    if primary.is_some() {
        return reference(&item.id, "Primary", 400, primary);
    }
    reference(item.album_id.as_ref()?, "Primary", 400, Some(item.album_primary_image_tag.as_ref()?))
}

fn backdrop_image(item: &Item) -> Option<ImageRef> {
    if !item.backdrop_image_tags.is_empty() {
        return reference(&item.id, "Backdrop", 1920, item.backdrop_image_tags.first());
    }
    // Épisodes et saisons : l'image de la série (le serveur répond 404 si elle n'en a pas).
    let parent = item.parent_backdrop_item_id.as_ref().or(item.series_id.as_ref())?;
    reference(parent, "Backdrop", 1920, None)
}

/// Vignette 16:9 d'un épisode, pour la carte « Épisode suivant ».
pub fn still(item: &Item) -> Option<ImageRef> {
    thumbnail(item, Shape::Wide)
}

/// Affiche de l'élément, pour l'écran de verrouillage de la télécommande.
pub fn artwork(item: &Item) -> Option<ImageRef> {
    thumbnail(item, Shape::Poster).map(|image| ImageRef { width: 600, ..image })
}

// ------------------------------------------------------------------ image de fond

/// Fond de l'élément sélectionné, avec fondu enchaîné entre deux images.
#[derive(Default)]
pub struct Backdrop {
    wanted: Option<ImageRef>,
    changed: Option<Instant>,
    current: Option<(ImageRef, f32)>,
    previous: Option<(ImageRef, f32)>,
}

impl Backdrop {
    /// Renvoie vrai tant qu'une animation est en cours.
    fn paint(
        &mut self,
        painter: &egui::Painter,
        screen: Rect,
        rem: f32,
        dt: f32,
        wanted: Option<ImageRef>,
        images: &mut Images,
    ) -> bool {
        if wanted != self.wanted {
            self.wanted = wanted;
            self.changed = Some(Instant::now());
        }
        let shown = self.current.as_ref().map(|(image, _)| image);
        let mut animating = false;
        if shown != self.wanted.as_ref() {
            // Délai : ne rien charger tant que la sélection défile.
            let settled = self.changed.is_none_or(|at| at.elapsed().as_millis() > 250);
            let ready = self.wanted.as_ref().is_none_or(|image| images.get(image).is_some());
            if settled && ready {
                self.previous = self.current.take();
                self.current = self.wanted.clone().map(|image| (image, 0.0));
            }
            animating = !settled;
        }

        let rect = Rect::from_min_max(
            Pos2::new(screen.left() + screen.width() * 0.35, screen.top()),
            Pos2::new(screen.right(), screen.top() + BACKDROP_HEIGHT * rem),
        );
        let step = dt / 0.4;
        if let Some((image, alpha)) = &mut self.previous {
            *alpha -= step;
            if let (true, Some(loaded)) = (*alpha > 0.0, images.get(image)) {
                theme::image(painter, rect, loaded.texture, 0.0, *alpha, 0.2, false);
                animating = true;
            } else {
                self.previous = None;
            }
        }
        if let Some((image, alpha)) = &mut self.current {
            if *alpha < 1.0 {
                *alpha = (*alpha + step).min(1.0);
                animating = true;
            }
            if let Some(loaded) = images.get(image) {
                theme::image(painter, rect, loaded.texture, 0.0, *alpha, 0.2, false);
            }
        }
        theme::fade(painter, rect, true, &[(0.0, 1.0), (0.3, 0.7), (0.75, 0.0), (1.0, 0.0)]);
        theme::fade(painter, rect, false, &[(0.0, 0.3), (0.25, 0.3), (0.92, 1.0), (1.0, 1.0)]);
        animating
    }
}

// ------------------------------------------------------------------ dessin

pub struct Frame<'a> {
    pub painter: &'a egui::Painter,
    pub screen: Rect,
    pub dt: f32,
    pub images: &'a mut Images,
    pub backdrop: &'a mut Backdrop,
    /// Position d'un clic à traiter, s'il y en a un.
    pub click: Option<Pos2>,
}

pub struct Painted {
    pub animating: bool,
    /// Carte cliquée : (section, carte).
    pub clicked: Option<(usize, usize)>,
}

fn paint_info(painter: &egui::Painter, screen: Rect, rem: f32, item: &Item) {
    let state = if item.user_data.played {
        Some("Vu".to_owned())
    } else if item.resume_seconds() > 0.0 {
        Some(format!("Reprise à {}", clock(item.resume_seconds())))
    } else {
        None
    };
    let meta: Vec<String> = match item.kind.as_str() {
        "Episode" => vec![item.series_name.clone(), item.episode_code(), runtime(item), state],
        "TvChannel" => vec![item.channel_number.as_ref().map(|number| format!("Chaîne {number}"))],
        _ => vec![item.production_year.map(|year| year.to_string()), runtime(item), state],
    }
    .into_iter()
    .flatten()
    .collect();
    let overview = match (&item.overview, &item.current_program) {
        (Some(overview), _) => overview.split_whitespace().collect::<Vec<_>>().join(" "),
        (None, Some(program)) => program.name.clone(),
        _ => String::new(),
    };

    let mut position = screen.min + Vec2::new(GUTTER, 3.0) * rem;
    let title =
        Text { family: theme::semibold(), ..Text::line(&item.name, 2.4 * rem, theme::TEXT, screen.width() * 0.6) };
    position.y += title.paint(painter, position) + 0.3 * rem;
    let meta = meta.join(" · ");
    position.y += Text::line(&meta, rem, theme::DIM, screen.width() * 0.6).paint(painter, position) + 0.7 * rem;
    Text { max_rows: 3, leading: 1.45, ..Text::line(&overview, rem, theme::TEXT, 44.0 * rem) }.paint(painter, position);
}

struct CardView<'a> {
    item: &'a Item,
    rect: Rect,
    thumb_height: f32,
    shape: Shape,
    mixed: bool,
    focused: bool,
    alpha: f32,
}

fn paint_card(painter: &egui::Painter, rem: f32, card: CardView, images: &mut Images) -> bool {
    let CardView { item, rect, alpha, .. } = card;
    let fade = |color: Color32| color.gamma_multiply(alpha);
    let thumb = Rect::from_min_size(rect.min, Vec2::new(rect.width(), card.thumb_height));
    let radius = RADIUS * rem;
    let (name, sub) = caption(item, card.mixed);
    let mut animating = false;

    painter.rect_filled(thumb, theme::corner(radius), fade(theme::CARD));
    match thumbnail(item, card.shape).and_then(|image| images.get(&image)) {
        Some(loaded) => {
            let appear = (loaded.age / 0.25).min(1.0);
            animating = appear < 1.0;
            if item.is_channel() {
                // Les logos de chaînes sont montrés entiers, avec une marge.
                theme::image(painter, thumb.shrink(1.2 * rem), loaded.texture, 0.0, alpha * appear, 0.5, true);
            } else {
                theme::image(painter, thumb, loaded.texture, radius, alpha * appear, 0.5, false);
            }
        }
        None => {
            let label = Text {
                max_rows: 3,
                centered: true,
                ..Text::line(&name, 0.9 * rem, fade(theme::DIM), thumb.width() - 1.6 * rem)
            };
            let height = label.layout(painter).size().y;
            label.paint(painter, Pos2::new(thumb.center().x, thumb.center().y - height / 2.0));
        }
    }

    // Pastille en haut à droite : coche si vu, sinon nombre d'épisodes restants.
    let count = item.user_data.unplayed_item_count.filter(|count| *count > 0 && !item.user_data.played);
    if item.user_data.played || count.is_some() {
        let label = count.map(|count| count.to_string()).unwrap_or_default();
        let text = Text::line(&label, 0.75 * rem, fade(theme::TEXT), 6.0 * rem).layout(painter);
        let size = Vec2::new((text.size().x + 0.8 * rem).max(1.5 * rem), 1.3 * rem);
        let pill = Rect::from_min_size(Pos2::new(thumb.right() - 0.4 * rem - size.x, thumb.top() + 0.4 * rem), size);
        painter.rect_filled(pill, theme::corner(size.y / 2.0), Color32::from_black_alpha((190.0 * alpha) as u8));
        if item.user_data.played {
            // Coche tracée à la main : toutes les polices n'ont pas ce symbole.
            let unit = 0.14 * rem;
            let points = [(-2.2, 0.1), (-0.7, 1.6), (2.3, -1.7)].map(|(x, y)| pill.center() + Vec2::new(x, y) * unit);
            painter.line(points.to_vec(), Stroke::new(0.12 * rem, fade(theme::TEXT)));
        } else {
            painter.galley(pill.center() - text.size() / 2.0, text, theme::TEXT);
        }
    }

    let (position, total) = (item.user_data.playback_position_ticks, item.run_time_ticks.unwrap_or(0));
    if !item.user_data.played && position > 0 && total > 0 {
        // La barre est la vignette elle-même, découpée à sa hauteur : elle épouse ainsi son arrondi.
        let width = thumb.width() * (position as f32 / total as f32).clamp(0.0, 1.0);
        let bar = Rect::from_min_max(
            Pos2::new(thumb.left(), thumb.bottom() - 0.25 * rem),
            Pos2::new(thumb.left() + width, thumb.bottom()),
        );
        let clipped = painter.with_clip_rect(bar.intersect(painter.clip_rect()));
        clipped.rect_filled(thumb, theme::corner(radius), fade(theme::TEXT));
    }

    if card.focused {
        let ring = thumb.expand(0.2 * rem);
        painter.rect_stroke(
            ring,
            theme::corner(radius + 0.2 * rem),
            Stroke::new(0.2 * rem, theme::TEXT),
            StrokeKind::Outside,
        );
    }

    let mut text = Pos2::new(rect.left(), thumb.bottom() + 0.5 * rem);
    text.y += Text::line(&name, NAME_SIZE * rem, fade(theme::TEXT), rect.width()).paint(painter, text);
    Text::line(&sub, SUB_SIZE * rem, fade(theme::DIM), rect.width()).paint(painter, text);
    animating
}

impl Page {
    pub fn paint(&mut self, frame: Frame) -> Painted {
        let Frame { painter, screen, dt, images, backdrop, click } = frame;
        let rem = theme::rem(screen);
        let mut painted = Painted { animating: false, clicked: None };

        painter.rect_filled(screen, 0.0, theme::BACKGROUND);
        let wanted = self.focused().and_then(backdrop_image);
        painted.animating |= backdrop.paint(painter, screen, rem, dt, wanted, images);
        if let Some(item) = self.focused() {
            paint_info(painter, screen, rem, item);
        }

        let content = Rect::from_min_max(Pos2::new(screen.left(), screen.top() + INFO_HEIGHT * rem), screen.max);
        let left = screen.left() + GUTTER * rem;
        if self.sections.is_empty() {
            let message = match (&self.error, self.loading) {
                (Some(error), _) => error.as_str(),
                (None, true) => "Chargement…",
                (None, false) => "Rien à afficher.",
            };
            Text::line(message, rem, theme::DIM, screen.width() * 0.8)
                .paint(painter, Pos2::new(left, content.top() + 2.0 * rem));
            return painted;
        }

        let width = screen.width() - 2.0 * GUTTER * rem;
        let (geometries, total) = geometry(&self.sections, width, rem);
        let ease = 1.0 - (-dt * 12.0).exp();
        let mut glide = |value: &mut f32, target: f32, snap: bool| {
            if snap || (target - *value).abs() < 0.5 {
                *value = target;
            } else {
                *value += (target - *value) * ease;
                painted.animating = true;
            }
        };

        // Garde la carte sélectionnée au centre, verticalement puis dans sa rangée.
        let (focus_section, focus_index) = self.focus;
        if let (Some(section), Some(geo)) = (self.sections.get_mut(focus_section), geometries.get(focus_section)) {
            let pitch = geo.pitch(rem);
            let row = if section.layout == Layout::Grid { focus_index / geo.columns } else { 0 };
            let center = geo.cards_y + row as f32 * pitch.y + geo.card.y / 2.0;
            let target = (center - content.height() / 2.0).clamp(0.0, (total - content.height()).max(0.0));
            glide(&mut self.scroll, target, self.snap);
            if section.layout == Layout::Row {
                let length = section.items.len() as f32 * pitch.x - GAP_X * rem;
                let center = focus_index as f32 * pitch.x + geo.card.x / 2.0;
                let target = (center - width / 2.0).clamp(0.0, (length - width).max(0.0));
                glide(&mut section.scroll, target, self.snap);
            }
        }
        self.snap = false;

        let clipped = painter.with_clip_rect(content);
        for (section_index, (section, geo)) in self.sections.iter().zip(&geometries).enumerate() {
            let pitch = geo.pitch(rem);
            let top = content.top() - self.scroll;
            let title = Pos2::new(left, top + geo.title_y);
            if title.y < content.bottom() && title.y + 2.0 * rem > content.top() {
                Text::line(&section.title, 1.05 * rem, theme::DIM, width).paint(&clipped, title);
            }

            // Seules les cartes visibles sont dessinées.
            let visible = match section.layout {
                Layout::Row => {
                    let first = ((section.scroll - GUTTER * rem) / pitch.x).floor().max(0.0) as usize;
                    let count = (screen.width() / pitch.x).ceil() as usize + 2;
                    first..(first + count).min(section.items.len())
                }
                Layout::Grid => {
                    let first = ((self.scroll - geo.cards_y) / pitch.y).floor().max(0.0) as usize;
                    let rows = (content.height() / pitch.y).ceil() as usize + 2;
                    (first * geo.columns).min(section.items.len())
                        ..((first + rows) * geo.columns).min(section.items.len())
                }
            };
            for index in visible {
                let (column, row) = match section.layout {
                    Layout::Row => (index, 0),
                    Layout::Grid => (index % geo.columns, index / geo.columns),
                };
                let min = Pos2::new(
                    left + column as f32 * pitch.x - section.scroll,
                    top + geo.cards_y + row as f32 * pitch.y,
                );
                let rect = Rect::from_min_size(min, geo.card);
                if !rect.intersects(content) {
                    continue;
                }
                // Les cartes qui passent sous le bandeau s'estompent au lieu d'être coupées net.
                let alpha = ((rect.bottom() - content.top()) / rect.height()).clamp(0.0, 1.0).powi(2);
                let card = CardView {
                    item: &section.items[index],
                    rect,
                    thumb_height: geo.thumb_height,
                    shape: section.shape,
                    mixed: section.mixed,
                    focused: (section_index, index) == self.focus,
                    alpha,
                };
                painted.animating |= paint_card(&clipped, rem, card, images);
                if click.is_some_and(|position| rect.contains(position) && content.contains(position)) {
                    painted.clicked = Some((section_index, index));
                }
            }
        }
        painted
    }
}
