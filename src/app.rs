//! État de l'application et boucle principale.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{self, Key, Pos2, Rect};

use crate::config::{self, Config};
use crate::images::Images;
use crate::jellyfin::{Client, Item};
use crate::mpv::{self, Mpv};
use crate::player::{Outcome, Player, Stream, TrackKind};
use crate::remote;
use crate::ui::browse::{self, Backdrop, Direction, Page, Section, Source};
use crate::ui::login::{self, Login};
use crate::ui::overlay::{self, Action, Menu};
use crate::ui::{playback, theme};

const TOAST_DURATION: Duration = Duration::from_secs(4);
const CURSOR_TIMEOUT: Duration = Duration::from_secs(3);
const QUICK_CONNECT_TIMEOUT: Duration = Duration::from_secs(300);

/// Résultat d'un travail d'arrière-plan, traité par le fil d'interface.
pub enum Message {
    Page(u64, Result<Vec<Section>, String>),
    Resolved(u64, Result<Box<Stream>, String>),
    Shuffled(Result<Vec<Item>, String>),
    PlayedSaved(Result<(), String>),
    QuickConnectCode(String),
    LoggedIn(Result<Client, String>),
}

/// Lance des travaux hors du fil d'interface et la réveille à leur retour.
#[derive(Clone)]
pub struct Tasks {
    sender: Sender<Message>,
    ctx: egui::Context,
}

impl Tasks {
    pub fn spawn(&self, work: impl FnOnce() -> Message + Send + 'static) {
        let tasks = self.clone();
        std::thread::spawn(move || tasks.send(work()));
    }

