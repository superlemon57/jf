//! jf — lightweight Jellyfin HTPC client with embedded mpv.

// No console window next to the application on Windows (release builds).
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod androidtv;
mod app;
mod atv;
mod cast;
mod config;
mod i18n;
mod images;
mod jellyfin;
mod lan;
mod mpv;
mod player;
mod remote;
mod ui;

use std::process::ExitCode;

use eframe::egui;

use i18n::t;

pub const USER_AGENT: &str = concat!("jf/", env!("CARGO_PKG_VERSION"));

fn help() -> &'static str {
    t(
        "\
jf — lightweight Jellyfin HTPC client

Usage: jf [OPTIONS]

  -f, --fullscreen        start in fullscreen
      --logout            forget the saved session
      --reset-apple-tv    new Apple TV identity: forgets paired iPhones,
                          which then see a new device to pair again
      --reset-android-tv  forget paired Android phones
  -h, --help              show this help
  -V, --version           show the version",
        "\
jf — client Jellyfin léger pour le salon

Usage : jf [OPTIONS]

  -f, --fullscreen        démarrer en plein écran
      --logout            oublier la session enregistrée
      --reset-apple-tv    nouvelle identité Apple TV : oublie les iPhone appairés,
                          qui voient alors un nouvel appareil à appairer
      --reset-android-tv  oublier les téléphones Android appairés
  -h, --help              afficher cette aide
  -V, --version           afficher la version",
    )
}

/// Deletes a remote's saved pairings.
fn reset(directory: &std::path::Path, done: &str) -> ExitCode {
    match std::fs::remove_dir_all(directory) {
        Ok(()) => {
            println!("{done}");
            ExitCode::SUCCESS
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            println!("{}", t("Nothing to reset.", "Rien à réinitialiser."));
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("jf: {error}");
            ExitCode::FAILURE
        }
    }
}

fn main() -> ExitCode {
    i18n::set(i18n::Language::parse(&config::Config::load().language));
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
                        println!("{}", t("Session forgotten.", "Session oubliée."));
                        ExitCode::SUCCESS
                    }
                    Err(error) => {
                        eprintln!("jf: {error}");
                        ExitCode::FAILURE
                    }
                };
            }
            "--reset-apple-tv" => {
                let done = t(
                    "Apple TV identity reset: pair the iPhone again.",
                    "Identité Apple TV réinitialisée : appairez de nouveau l'iPhone.",
                );
                return reset(&config::config_dir().join("apple-tv"), done);
            }
            "--reset-android-tv" => {
                let done = t(
                    "Android phones forgotten: pair them again.",
                    "Téléphones Android oubliés : appairez-les de nouveau.",
                );
                return reset(&config::config_dir().join("android-tv").join("clients"), done);
            }
            "-h" | "--help" => {
                println!("{}", help());
                return ExitCode::SUCCESS;
            }
            "-V" | "--version" => {
                println!("jf {}", env!("CARGO_PKG_VERSION"));
                return ExitCode::SUCCESS;
            }
            other => {
                eprintln!("jf: {} '{other}'\n\n{}", t("unknown option", "option inconnue"), help());
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
            eprintln!("jf: {error}");
            ExitCode::FAILURE
        }
    }
}
