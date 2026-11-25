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
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
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
    links: Links,
}

/// Connexions ouvertes, pour libérer celles qu'iOS laisse sans télécommande.
///
/// iOS (rapportd, sharingd) ouvre des connexions de fond qui ne portent jamais de
/// télécommande ; tant qu'une telle connexion reste ouverte, la télécommande de l'iPhone
/// attend dessus et reste sur « Connexion » (journaux du 25/11/2026, et atv-core).
/// Une connexion qui sert la télécommande n'est jamais fermée par une autre.
#[derive(Default)]
pub(crate) struct Links {
    next: AtomicU64,
    open: Mutex<Vec<Link>>,
}

struct Link {
    id: u64,
    peer: IpAddr,
    controller: Option<Vec<u8>>,
    state: Arc<LinkState>,
}

/// État partagé d'une connexion : utilisée par la télécommande, ou à fermer.
#[derive(Default)]
pub(crate) struct LinkState {
    pub(crate) in_use: AtomicBool,
    pub(crate) close: AtomicBool,
}

impl Links {
    /// Enregistre une nouvelle connexion ; les connexions inutilisées du même appareil
    /// sont fermées, l'iPhone repartant de la nouvelle.
    pub(crate) fn open(&self, peer: IpAddr) -> (u64, Arc<LinkState>) {
        let id = self.next.fetch_add(1, Ordering::SeqCst) + 1;
        let state = Arc::new(LinkState::default());
        let mut open = self.open.lock().unwrap();
        for link in open.iter().filter(|link| link.peer == peer) {
            link.release_if_idle(id);
        }
        open.push(Link { id, peer, controller: None, state: state.clone() });
        (id, state)
    }

    /// L'iPhone `controller` s'est vérifié sur la connexion `id` : ses autres connexions
    /// inutilisées sont fermées (une Apple TV ne garde qu'une connexion par appairage).
    pub(crate) fn verified(&self, id: u64, controller: &[u8]) {
        let mut open = self.open.lock().unwrap();
        for link in open.iter().filter(|link| link.id != id && link.controller.as_deref() == Some(controller)) {
            link.release_if_idle(id);
        }
        if let Some(link) = open.iter_mut().find(|link| link.id == id) {
            link.controller = Some(controller.to_vec());
        }
    }

    pub(crate) fn close(&self, id: u64) {
        self.open.lock().unwrap().retain(|link| link.id != id);
    }
}

impl Link {
    fn release_if_idle(&self, newer: u64) {
        if !self.state.in_use.load(Ordering::SeqCst) && !self.state.close.swap(true, Ordering::SeqCst) {
            trace!("connexion n° {} inutilisée, fermée au profit de la n° {newer}", self.id);
        }
    }
}

pub struct Handle {
    shared: Arc<Shared>,
    advert: Arc<Mutex<Option<Advert>>>,
    stop: Arc<AtomicBool>,
}

/// Annonce Bonjour en cours, sur une adresse du réseau local.
struct Advert {
    ip: Ipv4Addr,
    daemon: ServiceDaemon,
    /// Noms complets des services annoncés, pour les retirer.
    services: Vec<String>,
}

impl Advert {
    /// Retire l'annonce : sans cela, l'iPhone garde l'ancienne en cache (plus d'une heure)
    /// et peut viser un appareil ou une adresse qui n'existe plus.
    fn withdraw(self) {
        for service in &self.services {
            if let Ok(done) = self.daemon.unregister(service) {
                let _ = done.recv_timeout(Duration::from_millis(500));
            }
        }
        let _ = self.daemon.shutdown();
        trace!("annonce retirée de {}", self.ip);
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(advert) = self.advert.lock().unwrap().take() {
            advert.withdraw();
        }
    }
}

/// Intervalle de vérification de l'adresse locale (changement de réseau, bail DHCP).
const ADDRESS_CHECK: Duration = Duration::from_secs(15);

/// Tient l'annonce à jour : elle suit l'adresse locale, réapparaît quand le réseau revient
/// et disparaît quand il n'y en a plus.
fn keep_advertised(shared: Arc<Shared>, port: u16, advert: Arc<Mutex<Option<Advert>>>, stop: Arc<AtomicBool>) {
    while !stop.load(Ordering::SeqCst) {
        let ip = local_ip();
        let current = advert.lock().unwrap().as_ref().map(|advert| advert.ip);
        if ip != current {
            if let Some(old) = advert.lock().unwrap().take() {
                old.withdraw();
            }
            if let Some(ip) = ip {
                match advertise(&shared, ip, port) {
                    Ok((daemon, services)) => {
                        trace!("annoncé sur {ip}, port {port}");
                        *advert.lock().unwrap() = Some(Advert { ip, daemon, services });
                    }
                    Err(error) => trace!("annonce Bonjour impossible sur {ip} : {error}"),
                }
            } else {
                trace!("aucune adresse sur le réseau local, annonce suspendue");
            }
        }
        let started = Instant::now();
        while started.elapsed() < ADDRESS_CHECK && !stop.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(250));
        }
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

/// Interfaces virtuelles (VPN, conteneurs, machines virtuelles) que l'iPhone ne voit pas.
const VIRTUAL_INTERFACES: [&str; 20] = [
    "lo",
    "docker",
    "br-",
    "veth",
    "virbr",
    "vmnet",
    "vboxnet",
    "tailscale",
    "tun",
    "tap",
    "wg",
    "utun",
    "zt",
    "mullvad",
    "nordlynx",
    "proton",
    "podman",
    "cni",
    "incus",
    "lxc",
];

/// Adresse du PC sur le réseau local (Wi-Fi ou Ethernet), celle que l'iPhone peut joindre.
/// La route par défaut passe parfois par un VPN : on retient alors l'adresse d'une vraie
/// carte réseau.
fn local_ip() -> Option<Ipv4Addr> {
    let lan: Vec<Ipv4Addr> = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter(|interface| !VIRTUAL_INTERFACES.iter().any(|prefix| interface.name.starts_with(prefix)))
        .filter_map(|interface| match interface.ip() {
            IpAddr::V4(ip) if !ip.is_loopback() && !ip.is_link_local() && !ip.is_unspecified() => Some(ip),
            _ => None,
        })
        .collect();
    let routed = (|| {
        let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
        // Aucun paquet n'est envoyé : la connexion choisit seulement l'interface de sortie.
        socket.connect((Ipv4Addr::new(192, 0, 2, 1), 9)).ok()?;
        match socket.local_addr().ok()?.ip() {
            IpAddr::V4(ip) => Some(ip),
            IpAddr::V6(_) => None,
        }
    })();
    match routed {
        Some(ip) if lan.contains(&ip) => Some(ip),
        _ => lan.iter().find(|ip| ip.is_private()).or(lan.first()).copied(),
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
        links: Links::default(),
    });
    // Première annonce tout de suite, pour signaler une erreur au lancement.
    let first = match local_ip() {
        Some(ip) => {
            let (daemon, services) = advertise(&shared, ip, port).map_err(|error| format!("Bonjour : {error}"))?;
            trace!("annoncé sur {ip}, port {port}");
            Some(Advert { ip, daemon, services })
        }
        None => {
            trace!("aucune adresse sur le réseau local, annonce en attente");
            None
        }
    };
    let advert = Arc::new(Mutex::new(first));
    let stop = Arc::new(AtomicBool::new(false));
    {
        let (shared, advert, stop) = (shared.clone(), advert.clone(), stop.clone());
        std::thread::spawn(move || keep_advertised(shared, port, advert, stop));
    }
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
    Ok(Handle { shared, advert, stop })
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
