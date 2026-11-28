//! Android TV remote (the remote built into the Google TV app and Android's quick
//! settings): jf presents itself on the local network as a Google TV device, pairs with
//! a code shown on screen, then turns key presses into jf commands.
//!
//! Protocol: Android TV Remote v2, as implemented by androidtvremote2 and documented by
//! OpenRemote (TLS with client certificates, Google's "Polo" pairing on port 6467, the
//! remote itself on port 6466, length-delimited Protocol Buffers messages).

mod cert;
mod pairing;
mod proto;
mod session;

use std::fs;
use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui;
use mdns_sd::ServiceInfo;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{DistinguishedName, SignatureScheme};
use sha2::{Digest, Sha256};

use crate::i18n::t;
use crate::{lan, remote};

/// Ports of a real device: the phone connects to them directly.
const REMOTE_PORT: u16 = 6466;
const PAIRING_PORT: u16 = 6467;
const MAX_CONNECTIONS: usize = 16;
/// How long a code stays on screen.
const PAIRING_WINDOW: Duration = Duration::from_secs(120);
/// Wrong codes allowed before pairing pauses, against brute force.
const PAIRING_ATTEMPTS: u8 = 3;
const PAIRING_COOLDOWN: Duration = Duration::from_secs(30);

/// Protocol log: `android-tv.log` in the cache folder.
static LOG: lan::Log = lan::Log::new("android-tv");

macro_rules! trace {
    ($($argument:tt)*) => {
        $crate::androidtv::LOG.line(&format!($($argument)*))
    };
}
use trace;

/// Code on screen while a phone pairs.
struct PairingWindow {
    code: String,
    connection: u64,
    expires: Instant,
}

#[derive(Default)]
struct PairingState {
    window: Option<PairingWindow>,
    failures: u8,
    blocked_until: Option<Instant>,
}

/// State shared between the connections and the interface.
pub struct Shared {
    name: String,
    directory: PathBuf,
    server_key: cert::PublicKey,
    pairing: Mutex<PairingState>,
    notice: Mutex<Option<String>>,
    commands: Sender<remote::Command>,
    ui: egui::Context,
}

impl Shared {
    fn send(&self, command: remote::Command) {
        if self.commands.send(command).is_ok() {
            self.ui.request_repaint();
        }
    }

    fn client_file(&self, certificate: &[u8]) -> PathBuf {
        let name: String = Sha256::digest(certificate).iter().take(16).map(|byte| format!("{byte:02x}")).collect();
        self.directory.join("clients").join(name)
    }

    fn is_paired(&self, certificate: &[u8]) -> bool {
        self.client_file(certificate).exists()
    }

    /// Shows `code` for the pairing on `connection`, unless pairing is paused.
    fn open_window(&self, connection: u64, code: String) -> bool {
        let mut pairing = self.pairing.lock().unwrap();
        if pairing.blocked_until.is_some_and(|until| Instant::now() < until) {
            return false;
        }
        pairing.window = Some(PairingWindow { code, connection, expires: Instant::now() + PAIRING_WINDOW });
        self.ui.request_repaint();
        true
    }

    fn window_open(&self, connection: u64) -> bool {
        let pairing = self.pairing.lock().unwrap();
        pairing.window.as_ref().is_some_and(|window| window.connection == connection && Instant::now() < window.expires)
    }

    fn close_window(&self, connection: u64) {
        let mut pairing = self.pairing.lock().unwrap();
        if pairing.window.as_ref().is_some_and(|window| window.connection == connection) {
            pairing.window = None;
            self.ui.request_repaint();
        }
    }

    fn reject_code(&self, connection: u64) {
        self.close_window(connection);
        let mut pairing = self.pairing.lock().unwrap();
        pairing.failures += 1;
        if pairing.failures >= PAIRING_ATTEMPTS {
            pairing.failures = 0;
            pairing.blocked_until = Some(Instant::now() + PAIRING_COOLDOWN);
            trace!("too many wrong codes, pairing paused for {}s", PAIRING_COOLDOWN.as_secs());
        }
    }

    fn trust(&self, connection: u64, certificate: &[u8]) -> io::Result<()> {
        crate::config::write_private(&self.client_file(certificate), b"")?;
        self.close_window(connection);
        self.pairing.lock().unwrap().failures = 0;
        *self.notice.lock().unwrap() = Some(t("Android phone paired", "Téléphone Android jumelé").into());
        self.ui.request_repaint();
        Ok(())
    }
}

pub struct Handle {
    shared: Arc<Mutex<Option<Arc<Shared>>>>,
    /// Startup error, shown once.
    error: Arc<Mutex<Option<String>>>,
    _announcement: Arc<Mutex<Option<lan::Announcement>>>,
}

