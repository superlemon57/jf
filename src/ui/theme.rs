//! Couleurs, polices et primitives de dessin partagées par les écrans.

use std::process::Command;
use std::sync::Arc;

use eframe::egui::{self, epaint, Color32, FontFamily, FontId, Pos2, Rect};

pub const BACKGROUND: Color32 = Color32::from_rgb(11, 11, 12);
pub const TEXT: Color32 = Color32::from_rgb(236, 236, 236);
pub const DIM: Color32 = Color32::from_rgb(152, 152, 157);
pub const CARD: Color32 = Color32::from_rgb(27, 27, 30);
pub const PANEL: Color32 = Color32::from_rgb(20, 20, 22);

/// Unité de longueur de l'interface : tout grossit avec la largeur de la fenêtre,
/// pour rester lisible depuis le canapé.
pub fn rem(screen: Rect) -> f32 {
    (screen.width() * 0.0115).clamp(13.0, 32.0).round()
}

pub fn semibold() -> FontFamily {
    FontFamily::Name("semibold".into())
}

fn background(alpha: f32) -> Color32 {
    Color32::from_rgba_unmultiplied(BACKGROUND.r(), BACKGROUND.g(), BACKGROUND.b(), (alpha * 255.0) as u8)
}

/// Fichier et index de face choisis par fontconfig pour un motif donné.
fn system_font(pattern: &str) -> Option<(String, u32)> {
    let output = Command::new("fc-match").args(["--format", "%{file}\n%{index}", pattern]).output().ok()?;
    let text = String::from_utf8(output.stdout).ok()?;
    let (file, index) = text.split_once('\n')?;
    Some((file.to_owned(), index.trim().parse().ok()?))
}

/// Utilise la police sans empattement du système (réglages fontconfig de l'utilisateur),
/// en gardant celles d'egui en secours pour les symboles.
pub fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    // Les 16 bits hauts de l'index désignent une instance de police variable : seule la face compte ici.
    let face = |pattern: &str| system_font(pattern).map(|(file, index)| (file, index & 0xffff));
    let mut load = |key: &str, face: &(String, u32), weight: Option<f32>| -> bool {
        let Ok(data) = std::fs::read(&face.0) else { return false };
        let mut font = egui::FontData::from_owned(data);
        font.index = face.1;
        if let Some(weight) = weight {
            // Pour une police variable, c'est ce réglage qui donne la graisse ; une police fixe l'ignore.
            font.tweak.coords = egui::epaint::text::VariationCoords::new([(b"wght", weight)]);
        }
        fonts.font_data.insert(key.to_owned(), Arc::new(font));
        true
    };
    let body = face("sans-serif");
    let regular = body.as_ref().is_some_and(|face| load("regular", face, None));
    let bold = face("sans-serif:semibold").is_some_and(|face| load("semibold", &face, Some(600.0)));
    // Sans police japonaise installée, fontconfig renvoie la police de base : inutile de la recharger.
    let cjk = face("sans-serif:lang=ja")
        .filter(|face| Some(face) != body.as_ref())
        .is_some_and(|face| load("cjk", &face, None));

    let mut chain = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    if cjk {
        chain.push("cjk".to_owned());
    }
    let mut heading = chain.clone();
    if regular {
        chain.insert(0, "regular".to_owned());
    }
    if bold {
        heading.insert(0, "semibold".to_owned());
    } else {
        heading.clone_from(&chain);
    }
    fonts.families.insert(FontFamily::Proportional, chain);
    fonts.families.insert(semibold(), heading);
    ctx.set_fonts(fonts);
}

pub struct Text<'a> {
    pub text: &'a str,
    pub size: f32,
    pub color: Color32,
    pub family: FontFamily,
    pub max_width: f32,
    pub max_rows: usize,
    /// Interligne en multiple de la taille.
    pub leading: f32,
    pub centered: bool,
}