    fn send(&self, message: Message) {
        let _ = self.sender.send(message);
        self.ctx.request_repaint();
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Nav {
    Move(Direction),
    Ok,
    Back,
    Home,
    Menu,
    Restart,
    TogglePlayed,
}

pub struct App {
    config: Config,
    client: Option<Client>,
    tasks: Tasks,
    inbox: Receiver<Message>,
    images: Images,
    player: Player,
    remote_state: remote::Shared,
    remote_inbox: Receiver<remote::Command>,

    login: Login,
    /// Invalide la tentative de connexion précédente quand une nouvelle démarre.
    login_attempt: Arc<AtomicU64>,
    pages: Vec<Page>,
    next_page: u64,
    backdrop: Backdrop,
    menu: Option<Menu>,
    search: Option<String>,
    toast: Option<(String, Instant)>,

    screen: Rect,
    wheel: f32,
    pointer_moved: Instant,
    state_published: Instant,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, fullscreen: bool) -> Result<Self, String> {
        let ctx = &cc.egui_ctx;
        theme::install_fonts(ctx);
        ctx.set_visuals(egui::Visuals { panel_fill: theme::BACKGROUND, ..egui::Visuals::dark() });
        ctx.options_mut(|options| options.zoom_with_keyboard = false);

        let config = Config::load();
        let mpv = Mpv::new(cc, &config::config_dir().join("mpv"))?;
        if fullscreen {
            mpv.set("fullscreen", "yes");
        }
        let (sender, inbox) = channel();
        let (remote_sender, remote_inbox) = channel();
        let remote_state = remote::Shared::default();

        let mut app = Self {
            client: None,
            tasks: Tasks { sender, ctx: ctx.clone() },
            inbox,
            images: Images::new(ctx),
            player: Player::new(mpv),
            remote_state: remote_state.clone(),
            remote_inbox,
            login: Login::new(&config.server),
            login_attempt: Arc::default(),
            pages: Vec::new(),
            next_page: 0,
            backdrop: Backdrop::default(),
            menu: None,
            search: None,
            toast: None,
            screen: ctx.content_rect(),
            wheel: 0.0,
            pointer_moved: Instant::now(),
            state_published: Instant::now(),
            config,
        };
        if !app.config.remote.is_empty() {
            if let Err(error) = remote::start(&app.config.remote, remote_state, remote_sender, ctx.clone()) {
                app.show_toast(format!("Télécommande indisponible sur {} : {error}", app.config.remote));
            }
        }
        if app.config.logged_in() {
            app.set_client(Client::from_config(&app.config));
        }
        Ok(app)
    }

    fn show_toast(&mut self, message: impl Into<String>) {
        self.toast = Some((message.into(), Instant::now()));
    }

    fn set_client(&mut self, client: Client) {
        self.images.set_client(Some(client.clone()));
        self.player.set_client(Some(client.clone()));
        self.client = Some(client);
        self.pages.clear();
        self.open(Source::Home);
    }

    // ---------------------------------------------------------------- pages

    fn fetch(&self, id: u64, source: Source) {
        let Some(client) = self.client.clone() else { return };
        self.tasks.spawn(move || Message::Page(id, source.fetch(&client)));
    }

    fn open(&mut self, source: Source) {
        self.next_page += 1;
        self.fetch(self.next_page, source.clone());
        self.pages.push(Page::new(self.next_page, source));
    }

    /// Recharge la page affichée ; les autres le seront quand on y reviendra.
    fn refresh(&mut self) {
        for page in &mut self.pages {
            page.stale = true;
        }
        self.refresh_if_stale();
    }

    fn refresh_if_stale(&mut self) {
        let Some(page) = self.pages.last_mut().filter(|page| page.stale) else { return };
        page.stale = false;
        let (id, source) = (page.id, page.source.clone());
        self.fetch(id, source);
    }

    fn back(&mut self) {
        if self.pages.len() > 1 {
            self.pages.pop();
            self.refresh_if_stale();
        }
    }

    // ---------------------------------------------------------------- actions

    fn play_focused(&mut self, from_start: bool) {
        let Some(page) = self.pages.last() else { return };
        let (Some(section), Some(item)) = (page.sections.get(page.focus.0), page.focused()) else { return };
        // Dans une saison, un album ou la liste des chaînes, la lecture enchaîne sur la suite.
        let sequence = !section.mixed && matches!(item.kind.as_str(), "Episode" | "Audio" | "TvChannel");
        let (queue, index) = if sequence { (section.items.clone(), page.focus.1) } else { (vec![item.clone()], 0) };
        self.menu = None;
        self.player.play(queue, index, from_start, &self.tasks);
    }

    fn activate(&mut self) {
        let Some(item) = self.pages.last().and_then(Page::focused).cloned() else { return };
        if item.is_playable() {
            self.play_focused(false);
        } else {
            self.open(Source::Children(Box::new(item)));
        }
    }

    fn toggle_played(&mut self) {
        let (Some(client), Some(page)) = (self.client.clone(), self.pages.last_mut()) else { return };
        let Some(item) = page.focused_mut().filter(|item| !item.is_channel()) else { return };
        // Mise à jour immédiate à l'écran ; la page est rechargée une fois le serveur à jour.
        let played = !item.user_data.played;
        item.user_data.played = played;
        item.user_data.playback_position_ticks = 0;
        item.user_data.unplayed_item_count = None;
        let id = item.id.clone();
        self.tasks.spawn(move || Message::PlayedSaved(client.set_played(&id, played).map_err(|e| e.to_string())));
    }

    fn run(&mut self, action: Action) {
        let Some(menu) = self.menu.take() else { return };
        match action {
            Action::Play => self.play_focused(false),
            Action::Restart => self.play_focused(true),
            Action::Open => self.open(Source::Children(Box::new(menu.item))),
            Action::TogglePlayed => self.toggle_played(),
            Action::Shuffle => {
                let Some(client) = self.client.clone() else { return };
                self.show_toast("Préparation de la lecture aléatoire…");
                self.tasks.spawn(move || Message::Shuffled(client.shuffled(&menu.item).map_err(|e| e.to_string())));
            }
        }
    }

    fn navigate(&mut self, nav: Nav) {
        if let Some(menu) = &mut self.menu {
            match nav {
                Nav::Move(Direction::Up) => menu.move_focus(false),
                Nav::Move(Direction::Down) => menu.move_focus(true),
                Nav::Ok => {
                    let action = menu.selected();
                    self.run(action);
                }
                Nav::Back | Nav::Menu => self.menu = None,
                _ => {}
            }
            return;
        }
        match nav {
            Nav::Move(direction) => {
                if let Some(page) = self.pages.last_mut() {
                    page.move_focus(direction, self.screen);
                }
            }
            Nav::Ok => self.activate(),
            Nav::Back => self.back(),
            Nav::Home => {
                self.pages.truncate(1);
                self.refresh_if_stale();
            }
            Nav::Menu => self.menu = self.pages.last().and_then(Page::focused).map(Menu::new),
            Nav::Restart => {
                if self.pages.last().and_then(Page::focused).is_some_and(Item::is_playable) {
                    self.play_focused(true);
                }
            }
            Nav::TogglePlayed => self.toggle_played(),
        }
    }

    fn stop_playback(&mut self) {
        self.player.stop();
        self.refresh();
    }

    // ---------------------------------------------------------------- événements

    fn on_outcome(&mut self, outcome: Outcome) {
        match outcome {
            Outcome::Continue => {}
            Outcome::Ended => self.refresh(),
            Outcome::Failed(message) => {
                self.show_toast(message);
                self.refresh();
            }
        }
    }

    fn process_messages(&mut self) {
        while let Ok(message) = self.inbox.try_recv() {
            match message {
                Message::Page(id, result) => {
                    let Some(page) = self.pages.iter_mut().find(|page| page.id == id) else { continue };
                    page.loading = false;
                    match result {
                        Ok(sections) => {
                            page.error = None;
                            page.set_sections(sections);
                        }
                        Err(error) => page.error = Some(error),
                    }
                }
                Message::Resolved(generation, result) => {
                    let outcome = self.player.on_resolved(generation, result);
                    self.on_outcome(outcome);
                }
                Message::Shuffled(Ok(items)) if items.is_empty() => self.show_toast("Rien à lire ici."),
                Message::Shuffled(Ok(items)) => {
                    self.toast = None;
                    self.player.play(items, 0, true, &self.tasks);
                }
                Message::Shuffled(Err(error)) => self.show_toast(error),
                Message::PlayedSaved(result) => {
                    if let Err(error) = result {
                        self.show_toast(error);
                    }
                    self.refresh();
                }
                Message::QuickConnectCode(code) => self.login.code = Some(code),
                Message::LoggedIn(result) => {
                    self.login.busy = false;
                    self.login.code = None;
                    match result {
                        Ok(client) => {
                            self.config.server.clone_from(&client.server);
                            self.config.token.clone_from(&client.token);
                            self.config.user_id.clone_from(&client.user_id);
                            if let Err(error) = self.config.save() {
                                self.show_toast(format!("Connexion non enregistrée : {error}"));
                            }
                            self.set_client(client);
                        }
                        Err(error) => self.login.error = Some(error),
                    }
                }
            }
        }
    }

    fn process_player(&mut self, ctx: &egui::Context) {
        while let Some(event) = self.player.mpv.poll_event() {
            match event {
                mpv::Event::Shutdown => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                mpv::Event::Fullscreen(on) => ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(on)),
                event => {
                    let outcome = self.player.handle(event, &self.tasks);
                    self.on_outcome(outcome);
                }
            }
        }
        self.player.tick();
    }

