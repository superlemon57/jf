//! Télécommande Apple TV d'iOS (Centre de contrôle et app « Remote ») : jf se présente
//! sur le réseau local comme une Apple TV, s'appaire avec un code affiché à l'écran,
//! puis traduit boutons, gestes et commandes de lecture en commandes de jf.
//!
//! Le protocole (Companion Link) a été reconstitué par pyatv ; le codec OPACK, le SRP,
//! le TLV et le chiffrement viennent de myRemote (licence MIT), la session est propre à jf.

mod archive;
mod crypto;
mod opack;
mod session;
mod srp;
mod tlv;

use std::fs;
use std::io::{self, Write};
use std::net::{IpAddr, Ipv4Addr, TcpListener, UdpSocket};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui;
use mdns_sd::{IfKind, ServiceDaemon, ServiceInfo};
use rand::RngCore;
use sha2::{Digest, Sha256};

use crate::remote;

const DEVICE_MODEL: &str = "AppleTV14,1";
const SOURCE_VERSION: &str = "715.2";
const COMPANION_FLAGS: &str = "0x36782";
/// Port Companion Link habituel d'une Apple TV ; un autre est pris s'il est occupé.
const PORT: u16 = 49153;
const PAIRING_WINDOW: Duration = Duration::from_secs(120);
/// Essais de code autorisés par fenêtre d'appairage.
const PAIRING_ATTEMPTS: u8 = 3;
/// Délai avant une nouvelle fenêtre après un code faux, contre la recherche systématique.
const PAIRING_COOLDOWN: Duration = Duration::from_secs(30);
const MAX_CONNECTIONS: usize = 32;

/// Journal du protocole, toujours écrit dans `~/.cache/jf/apple-tv.log` (le précédent est
/// gardé en `.log.1`) pour qu'un souci avec l'iPhone laisse une trace sans rien préparer ;
/// `JF_LOG=1` le recopie aussi sur la sortie d'erreur.
static LOG: Mutex<Option<fs::File>> = Mutex::new(None);
const LOG_LIMIT: u64 = 8 * 1024 * 1024;

fn open_log() {
    let directory = crate::config::cache_dir();
    let path = directory.join("apple-tv.log");
    let _ = fs::create_dir_all(&directory);
    let _ = fs::rename(&path, directory.join("apple-tv.log.1"));
    if let Ok(mut file) = fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&path) {
        let since = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
        let _ = writeln!(file, "jf {} — démarré à {} (secondes Unix)", env!("CARGO_PKG_VERSION"), since.as_secs());
        *LOG.lock().unwrap() = Some(file);
    }
    clock();
}

pub(crate) fn log_line(line: &str) {
    let line = format!("[apple-tv {:9.3}] {line}", clock());
    if std::env::var_os("JF_LOG").is_some_and(|value| value != "0") {
        eprintln!("{line}");
    }
    if let Some(file) = LOG.lock().unwrap().as_mut() {
        if file.metadata().is_ok_and(|meta| meta.len() < LOG_LIMIT) {
            let _ = writeln!(file, "{line}");
        }
    }
}

/// Secondes écoulées depuis l'ouverture du journal.
fn clock() -> f64 {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_secs_f64()
}

macro_rules! trace {
    ($($argument:tt)*) => {
        $crate::atv::log_line(&format!($($argument)*))
    };
}
pub(crate) use trace;

/// Identité stable de l'« Apple TV » jf, dérivée d'une clé privée enregistrée.
struct Identity {
    private_key: [u8; 32],
    identifier: String,
    mac: [u8; 6],
}

impl Identity {
    /// Valeur dérivée de l'identifiant public, en hexadécimal minuscule comme sur une
    /// vraie Apple TV ou un HomePod (rpHI, rpAD…). La clé privée n'y entre jamais.
    fn alias(&self, label: &str) -> String {
        let hash = Sha256::new().chain_update(label).chain_update(self.identifier.as_bytes()).finalize();
        hash[..6].iter().map(|byte| format!("{byte:02x}")).collect()
    }

    /// UUID stable dérivé de l'identifiant public.
    fn uuid(&self, label: &str) -> String {
        let hash = Sha256::new().chain_update(label).chain_update(self.identifier.as_bytes()).finalize();
        let hex: String = hash[..16].iter().map(|byte| format!("{byte:02X}")).collect();
        format!("{}-{}-{}-{}-{}", &hex[..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..])
    }

    /// Adresse Bluetooth annoncée (rpBA), aussi donnée comme `_pubID`.
    fn bluetooth(&self) -> String {
        self.mac.map(|byte| format!("{byte:02X}")).join(":")
    }
}

struct PairingWindow {
    pin: u32,
    generation: u64,
    expires: Instant,
    attempts: u8,
}

