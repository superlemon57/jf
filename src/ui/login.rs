//! Écran de connexion au serveur Jellyfin.

use eframe::egui::{self, FontId, Key, RichText};

use super::theme;

#[derive(Default)]
pub struct Login {
    server: String,
    user: String,
    password: String,
    pub busy: bool,
    pub error: Option<String>,
    /// Code Quick Connect à saisir sur un autre appareil.
    pub code: Option<String>,
    /// Le premier champ a reçu le focus à l'ouverture de l'écran.
    focused: bool,
}

pub enum Request {
    Password { server: String, user: String, password: String },
    QuickConnect { server: String },
}

impl Login {
    pub fn new(server: &str) -> Self {
        Self { server: server.to_owned(), ..Self::default() }
    }

    pub fn show(&mut self, ui: &mut egui::Ui) -> Option<Request> {
        let rem = theme::rem(ui.max_rect());
        let width = 24.0 * rem;
        let style = ui.style_mut();
        style.override_font_id = Some(FontId::proportional(1.1 * rem));
        style.spacing.item_spacing = egui::vec2(0.6, 0.8) * rem;
        style.spacing.button_padding = egui::vec2(1.2, 0.6) * rem;
        style.spacing.text_edit_width = width;

        let mut request = None;
        let column = egui::Rect::from_center_size(ui.max_rect().center(), egui::vec2(width, 26.0 * rem));
        ui.scope_builder(egui::UiBuilder::new().max_rect(column), |ui| {
            ui.label(RichText::new("jf").font(FontId::new(2.4 * rem, theme::semibold())).color(theme::TEXT));
            ui.label(RichText::new("Connexion à Jellyfin").color(theme::DIM));
            ui.add_space(rem);

            let field = |text: &mut String, hint: &str, password: bool, ui: &mut egui::Ui| {
                let edit = egui::TextEdit::singleline(text)
                    .hint_text(hint)
                    .password(password)
                    .margin(egui::vec2(0.8, 0.6) * rem)
                    .desired_width(width);
                ui.add_enabled(!self.busy, edit)
            };
            let server = field(&mut self.server, "Adresse du serveur", false, ui);
            if !self.focused {
                server.request_focus();
                self.focused = true;
            }
            field(&mut self.user, "Utilisateur", false, ui);
            let password = field(&mut self.password, "Mot de passe", true, ui);
            let submitted = password.lost_focus() && ui.input(|input| input.key_pressed(Key::Enter));

            ui.add_space(0.4 * rem);
            ui.horizontal(|ui| {
                let ready = !self.busy && !self.server.trim().is_empty();
                let connect = ui.add_enabled(ready && !self.user.is_empty(), egui::Button::new("Se connecter"));
                if connect.clicked() || (submitted && ready && !self.user.is_empty()) {
                    request = Some(Request::Password {
                        server: self.server.clone(),
                        user: self.user.clone(),
                        password: self.password.clone(),
                    });
                }
                if ui.add_enabled(ready, egui::Button::new("Quick Connect")).clicked() {
                    request = Some(Request::QuickConnect { server: self.server.clone() });
                }
            });

            ui.add_space(0.4 * rem);
            if let Some(code) = &self.code {
                ui.label(RichText::new(code).font(FontId::new(2.4 * rem, theme::semibold())).color(theme::TEXT));
                ui.label(
                    RichText::new("Saisis ce code dans un client déjà connecté (menu utilisateur → Quick Connect).")
                        .color(theme::DIM),
                );
            } else if let Some(error) = &self.error {
                ui.label(RichText::new(error).color(theme::TEXT));
            } else if self.busy {
                ui.label(RichText::new("Connexion…").color(theme::DIM));
            }
        });
        request
    }
}