impl Handle {
    /// Code to show while a phone pairs.
    pub fn pairing_code(&self) -> Option<(String, Duration)> {
        let shared = self.shared.lock().unwrap().clone()?;
        let pairing = shared.pairing.lock().unwrap();
        let window = pairing.window.as_ref()?;
        let left = window.expires.checked_duration_since(Instant::now())?;
        Some((window.code.clone(), left))
    }

    pub fn take_notice(&self) -> Option<String> {
        if let Some(error) = self.error.lock().unwrap().take() {
            return Some(error);
        }
        let shared = self.shared.lock().unwrap().clone()?;
        let notice = shared.notice.lock().unwrap().take();
        notice
    }
}

/// Accepts any client certificate during pairing (a real TV does too: the code proves
/// who the phone is), or only the paired ones on the remote port.
#[derive(Debug)]
struct Clients {
    shared: Option<Arc<Shared>>,
    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl std::fmt::Debug for Shared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Shared")
    }
}

impl Clients {
    /// Checks that the phone holds the key of its certificate. The certificate is not
    /// parsed by webpki, which refuses the ones Android generates (critical extension).
    fn verify_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        let invalid = || rustls::Error::InvalidCertificate(rustls::CertificateError::BadSignature);
        let (algorithm, key) = cert::subject_public_key(cert).ok_or_else(invalid)?;
        let mapping = self.provider.signature_verification_algorithms.mapping;
        let (_, candidates) = mapping.iter().find(|(scheme, _)| *scheme == dss.scheme).ok_or_else(invalid)?;
        candidates
            .iter()
            .filter(|candidate| candidate.public_key_alg_id().as_ref() == algorithm)
            .any(|candidate| candidate.verify_signature(key, message, dss.signature()).is_ok())
            .then(rustls::client::danger::HandshakeSignatureValid::assertion)
            .ok_or_else(invalid)
    }
}