#[derive(Default)]
struct PairingState {
    window: Option<PairingWindow>,
    generation: u64,
    blocked_until: Option<Instant>,
}

/// État partagé entre les connexions et l'interface.
pub struct Shared {
    name: String,
    identity: Identity,
    directory: PathBuf,
    pairing: Mutex<PairingState>,
    /// Message à afficher par l'interface (« iPhone jumelé »).
    notice: Mutex<Option<String>>,
    pub(crate) state: remote::Shared,
    commands: Sender<remote::Command>,
    ui: egui::Context,
}

pub struct Handle {
    shared: Arc<Shared>,
    mdns: ServiceDaemon,
    /// Noms complets des services annoncés, pour les retirer à la fermeture.
    services: Vec<String>,
}

impl Drop for Handle {
    /// Annonce le départ de jf : sans cela, l'iPhone garde l'ancienne annonce en cache
    /// (plus d'une heure) et peut viser un appareil qui n'existe plus.
    fn drop(&mut self) {
        for service in &self.services {
            if let Ok(done) = self.mdns.unregister(service) {
                let _ = done.recv_timeout(Duration::from_millis(500));
            }
        }
        let _ = self.mdns.shutdown();
        trace!("annonce retirée");
    }
}

impl Handle {
    /// Code à afficher pendant qu'un iPhone demande l'appairage.
    pub fn pairing_code(&self) -> Option<(u32, Duration)> {
        let pairing = self.shared.pairing.lock().unwrap();
        let window = pairing.window.as_ref()?;
        let left = window.expires.checked_duration_since(Instant::now())?;
        Some((window.pin, left))
    }

    pub fn take_notice(&self) -> Option<String> {
        self.shared.notice.lock().unwrap().take()
    }
}

impl Shared {
    fn send(&self, command: remote::Command) {
        if self.commands.send(command).is_ok() {
            self.ui.request_repaint();
        }
    }

    fn key(&self, key: &str) {
        self.send(remote::Command::Key(key.to_owned()));
    }

    /// Ouvre (ou reprend) la fenêtre d'appairage et renvoie le code attendu.
    fn begin_pairing(&self) -> Option<(u32, u64)> {
        let mut pairing = self.pairing.lock().unwrap();
        let now = Instant::now();
        if pairing.blocked_until.is_some_and(|until| now < until) {
            return None;
        }
        let expired = pairing.window.as_ref().is_none_or(|window| now >= window.expires || window.attempts == 0);
        if expired {
            pairing.generation += 1;
            let mut bytes = [0u8; 4];
            rand::rngs::OsRng.fill_bytes(&mut bytes);
            let pin = 1000 + u32::from_le_bytes(bytes) % 9000;
            pairing.window = Some(PairingWindow {
                pin,
                generation: pairing.generation,
                expires: now + PAIRING_WINDOW,
                attempts: PAIRING_ATTEMPTS,
            });
        }
        let window = pairing.window.as_mut().expect("fenêtre ouverte");
        window.attempts -= 1;
        let result = (window.pin, window.generation);
        self.ui.request_repaint();
        Some(result)
    }

    fn reject_code(&self) {
        let mut pairing = self.pairing.lock().unwrap();
        if pairing.window.as_ref().is_some_and(|window| window.attempts == 0) {
            pairing.window = None;
            pairing.blocked_until = Some(Instant::now() + PAIRING_COOLDOWN);
        }
        self.ui.request_repaint();
    }

    fn trust(&self, generation: u64, id: &[u8], key: [u8; 32]) -> Result<(), String> {
        let mut pairing = self.pairing.lock().unwrap();
        let open = pairing
            .window
            .as_ref()
            .is_some_and(|window| window.generation == generation && Instant::now() < window.expires);
        if !open {
            return Err("fenêtre d'appairage fermée".into());
        }
        write_private(&self.controller_file(id), &key).map_err(|error| error.to_string())?;
        pairing.window = None;
        *self.notice.lock().unwrap() = Some("iPhone jumelé".into());
        self.ui.request_repaint();
        Ok(())
    }

    fn controller_key(&self, id: &[u8]) -> Option<[u8; 32]> {
        fs::read(self.controller_file(id)).ok()?.try_into().ok()
    }

    fn controller_file(&self, id: &[u8]) -> PathBuf {
        let name: String = Sha256::digest(id).iter().take(16).map(|byte| format!("{byte:02x}")).collect();
        self.directory.join("controllers").join(name)
    }
}

fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(path)?;
    file.write_all(bytes)
}