    fn process_remote(&mut self) {
        while let Ok(command) = self.remote_inbox.try_recv() {
            let playing = self.player.is_active();
            let mpv = |args: &[&str]| {
                let _ = self.player.mpv.command(args);
            };
            match command {
                remote::Command::SeekTo(position) => mpv(&["seek", &position.to_string(), "absolute"]),
                remote::Command::SeekBy(offset) => mpv(&["seek", &offset.to_string(), "relative"]),
                remote::Command::Track(kind, id) => self.player.select_track(kind, id),
                remote::Command::Key(key) if playing => match key.as_str() {
                    "ok" | "playpause" => mpv(&["cycle", "pause"]),
                    "play" => self.player.mpv.set("pause", "no"),
                    "pause" => self.player.mpv.set("pause", "yes"),
                    "left" => mpv(&["seek", "-10"]),
                    "right" => mpv(&["seek", "10"]),
                    "down" => mpv(&["seek", "-60"]),
                    "up" => mpv(&["seek", "60"]),
                    "next" => self.player.skip(1, &self.tasks),
                    "previous" => self.player.skip(-1, &self.tasks),
                    "back" | "stop" => self.stop_playback(),
                    _ => {}
                },
                remote::Command::Key(key) => {
                    let nav = match key.as_str() {
                        "up" => Nav::Move(Direction::Up),
                        "down" => Nav::Move(Direction::Down),
                        "left" => Nav::Move(Direction::Left),
                        "right" => Nav::Move(Direction::Right),
                        "ok" => Nav::Ok,
                        "back" => Nav::Back,
                        "home" => Nav::Home,
                        "menu" => Nav::Menu,
                        _ => continue,
                    };
                    if self.client.is_some() && self.search.is_none() {
                        self.navigate(nav);
                    }
                }
            }
        }
    }

