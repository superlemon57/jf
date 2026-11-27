//! Local network helpers shared by the emulated remotes (Apple TV, Android TV): the
//! address the phone can reach, a Bonjour announcement that follows it, and a protocol
//! log kept in the cache folder.

use std::fs;
use std::io::Write;
use std::net::{IpAddr, Ipv4Addr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use mdns_sd::{IfKind, ServiceDaemon, ServiceInfo};

/// Protocol log, always written to `<name>.log` in the cache folder (the previous one is
/// kept as `.log.1`) so that a problem with a phone leaves a trace without any setup;
/// `JF_LOG=1` also copies it to standard error.
pub struct Log {
    name: &'static str,
    file: Mutex<Option<fs::File>>,
}

const LOG_LIMIT: u64 = 8 * 1024 * 1024;

impl Log {
    pub const fn new(name: &'static str) -> Self {
        Self { name, file: Mutex::new(None) }
    }

    pub fn open(&self) {
        let directory = crate::config::cache_dir();
        let path = directory.join(format!("{}.log", self.name));
        let _ = fs::create_dir_all(&directory);
        let _ = fs::rename(&path, directory.join(format!("{}.log.1", self.name)));
        let since = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
        let header = format!("jf {} — started at {} (Unix seconds)\n", env!("CARGO_PKG_VERSION"), since.as_secs());
        if crate::config::write_private(&path, header.as_bytes()).is_ok() {
            let Ok(file) = fs::OpenOptions::new().append(true).open(&path) else { return };
            *self.file.lock().unwrap() = Some(file);
        }
        clock();
    }

    pub fn line(&self, line: &str) {
        let line = format!("[{} {:9.3}] {line}", self.name, clock());
        if std::env::var_os("JF_LOG").is_some_and(|value| value != "0") {
            eprintln!("{line}");
        }
        if let Some(file) = self.file.lock().unwrap().as_mut() {
            if file.metadata().is_ok_and(|meta| meta.len() < LOG_LIMIT) {
                let _ = writeln!(file, "{line}");
            }
        }
    }
}

/// Seconds since the first log was opened.
fn clock() -> f64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_secs_f64()
}

/// Virtual interfaces (VPN, containers, virtual machines) the phone cannot see.
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

/// Address of the computer on the local network (Wi-Fi or Ethernet), the one the phone
/// can reach. The default route sometimes goes through a VPN: the address of a real
/// network interface is used then.
pub fn local_ip() -> Option<Ipv4Addr> {
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
        // No packet is sent: connecting only picks the outgoing interface.
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

/// Builds the service announced on a given address.
pub type MakeService = dyn Fn(Ipv4Addr) -> Result<ServiceInfo, mdns_sd::Error> + Send + Sync;

/// Bonjour announcement that follows the local address: it moves with the network
/// (DHCP lease, Wi-Fi change), pauses without one and is withdrawn when dropped.
pub struct Announcement {
    current: Arc<Mutex<Option<Advert>>>,
    stop: Arc<AtomicBool>,
    log: &'static Log,
}

struct Advert {
    ip: Ipv4Addr,
    daemon: ServiceDaemon,
    service: String,
}

/// How often the local address is checked.
const ADDRESS_CHECK: Duration = Duration::from_secs(15);

impl Advert {
    fn new(ip: Ipv4Addr, make: &MakeService) -> Result<Self, mdns_sd::Error> {
        let daemon = ServiceDaemon::new()?;
        // Only on the local network interface: never on a VPN or a container bridge.
        daemon.disable_interface(IfKind::All)?;
        daemon.enable_interface(IfKind::Addr(IpAddr::V4(ip)))?;
        // Real Android TVs announce "_androidtvremote2" (16 bytes), over the 15 that the
        // library allows by default; it would drop the service without an error.
        daemon.set_service_name_len_max(30)?;
        let info = make(ip)?;
        let service = info.get_fullname().to_owned();
        daemon.register(info)?;
        Ok(Self { ip, daemon, service })
    }

    /// Withdraws the announcement: otherwise the phone keeps the old one cached (over an
    /// hour) and may aim at a device or an address that no longer exists.
    fn withdraw(self, log: &Log) {
        if let Ok(done) = self.daemon.unregister(&self.service) {
            let _ = done.recv_timeout(Duration::from_millis(500));
        }
        let _ = self.daemon.shutdown();
        log.line(&format!("announcement withdrawn from {}", self.ip));
    }
}

impl Announcement {
    /// Announces right away (to report an error at startup), then keeps it up to date.
    pub fn start(log: &'static Log, make: Box<MakeService>) -> Result<Self, String> {
        let first = match local_ip() {
            Some(ip) => {
                let advert = Advert::new(ip, &*make).map_err(|error| format!("Bonjour: {error}"))?;
                log.line(&format!("announced on {ip}"));
                Some(advert)
            }
            None => {
                log.line("no local network address, announcement pending");
                None
            }
        };
        let current = Arc::new(Mutex::new(first));
        let stop = Arc::new(AtomicBool::new(false));
        let (thread_current, thread_stop) = (current.clone(), stop.clone());
        std::thread::spawn(move || follow(log, &*make, &thread_current, &thread_stop));
        Ok(Self { current, stop, log })
    }
}

fn follow(log: &Log, make: &MakeService, current: &Mutex<Option<Advert>>, stop: &AtomicBool) {
    loop {
        let started = Instant::now();
        while started.elapsed() < ADDRESS_CHECK {
            if stop.load(Ordering::SeqCst) {
                return;
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        let ip = local_ip();
        let mut advert = current.lock().unwrap();
        if ip == advert.as_ref().map(|advert| advert.ip) || stop.load(Ordering::SeqCst) {
            continue;
        }
        if let Some(old) = advert.take() {
            old.withdraw(log);
        }
        match ip {
            Some(ip) => match Advert::new(ip, make) {
                Ok(new) => {
                    log.line(&format!("announced on {ip}"));
                    *advert = Some(new);
                }
                Err(error) => log.line(&format!("Bonjour announcement failed on {ip}: {error}")),
            },
            None => log.line("no local network address, announcement paused"),
        }
    }
}

impl Drop for Announcement {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(advert) = self.current.lock().unwrap().take() {
            advert.withdraw(self.log);
        }
    }
}