fn load_identity(directory: &Path) -> io::Result<Identity> {
    let path = directory.join("identity.key");
    let private_key: [u8; 32] = match fs::read(&path) {
        Ok(bytes) => bytes.try_into().map_err(|_| io::Error::other("identity.key invalide"))?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let mut key = [0u8; 32];
            rand::rngs::OsRng.fill_bytes(&mut key);
            write_private(&path, &key)?;
            key
        }
        Err(error) => return Err(error),
    };
    let hash = Sha256::new().chain_update(b"jf apple tv identity").chain_update(private_key).finalize();
    let hex: String = hash[..16].iter().map(|byte| format!("{byte:02X}")).collect();
    let identifier = format!("{}-{}-{}-{}-{}", &hex[..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..]);
    let mut mac: [u8; 6] = hash[16..22].try_into().expect("6 octets");
    // Adresse locale (bit 2) et non multicast (bit 1).
    mac[0] = (mac[0] | 2) & !1;
    Ok(Identity { private_key, identifier, mac })
}

fn local_ip() -> Option<Ipv4Addr> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    // Aucun paquet n'est envoyé : la connexion choisit seulement l'interface de sortie.
    socket.connect((Ipv4Addr::new(192, 0, 2, 1), 9)).ok()?;
    match socket.local_addr().ok()?.ip() {
        IpAddr::V4(ip) if !ip.is_loopback() => Some(ip),
        _ => None,
    }
}

fn bind(port: u16) -> io::Result<TcpListener> {
    TcpListener::bind((Ipv4Addr::UNSPECIFIED, port)).or_else(|_| TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0)))
}

/// Démarre l'annonce Bonjour et les connexions. `name` est le nom affiché sur l'iPhone.
pub fn start(
    name: &str,
    directory: PathBuf,
    state: remote::Shared,
    commands: Sender<remote::Command>,
    ui: egui::Context,
) -> Result<Handle, String> {
    open_log();
    let ip = local_ip().ok_or("aucune adresse sur le réseau local")?;
    let identity = load_identity(&directory).map_err(|error| error.to_string())?;
    let companion = bind(PORT).map_err(|error| error.to_string())?;
    let port = companion.local_addr().map(|address| address.port()).map_err(|error| error.to_string())?;

    let shared = Arc::new(Shared {
        name: name.to_owned(),
        identity,
        directory,
        pairing: Mutex::default(),
        notice: Mutex::default(),
        state,
        commands,
        ui,
    });
    let (mdns, services) = advertise(&shared, ip, port).map_err(|error| format!("Bonjour : {error}"))?;
    trace!("annoncé sur {ip}, port {port}");
    let connections = Arc::new(AtomicUsize::new(0));
    let session_shared = shared.clone();
    std::thread::spawn(move || {
        for stream in companion.incoming().flatten() {
            if connections.load(Ordering::SeqCst) >= MAX_CONNECTIONS {
                continue;
            }
            let (shared, connections) = (session_shared.clone(), connections.clone());
            connections.fetch_add(1, Ordering::SeqCst);
            std::thread::spawn(move || {
                session::serve(stream, shared);
                connections.fetch_sub(1, Ordering::SeqCst);
            });
        }
    });
    Ok(Handle { shared, mdns, services })
}

/// Annonce le service Companion Link d'une Apple TV, seulement sur l'interface du réseau
/// local. Comme linux-apple-remote (maintenu pour iOS 26/27), on n'annonce ni MediaRemote
/// ni AirPlay : un iPhone qui tente ces services factices attend une réponse qui ne vient
/// jamais, et la télécommande reste sur « Connexion ».
fn advertise(shared: &Shared, ip: Ipv4Addr, port: u16) -> Result<(ServiceDaemon, Vec<String>), mdns_sd::Error> {
    let identity = &shared.identity;
    let mac = identity.bluetooth();
    let (rp_ha, rp_hn, rp_ad, rp_hi) =
        (identity.alias("rpHA:"), identity.alias("rpHN:"), identity.alias("rpAD:"), identity.alias("rpHI:"));
    let txt = [
        ("rpMac", "2"),
        ("rpHA", rp_ha.as_str()),
        ("rpHN", &rp_hn),
        ("rpVr", SOURCE_VERSION),
        ("rpMd", DEVICE_MODEL),
        ("rpFl", COMPANION_FLAGS),
        ("rpAD", &rp_ad),
        ("rpHI", &rp_hi),
        ("rpBA", &mac),
        ("rpMRtID", &identity.identifier),
    ];
    let daemon = ServiceDaemon::new()?;
    daemon.disable_interface(IfKind::All)?;
    daemon.enable_interface(IfKind::Addr(IpAddr::V4(ip)))?;
    let host = format!("jf-{}.local.", mac.replace(':', "").to_lowercase());
    let info = ServiceInfo::new("_companion-link._tcp.local.", &shared.name, &host, IpAddr::V4(ip), port, &txt[..])?;
    let services = vec![info.get_fullname().to_owned()];
    daemon.register(info)?;
    Ok((daemon, services))
}
