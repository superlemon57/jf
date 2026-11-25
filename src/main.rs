//! jf — client Jellyfin natif pour le salon, avec mpv intégré.

mod app;
mod atv;
mod config;
mod images;
mod jellyfin;
mod mpv;
mod player;
mod remote;
mod ui;

use std::process::ExitCode;

use eframe::egui;

pub const USER_AGENT: &str = concat!("jf/", env!("CARGO_PKG_VERSION"));

const HELP: &str = "\
jf — client Jellyfin natif pour le salon

Usage : jf [OPTIONS]

  -f, --fullscreen   démarrer en plein écran
      --logout       oublier la session enregistrée
      --reset-apple-tv
                     nouvelle identité Apple TV : oublie les iPhone appairés,
                     qui verront un nouvel appareil (à appairer de nouveau)
  -h, --help         afficher cette aide
  -V, --version      afficher la version";

fn main() -> ExitCode {
    let mut fullscreen = false;
    for argument in std::env::args().skip(1) {
        match argument.as_str() {
            "-f" | "--fullscreen" => fullscreen = true,
            "--logout" => {
                let mut config = config::Config::load();
                config.token.clear();
                config.user_id.clear();
                return match config.save() {
                    Ok(()) => {
                        println!("Session oubliée.");
                        ExitCode::SUCCESS
                    }
                    Err(error) => {
                        eprintln!("jf : {error}");
                        ExitCode::FAILURE
                    }
                };
            }
            "--reset-apple-tv" => {
                let directory = config::config_dir().join("apple-tv");
                return match std::fs::remove_dir_all(&directory) {
                    Ok(()) => {
                        println!("Identité Apple TV réinitialisée : appairez de nouveau l'iPhone.");
                        ExitCode::SUCCESS
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        println!("Rien à réinitialiser.");
                        ExitCode::SUCCESS
                    }
                    Err(error) => {
                        eprintln!("jf : {error}");
                        ExitCode::FAILURE
                    }
                };
            }
            "-h" | "--help" => {
                println!("{HELP}");
                return ExitCode::SUCCESS;
            }
            "-V" | "--version" => {
                println!("jf {}", env!("CARGO_PKG_VERSION"));
                return ExitCode::SUCCESS;
            }
            other => {
                eprintln!("jf : option inconnue « {other} »\n\n{HELP}");
                return ExitCode::from(2);
            }
        }
    }

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("jf")
            .with_app_id("jf")
            .with_inner_size([1280.0, 720.0])
            .with_min_inner_size([640.0, 360.0]),
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };
    let created = eframe::run_native("jf", options, Box::new(move |cc| Ok(Box::new(app::App::new(cc, fullscreen)?))));
    match created {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("jf : {error}");
            ExitCode::FAILURE
        }
    }
}
