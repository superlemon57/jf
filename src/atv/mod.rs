//! iOS Apple TV remote (Control Center and the Remote app): jf presents itself on the
//! local network as an Apple TV, pairs with a code shown on screen, then turns buttons,
//! gestures and playback commands into jf commands.
//!
//! The protocol (Companion Link) was reverse-engineered by pyatv; the OPACK codec, SRP,
//! TLV and encryption come from myRemote (MIT license), the session is jf's own.

mod archive;
mod crypto;
mod opack;
mod session;
mod srp;
mod tlv;

use std::fs;
use std::io;
use std::net::{IpAddr, Ipv4Addr, TcpListener};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui;
use mdns_sd::ServiceInfo;
use rand::RngCore;
use sha2::{Digest, Sha256};

use crate::{lan, remote};

const DEVICE_MODEL: &str = "AppleTV14,1";
const SOURCE_VERSION: &str = "715.2";
const COMPANION_FLAGS: &str = "0x36782";
/// Usual Companion Link port of an Apple TV; another one is used if it is taken.
const PORT: u16 = 49153;
const PAIRING_WINDOW: Duration = Duration::from_secs(120);
/// Code attempts allowed per pairing window.
const PAIRING_ATTEMPTS: u8 = 3;
/// Delay before a new window after a wrong code, against brute force.
const PAIRING_COOLDOWN: Duration = Duration::from_secs(30);
const MAX_CONNECTIONS: usize = 32;

/// Protocol log: `apple-tv.log` in the cache folder.
pub(crate) static LOG: lan::Log = lan::Log::new("apple-tv");

pub(crate) fn log_line(line: &str) {
    LOG.line(line);
}

macro_rules! trace {
    ($($argument:tt)*) => {
        $crate::atv::log_line(&format!($($argument)*))
    };
}
pub(crate) use trace;

/// Stable identity of the jf "Apple TV", derived from a saved private key.
struct Identity {
    private_key: [u8; 32],
    identifier: String,
    mac: [u8; 6],
}

impl Identity {
    /// Value derived from the public identifier, in lowercase hex like on a real Apple TV
    /// or HomePod (rpHI, rpAD…). The private key never goes into it.
    fn alias(&self, label: &str) -> String {
        let hash = Sha256::new().chain_update(label).chain_update(self.identifier.as_bytes()).finalize();
        hash[..6].iter().map(|byte| format!("{byte:02x}")).collect()
    }

    /// Stable UUID derived from the public identifier.
    fn uuid(&self, label: &str) -> String {
        let hash = Sha256::new().chain_update(label).chain_update(self.identifier.as_bytes()).finalize();
        let hex: String = hash[..16].iter().map(|byte| format!("{byte:02X}")).collect();
        format!("{}-{}-{}-{}-{}", &hex[..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..])
    }

    /// Advertised Bluetooth address (rpBA), also given as `_pubID`.
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

/// State shared between the connections and the interface.
pub struct Shared {
    name: String,
    identity: Identity,
    directory: PathBuf,
    pairing: Mutex<PairingState>,
    /// Message for the interface to show ("iPhone paired").
    notice: Mutex<Option<String>>,
    pub(crate) state: remote::Shared,
    commands: Sender<remote::Command>,
    ui: egui::Context,
    links: Links,
}

/// Open connections, to free the ones iOS leaves without a remote.
///
/// iOS (rapportd, sharingd) opens background connections that never carry a remote;
/// while one stays open, the iPhone's remote waits on it and stays on "Connecting"
/// (seen with iOS 27, and in atv-core). A connection serving the remote is never closed
/// because of another one.
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

/// Shared state of a connection: used by the remote, or to be closed.
#[derive(Default)]
pub(crate) struct LinkState {
    pub(crate) in_use: AtomicBool,
    pub(crate) close: AtomicBool,
}

impl Links {
    /// Registers a new connection; idle connections from the same device are closed, the
    /// iPhone carrying on with the new one.
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

    /// iPhone `controller` verified on connection `id`: its other idle connections are
    /// closed (an Apple TV keeps one connection per pairing).
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
            trace!("connection #{} idle, closed in favor of #{newer}", self.id);
        }
    }
}

pub struct Handle {
    shared: Arc<Shared>,
    /// Withdrawn from the network when jf closes.
    _announcement: lan::Announcement,
}

impl Handle {
    /// Code to show while an iPhone asks to pair.
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

    /// Opens (or resumes) the pairing window and returns the expected code.
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
        let window = pairing.window.as_mut().expect("window open");
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
            return Err("pairing window closed".into());
        }
        crate::config::write_private(&self.controller_file(id), &key).map_err(|error| error.to_string())?;
        pairing.window = None;
        *self.notice.lock().unwrap() = Some(crate::i18n::t("iPhone paired", "iPhone jumelé").into());
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

fn load_identity(directory: &Path) -> io::Result<Identity> {
    let path = directory.join("identity.key");
    let private_key: [u8; 32] = match fs::read(&path) {
        Ok(bytes) => bytes.try_into().map_err(|_| io::Error::other("invalid identity.key"))?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let mut key = [0u8; 32];
            rand::rngs::OsRng.fill_bytes(&mut key);
            crate::config::write_private(&path, &key)?;
            key
        }
        Err(error) => return Err(error),
    };
    let hash = Sha256::new().chain_update(b"jf apple tv identity").chain_update(private_key).finalize();
    let hex: String = hash[..16].iter().map(|byte| format!("{byte:02X}")).collect();
    let identifier = format!("{}-{}-{}-{}-{}", &hex[..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..]);
    let mut mac: [u8; 6] = hash[16..22].try_into().expect("6 octets");
    // Locally administered (bit 2) and not multicast (bit 1).
    mac[0] = (mac[0] | 2) & !1;
    Ok(Identity { private_key, identifier, mac })
}

fn bind(port: u16) -> io::Result<TcpListener> {
    TcpListener::bind((Ipv4Addr::UNSPECIFIED, port)).or_else(|_| TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0)))
}

/// Starts the Bonjour announcement and the connections. `name` is shown on the iPhone.
pub fn start(
    name: &str,
    directory: PathBuf,
    state: remote::Shared,
    commands: Sender<remote::Command>,
    ui: egui::Context,
) -> Result<Handle, String> {
    LOG.open();
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
    let service = shared.clone();
    let announcement = lan::Announcement::start(&LOG, Box::new(move |ip| service_info(&service, ip, port)))?;
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
    Ok(Handle { shared, _announcement: announcement })
}

/// Apple TV's Companion Link service. The remote only needs Companion Link
/// (atvr4samsung, linux-apple-remote): no fake MediaRemote or AirPlay services.
fn service_info(shared: &Shared, ip: Ipv4Addr, port: u16) -> Result<ServiceInfo, mdns_sd::Error> {
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
    let host = format!("jf-{}.local.", mac.replace(':', "").to_lowercase());
    ServiceInfo::new("_companion-link._tcp.local.", &shared.name, &host, IpAddr::V4(ip), port, &txt[..])
}
