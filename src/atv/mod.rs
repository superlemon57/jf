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

use std::collections::HashMap;
use std::fs;
use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, TcpListener, TcpStream, UdpSocket};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui;
use mdns_sd::{ServiceDaemon, ServiceInfo};
use rand::RngCore;
use sha2::{Digest, Sha256};

use crate::remote;

const DEVICE_MODEL: &str = "AppleTV14,1";
const SOURCE_VERSION: &str = "715.2";
const COMPANION_FLAGS: &str = "0x36782";
/// Ports habituels d'une Apple TV ; un autre est pris s'ils sont occupés.
const PORTS: [u16; 3] = [49152, 49153, 49154];
const PAIRING_WINDOW: Duration = Duration::from_secs(120);
/// Essais de code autorisés par fenêtre d'appairage.
const PAIRING_ATTEMPTS: u8 = 3;
/// Délai avant une nouvelle fenêtre après un code faux, contre la recherche systématique.
const PAIRING_COOLDOWN: Duration = Duration::from_secs(30);
const MAX_CONNECTIONS: usize = 32;

/// Journal du protocole sur la sortie d'erreur, activé par `JF_LOG=1`.
fn logging() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("JF_LOG").is_some_and(|value| value != "0"))
}

/// Secondes écoulées depuis le premier message du journal.
fn clock() -> f64 {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_secs_f64()
}

macro_rules! trace {
    ($($argument:tt)*) => {
        if $crate::atv::logging() {
            eprintln!("[apple-tv {:9.3}] {}", $crate::atv::clock(), format!($($argument)*));
        }
    };
}
pub(crate) use trace;

/// Identité stable de l'« Apple TV » jf, dérivée d'une clé privée enregistrée.
struct Identity {
    private_key: [u8; 32],
    identifier: String,
    mac: [u8; 6],
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
    /// Connexion active de chaque iPhone, par identifiant : une nouvelle télécommande
    /// ouverte par le même iPhone remplace l'ancienne connexion.
    remotes: Mutex<HashMap<Vec<u8>, u64>>,
    connections: AtomicU64,
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

fn hex_upper(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02X}")).collect()
}

/// Démarre l'annonce Bonjour et les connexions. `name` est le nom affiché sur l'iPhone.
pub fn start(
    name: &str,
    directory: PathBuf,
    state: remote::Shared,
    commands: Sender<remote::Command>,
    ui: egui::Context,
) -> Result<Handle, String> {
    let ip = local_ip().ok_or("aucune adresse sur le réseau local")?;
    let identity = load_identity(&directory).map_err(|error| error.to_string())?;
    let listeners = PORTS.map(bind);
    let [media, companion, airplay] = listeners;
    let (media, companion, airplay) =
        (media.map_err(|e| e.to_string())?, companion.map_err(|e| e.to_string())?, airplay.map_err(|e| e.to_string())?);
    let port = |listener: &TcpListener| listener.local_addr().map(|address| address.port()).unwrap_or(0);
    let ports = [port(&media), port(&companion), port(&airplay)];

    let shared = Arc::new(Shared {
        name: name.to_owned(),
        identity,
        directory,
        pairing: Mutex::default(),
        notice: Mutex::default(),
        remotes: Mutex::default(),
        connections: AtomicU64::new(0),
        state,
        commands,
        ui,
    });
    let (mdns, services) = advertise(&shared, ip, ports).map_err(|error| format!("Bonjour : {error}"))?;
    trace!("annoncé sur {ip}, ports {ports:?}");

    std::thread::spawn(move || probe_endpoint(media, false));
    std::thread::spawn(move || probe_endpoint(airplay, true));
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

/// Service Bonjour : type, port et enregistrement TXT.
type Record<'a> = (&'a str, u16, Vec<(&'a str, &'a str)>);

/// Annonce les trois services qu'iOS attend d'une Apple TV.
fn advertise(
    shared: &Shared,
    ip: Ipv4Addr,
    [media, companion, airplay]: [u16; 3],
) -> Result<(ServiceDaemon, Vec<String>), mdns_sd::Error> {
    let identity = &shared.identity;
    let mac = identity.mac.map(|byte| format!("{byte:02X}")).join(":");
    let mac_id = mac.replace(':', "");
    let alias = |label: &str, bytes: usize| {
        let hash = Sha256::new().chain_update(label).chain_update(identity.identifier.as_bytes()).finalize();
        hex_upper(&hash[..bytes])
    };
    let (airplay_id, rp_hn, rp_ad, rp_hi) =
        (alias("airplay-identity:", 8), alias("rpHN:", 6), alias("rpAD:", 6), alias("rpHI:", 6));
    let airplay_key = alias("pk:", 16).to_ascii_lowercase();
    let unique = identity.identifier.replace('-', "");
    let name = shared.name.as_str();

    let records: [Record; 3] = [
        (
            "_mediaremotetv._tcp.local.",
            media,
            vec![
                ("Name", name),
                ("UniqueIdentifier", &unique),
                ("SystemBuildVersion", "22K160"),
                ("LocalAirPlayReceiverPairingIdentity", &airplay_id),
                ("ModelName", "Apple TV"),
                ("AllowPairing", "YES"),
            ],
        ),
        (
            "_companion-link._tcp.local.",
            companion,
            vec![
                ("rpMac", "1"),
                ("rpHA", &mac_id),
                ("rpHN", &rp_hn),
                ("rpVr", SOURCE_VERSION),
                ("rpMd", DEVICE_MODEL),
                ("rpFl", COMPANION_FLAGS),
                ("rpAD", &rp_ad),
                ("rpHI", &rp_hi),
                ("rpBA", &mac),
            ],
        ),
        (
            "_airplay._tcp.local.",
            airplay,
            vec![
                ("deviceid", &mac),
                ("features", "0x5A7FFFF7,0x1E"),
                ("flags", "0x44"),
                ("model", DEVICE_MODEL),
                ("srcvers", SOURCE_VERSION),
                ("vv", "2"),
                ("pi", &identity.identifier),
                ("pk", &airplay_key),
                ("name", name),
            ],
        ),
    ];
    let daemon = ServiceDaemon::new()?;
    let host = format!("jf-{}.local.", mac_id.to_lowercase());
    let mut services = Vec::new();
    for (kind, port, txt) in records {
        let info = ServiceInfo::new(kind, name, &host, IpAddr::V4(ip), port, &txt[..])?;
        services.push(info.get_fullname().to_owned());
        daemon.register(info)?;
    }
    Ok((daemon, services))
}

/// iOS sonde aussi les services MediaRemote et AirPlay : une réponse minimale suffit,
/// les commandes passent toutes par Companion Link.
fn probe_endpoint(listener: TcpListener, airplay: bool) {
    for stream in listener.incoming().flatten() {
        std::thread::spawn(move || answer_probe(stream, airplay));
    }
}

fn answer_probe(mut stream: TcpStream, airplay: bool) {
    let peer = stream.peer_addr().map(|address| address.to_string()).unwrap_or_default();
    trace!("{peer} : sonde {}", if airplay { "AirPlay" } else { "MediaRemote" });
    let mut buffer = [0u8; 1024];
    let wait = if airplay { 3 } else { 60 };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(wait)));
    if airplay {
        if matches!(stream.read(&mut buffer), Ok(1..)) {
            let _ = stream.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 2\r\nConnection: close\r\n\r\nOK",
            );
        }
        return;
    }
    while matches!(stream.read(&mut buffer), Ok(1..)) {}
}