    /// Met à jour l'état lu par la télécommande, au plus quatre fois par seconde.
    fn publish_state(&mut self, ctx: &egui::Context) {
        const INTERVAL: Duration = Duration::from_millis(250);
        let elapsed = self.state_published.elapsed();
        if elapsed < INTERVAL {
            // Repasser plus tard : sans cela, un changement suivi d'aucun dessin ne serait jamais publié.
            ctx.request_repaint_after(INTERVAL - elapsed);
            return;
        }
        self.state_published = Instant::now();
        let mut state = remote::State::default();
        if let (Some(item), Some(client)) = (self.player.item(), &self.client) {
            let (position, duration, paused, live) = self.player.progress().unwrap_or_default();
            state = remote::State {
                playing: true,
                paused,
                live,
                title: item.play_title(),
                position,
                duration,
                has_next: self.player.has_next(),
                has_previous: self.player.has_previous(),
                audio: self.player.tracks(TrackKind::Audio),
                subtitles: self.player.tracks(TrackKind::Subtitle),
                artwork: browse::artwork(item).map(|image| (image, client.clone())),
            };
        }
        *self.remote_state.lock().unwrap() = state;
    }

    // ---------------------------------------------------------------- écrans

    fn start_login(&mut self, request: login::Request) {
        self.login.busy = true;
        self.login.error = None;
        self.login.code = None;
        let attempt = self.login_attempt.fetch_add(1, Ordering::SeqCst) + 1;
        let (counter, tasks, device) = (self.login_attempt.clone(), self.tasks.clone(), self.config.device_id.clone());
        std::thread::spawn(move || {
            let result = (|| match request {
                login::Request::Password { server, user, password } => {
                    let mut client = Client::discover(&server, &device)?;
                    client.login(&user, &password)?;
                    Ok(client)
                }
                login::Request::QuickConnect { server } => {
                    let mut client = Client::discover(&server, &device)?;
                    let quick = client.quick_connect_start()?;
                    tasks.send(Message::QuickConnectCode(quick.code));
                    let started = Instant::now();
                    while started.elapsed() < QUICK_CONNECT_TIMEOUT && counter.load(Ordering::SeqCst) == attempt {
                        std::thread::sleep(Duration::from_secs(2));
                        if client.quick_connect_finish(&quick.secret)? {
                            return Ok(client);
                        }
                    }
                    Err(crate::jellyfin::Error::timeout())
                }
            })();
            if counter.load(Ordering::SeqCst) == attempt {
                tasks.send(Message::LoggedIn(result.map_err(|error: crate::jellyfin::Error| error.to_string())));
            }
        });
    }

    fn browse_input(&mut self, ctx: &egui::Context) -> Option<Pos2> {
        const KEYS: [(Key, Nav); 12] = [
            (Key::ArrowUp, Nav::Move(Direction::Up)),
            (Key::ArrowDown, Nav::Move(Direction::Down)),
            (Key::ArrowLeft, Nav::Move(Direction::Left)),
            (Key::ArrowRight, Nav::Move(Direction::Right)),
            (Key::Enter, Nav::Ok),
            (Key::Backspace, Nav::Back),
            (Key::Escape, Nav::Back),
            (Key::Home, Nav::Home),
            (Key::M, Nav::Menu),
            (Key::O, Nav::Menu),
            (Key::B, Nav::Restart),
            (Key::W, Nav::TogglePlayed),
        ];
        let (pressed, search, fullscreen, wheel, click) = ctx.input(|input| {
            let pressed: Vec<Nav> =
                KEYS.iter().filter(|(key, _)| input.key_pressed(*key)).map(|(_, nav)| *nav).collect();
            let click = input.pointer.primary_clicked().then(|| input.pointer.interact_pos()).flatten();
            let fullscreen = input.key_pressed(Key::F11) || input.key_pressed(Key::F);
            (pressed, input.key_pressed(Key::Slash), fullscreen, input.smooth_scroll_delta().y, click)
        });
        for nav in pressed {
            self.navigate(nav);
        }
        if fullscreen {
            let on = ctx.input(|input| input.viewport().fullscreen.unwrap_or(false));
            self.player.mpv.set("fullscreen", if on { "no" } else { "yes" });
        }
        if search && self.menu.is_none() {
            self.search = Some(String::new());
        }
        // La molette déplace la sélection d'une ligne par cran.
        self.wheel += wheel;
        if self.wheel.abs() >= 40.0 {
            let direction = if self.wheel > 0.0 { Direction::Up } else { Direction::Down };
            self.navigate(Nav::Move(direction));
            self.wheel = 0.0;
        }
        click
    }