impl ClientCertVerifier for Clients {
    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> Result<ClientCertVerified, rustls::Error> {
        match &self.shared {
            Some(shared) if !shared.is_paired(end_entity) => {
                trace!("unpaired phone refused on the remote port: it pairs first");
                Err(rustls::Error::InvalidCertificate(rustls::CertificateError::ApplicationVerificationFailure))
            }
            _ => Ok(ClientCertVerified::assertion()),
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        self.verify_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        self.verify_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

fn tls_config(identity: &cert::Identity, verifier: Clients) -> Result<Arc<rustls::ServerConfig>, String> {
    let provider = verifier.provider.clone();
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(identity.key.clone()));
    let config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|error| error.to_string())?
        .with_client_cert_verifier(Arc::new(verifier))
        .with_single_cert(vec![CertificateDer::from(identity.certificate.clone())], key)
        .map_err(|error| error.to_string())?;
    Ok(Arc::new(config))
}

/// Loads jf's certificate, or creates it the first time.
fn load_identity(directory: &std::path::Path, name: &str) -> Result<cert::Identity, String> {
    let (key_path, certificate_path) = (directory.join("key.der"), directory.join("certificate.der"));
    if let (Ok(key), Ok(certificate)) = (fs::read(&key_path), fs::read(&certificate_path)) {
        return Ok(cert::Identity { key, certificate });
    }
    trace!("creating the device certificate");
    let identity = cert::generate(name)?;
    crate::config::write_private(&key_path, &identity.key).map_err(|error| error.to_string())?;
    crate::config::write_private(&certificate_path, &identity.certificate).map_err(|error| error.to_string())?;
    Ok(identity)
}

type Tls = rustls::StreamOwned<rustls::ServerConnection, TcpStream>;

/// Finishes the TLS handshake (the phone's certificate is known after it).
fn handshake(tls: &mut Tls, timeout: Duration) -> io::Result<()> {
    let deadline = Instant::now() + timeout;
    while tls.conn.is_handshaking() {
        match tls.conn.complete_io(&mut tls.sock) {
            Ok(_) => {}
            Err(error) if waiting(&error) && Instant::now() < deadline => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

/// A read that timed out: nothing arrived yet.
fn waiting(error: &io::Error) -> bool {
    matches!(error.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut)
}

/// Next message, waiting at most the socket's read timeout; `None` if nothing came.
fn receive(tls: &mut Tls, inbox: &mut proto::Inbox) -> io::Result<Option<Vec<u8>>> {
    if let Some(message) = inbox.next()? {
        return Ok(Some(message));
    }
    let mut buffer = [0u8; 4096];
    match tls.read(&mut buffer) {
        Ok(0) => Err(io::Error::new(io::ErrorKind::UnexpectedEof, "closed by the phone")),
        Ok(count) => {
            inbox.push(&buffer[..count]);
            inbox.next()
        }
        Err(error) if waiting(&error) => Ok(None),
        Err(error) => Err(error),
    }
}

fn send(tls: &mut Tls, message: &proto::Message) -> io::Result<()> {
    tls.write_all(&message.delimited())?;
    tls.flush()
}

/// Closes the TLS session properly.
fn finish(tls: &mut Tls) {
    tls.conn.send_close_notify();
    let _ = tls.flush();
}

fn listen(port: u16) -> Result<TcpListener, String> {
    TcpListener::bind((Ipv4Addr::UNSPECIFIED, port)).map_err(|error| format!("port {port}: {error}"))
}

/// Starts the announcement and the connections. Creating the RSA key takes a moment the
/// first time, so everything happens off the interface thread; errors are logged and
/// shown as a notice.
pub fn start(name: &str, directory: PathBuf, commands: Sender<remote::Command>, ui: egui::Context) -> Handle {
    LOG.open();
    let handle = Handle { shared: Arc::default(), error: Arc::default(), _announcement: Arc::default() };
    let (slot, announcement, name) = (handle.shared.clone(), handle._announcement.clone(), name.to_owned());
    let failure = handle.error.clone();
    std::thread::spawn(move || {
        let repaint = ui.clone();
        if let Err(error) = run(name, directory, commands, ui, &slot, &announcement) {
            trace!("unavailable: {error}");
            let message = t("Android TV remote unavailable", "Télécommande Android TV indisponible");
            *failure.lock().unwrap() = Some(format!("{message}: {error}"));
            repaint.request_repaint();
        }
    });
    handle
}

fn run(
    name: String,
    directory: PathBuf,
    commands: Sender<remote::Command>,
    ui: egui::Context,
    slot: &Mutex<Option<Arc<Shared>>>,
    announcement: &Mutex<Option<lan::Announcement>>,
) -> Result<(), String> {
    let identity = load_identity(&directory, &name)?;
    let server_key = cert::key_public(&identity.key).ok_or("invalid key.der")?;
    let (remote_listener, pairing_listener) = (listen(REMOTE_PORT)?, listen(PAIRING_PORT)?);
    let mac = cert::address(&server_key);
    let shared = Arc::new(Shared {
        name,
        directory,
        server_key,
        pairing: Mutex::default(),
        notice: Mutex::default(),
        commands,
        ui,
    });
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let pairing_tls = tls_config(&identity, Clients { shared: None, provider: provider.clone() })?;
    let remote_tls = tls_config(&identity, Clients { shared: Some(shared.clone()), provider })?;
    *slot.lock().unwrap() = Some(shared.clone());

    let service = shared.clone();
    let make = move |ip| service_info(&service.name, &mac, ip);
    *announcement.lock().unwrap() = Some(lan::Announcement::start(&LOG, Box::new(make))?);

    let connections = Arc::new(AtomicUsize::new(0));
    let next_id = Arc::new(AtomicUsize::new(0));
    let accept = |listener: TcpListener, config: Arc<rustls::ServerConfig>, pairing: bool| {
        let (shared, connections, next_id) = (shared.clone(), connections.clone(), next_id.clone());
        move || {
            for stream in listener.incoming().flatten() {
                if connections.load(Ordering::SeqCst) >= MAX_CONNECTIONS {
                    continue;
                }
                let id = next_id.fetch_add(1, Ordering::SeqCst) as u64 + 1;
                let (shared, connections, config) = (shared.clone(), connections.clone(), config.clone());
                connections.fetch_add(1, Ordering::SeqCst);
                std::thread::spawn(move || {
                    let peer = stream.peer_addr().map(|address| address.ip().to_string()).unwrap_or_default();
                    let result = match rustls::ServerConnection::new(config) {
                        Ok(connection) => {
                            let tls = rustls::StreamOwned::new(connection, stream);
                            if pairing {
                                trace!("#{id}: pairing connection from {peer}");
                                pairing::serve(tls, &shared, id)
                            } else {
                                trace!("#{id}: remote connection from {peer}");
                                session::serve(tls, &shared, id)
                            }
                        }
                        Err(error) => Err(io::Error::other(error)),
                    };
                    trace!("#{id}: closed ({})", result.err().map_or("normally".to_owned(), |error| error.to_string()));
                    connections.fetch_sub(1, Ordering::SeqCst);
                });
            }
        }
    };
    std::thread::spawn(accept(pairing_listener, pairing_tls, true));
    std::thread::spawn(accept(remote_listener, remote_tls, false));
    trace!("listening on ports {REMOTE_PORT} (remote) and {PAIRING_PORT} (pairing)");
    Ok(())
}

fn service_info(name: &str, mac: &str, ip: Ipv4Addr) -> Result<ServiceInfo, mdns_sd::Error> {
    let host = format!("jf-{}.local.", mac.replace(':', "").to_lowercase());
    let txt = [("bt", mac)];
    ServiceInfo::new("_androidtvremote2._tcp.local.", name, &host, IpAddr::V4(ip), REMOTE_PORT, &txt[..])
}