impl<'a> Text<'a> {
    /// Une ligne, coupée par « … » si elle dépasse.
    pub fn line(text: &'a str, size: f32, color: Color32, max_width: f32) -> Self {
        Self {
            text,
            size,
            color,
            family: FontFamily::Proportional,
            max_width,
            max_rows: 1,
            leading: 1.3,
            centered: false,
        }
    }

    pub fn layout(&self, painter: &egui::Painter) -> Arc<egui::Galley> {
        let format = egui::TextFormat {
            font_id: FontId::new(self.size, self.family.clone()),
            color: self.color,
            line_height: Some(self.size * self.leading),
            ..Default::default()
        };
        let mut job = egui::text::LayoutJob::single_section(self.text.to_owned(), format);
        job.wrap = egui::text::TextWrapping {
            max_width: self.max_width,
            max_rows: self.max_rows,
            // Une ligne seule est coupée au caractère près, un paragraphe entre les mots.
            break_anywhere: self.max_rows == 1,
            overflow_character: Some('…'),
        };
        if self.centered {
            job.halign = egui::Align::Center;
        }
        painter.layout_job(job)
    }

    /// Dessine le texte et renvoie sa hauteur. Centré, `position` désigne le milieu du haut.
    pub fn paint(&self, painter: &egui::Painter, position: Pos2) -> f32 {
        let galley = self.layout(painter);
        let height = galley.size().y;
        painter.galley(position, galley, self.color);
        height
    }
}

pub fn corner(radius: f32) -> egui::CornerRadius {
    egui::CornerRadius::same(radius.round().clamp(0.0, 255.0) as u8)
}

/// Image recadrée pour remplir `rect` (`anchor` : 0 garde le haut, 0.5 centre),
/// ou entière et centrée si `contain`.
pub fn image(
    painter: &egui::Painter,
    rect: Rect,
    texture: &egui::TextureHandle,
    radius: f32,
    alpha: f32,
    anchor: f32,
    contain: bool,
) {
    let size = texture.size_vec2();
    if size.x < 1.0 || size.y < 1.0 {
        return;
    }
    let tint = Color32::WHITE.gamma_multiply(alpha);
    let (target, uv) = if contain {
        let scale = (rect.width() / size.x).min(rect.height() / size.y);
        (Rect::from_center_size(rect.center(), size * scale), Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)))
    } else {
        let scale = (rect.width() / size.x).max(rect.height() / size.y);
        let (u, v) = (rect.width() / (size.x * scale), rect.height() / (size.y * scale));
        let (left, top) = ((1.0 - u) / 2.0, (1.0 - v) * anchor);
        (rect, Rect::from_min_max(Pos2::new(left, top), Pos2::new(left + u, top + v)))
    };
    painter.add(epaint::RectShape::filled(target, corner(radius), tint).with_texture(texture.id(), uv));
}

/// Dégradé du fond de l'application vers la transparence.
/// `stops` : (position de 0 à 1 le long de l'axe, opacité du fond).
pub fn fade(painter: &egui::Painter, rect: Rect, horizontal: bool, stops: &[(f32, f32)]) {
    let mut mesh = egui::Mesh::default();
    for (index, (position, alpha)) in stops.iter().enumerate() {
        let color = background(*alpha);
        let (a, b) = if horizontal {
            let x = egui::lerp(rect.left()..=rect.right(), *position);
            (Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom()))
        } else {
            let y = egui::lerp(rect.top()..=rect.bottom(), *position);
            (Pos2::new(rect.left(), y), Pos2::new(rect.right(), y))
        };
        mesh.colored_vertex(a, color);
        mesh.colored_vertex(b, color);
        if index > 0 {
            let base = (index as u32 - 1) * 2;
            mesh.add_triangle(base, base + 1, base + 2);
            mesh.add_triangle(base + 1, base + 2, base + 3);
        }
    }
    painter.add(mesh);
}