    fn search_field(&mut self, ctx: &egui::Context) {
        let Some(query) = &mut self.search else { return };
        let rem = theme::rem(self.screen);
        let position = Pos2::new(self.screen.right() - 29.0 * rem, self.screen.top() + 2.5 * rem);
        let mut done = None;
        egui::Area::new(egui::Id::unique("search")).fixed_pos(position).order(egui::Order::Foreground).show(
            ctx,
            |ui| {
                let frame = egui::Frame::NONE
                    .fill(theme::CARD)
                    .stroke(egui::Stroke::new(0.15 * rem, theme::TEXT))
                    .corner_radius(theme::corner(0.35 * rem))
                    .inner_margin(egui::Margin::symmetric((rem) as i8, (0.6 * rem) as i8));
                frame.show(ui, |ui| {
                    let edit = egui::TextEdit::singleline(query)
                        .hint_text("Rechercher")
                        .font(egui::FontId::proportional(1.2 * rem))
                        .text_color(theme::TEXT)
                        .frame(egui::Frame::NONE)
                        .desired_width(23.0 * rem);
                    let response = ui.add(edit);
                    if response.lost_focus() {
                        done = Some(ui.input(|input| input.key_pressed(Key::Enter)));
                    } else {
                        response.request_focus();
                    }
                });
            },
        );
        if let Some(submitted) = done {
            let query = self.search.take().unwrap_or_default();
            if submitted && !query.trim().is_empty() {
                self.open(Source::Search(query.trim().to_owned()));
            }
        }
    }

    fn browse(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let click = if self.search.is_none() { self.browse_input(&ctx) } else { None };
        let dt = ctx.input(|input| input.stable_dt).min(0.05);
        let menu_open = self.menu.is_some();
        let mut animating = false;
        let mut clicked = None;
        if let Some(page) = self.pages.last_mut() {
            let painted = page.paint(browse::Frame {
                painter: ui.painter(),
                screen: self.screen,
                dt,
                images: &mut self.images,
                backdrop: &mut self.backdrop,
                click: click.filter(|_| !menu_open),
            });
            animating = painted.animating;
            clicked = painted.clicked;
            if let Some(focus) = clicked {
                page.focus = focus;
            }
        }
        if clicked.is_some() {
            self.activate();
        }
        if let Some(menu) = &mut self.menu {
            if let Some(action) = menu.paint(ui.painter(), self.screen, click) {
                self.run(action);
            } else if click.is_some() {
                self.menu = None;
            }
        }
        self.search_field(&ctx);
        if animating {
            ctx.request_repaint();
        }
    }

    fn hide_idle_cursor(&mut self, ctx: &egui::Context) {
        if ctx.input(|input| input.pointer.is_moving() || input.pointer.any_down()) {
            self.pointer_moved = Instant::now();
            ctx.request_repaint_after(CURSOR_TIMEOUT);
        } else if self.pointer_moved.elapsed() >= CURSOR_TIMEOUT {
            ctx.set_cursor_icon(egui::CursorIcon::None);
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = &ui.ctx().clone();
        self.screen = ctx.content_rect();
        self.images.begin_frame();
        self.process_messages();
        self.process_player(ctx);
        self.process_remote();

        egui::CentralPanel::default().frame(egui::Frame::NONE.fill(theme::BACKGROUND)).show(ui, |ui| {
            if self.client.is_none() {
                if let Some(request) = self.login.show(ui) {
                    self.start_login(request);
                }
            } else if self.player.is_active() {
                playback::paint_video(ui, &self.player.mpv, self.player.is_loading());
                if let playback::Input::Stop = playback::forward_input(ctx, &self.player.mpv) {
                    self.stop_playback();
                }
                // Garde l'état de la télécommande à jour même en pause.
                ctx.request_repaint_after(Duration::from_millis(250));
            } else {
                self.browse(ui);
            }
        });

        if let Some((message, since)) = &self.toast {
            if since.elapsed() < TOAST_DURATION {
                overlay::toast(ctx, message);
                ctx.request_repaint_after(TOAST_DURATION);
            } else {
                self.toast = None;
            }
        }
        if self.client.is_some() {
            self.hide_idle_cursor(ctx);
        }
        self.publish_state(ctx);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        // Enregistre la position en cours, puis libère le rendu tant que le contexte OpenGL existe.
        self.player.stop();
        self.player.mpv.free_renderer();
    }
}
