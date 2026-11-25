//! Une connexion Companion Link : appairage (SRP et code PIN), vérification, puis canal
//! chiffré qui transporte boutons, gestes du pavé tactile et commandes de lecture.

use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use plist::Value as Plist;
use rand::rngs::OsRng;
use sha2::{Digest, Sha256};
use x25519_dalek::{PublicKey, StaticSecret};

use super::archive::{class, dictionary, uid};
use super::crypto::{hkdf_expand, CompanionCipher};
use super::opack::{self, Value};
use super::srp::SrpServer;
use super::tlv::{self, tags};
use super::{trace, Shared, DEVICE_MODEL, SOURCE_VERSION};
use crate::player::TrackKind;
use crate::remote::{self, Command};

type Result<T> = std::result::Result<T, String>;
type Tlv = std::collections::BTreeMap<u8, Vec<u8>>;

// Types de trames.
const PS_START: u8 = 3;
const PS_NEXT: u8 = 4;
const PV_START: u8 = 5;
const PV_NEXT: u8 = 6;
const U_OPACK: u8 = 7;
const E_OPACK: u8 = 8;
const P_OPACK: u8 = 9;

const MAX_FRAME: usize = 64 * 1024;
const REMOTE_SERVICE: &str = "com.apple.tvremoteservices";
/// Avant vérification, une connexion n'a que le temps d'un appairage.
const UNVERIFIED_LIMIT: Duration = Duration::from_secs(180);
const IDLE_LIMIT: Duration = Duration::from_secs(3600);

// Pavé tactile, en unités d'une surface de 1000 × 1000.
const SURFACE: f64 = 1000.0;
const SWIPE: f64 = 180.0;
const TAP: f64 = 60.0;
/// Un même appui arrive parfois deux fois : fin de toucher puis bouton « select ».
const SELECT_ECHO: Duration = Duration::from_millis(120);
/// Appui long (pavé ou bouton central) : menu contextuel, comme sur une Apple TV.
const LONG_PRESS: Duration = Duration::from_millis(600);
// Flèche maintenue : répétition après un délai.
const REPEAT_DELAY: Duration = Duration::from_millis(400);
const REPEAT_EVERY: Duration = Duration::from_millis(130);
// Avance et retour rapides maintenus.
const SEEK_STEP: f64 = 10.0;
const SEEK_EVERY: Duration = Duration::from_millis(500);
const SEEK_LIMIT: Duration = Duration::from_secs(10);

// Commandes disponibles annoncées à l'iPhone.
const FLAG_PLAY: u64 = 0x0001;
const FLAG_PAUSE: u64 = 0x0002;
const FLAG_FAST_FORWARD: u64 = 0x0010;
const FLAG_REWIND: u64 = 0x0020;
const FLAG_VOLUME: u64 = 0x0100;
const FLAG_SKIP_FORWARD: u64 = 0x0200;
const FLAG_SKIP_BACKWARD: u64 = 0x0400;

#[derive(Clone, Copy, PartialEq)]
enum Source {
    Touch,
    Button,
}

struct Touch {
    origin: (f64, f64),
    last: (f64, f64),
    /// Distance maximale parcourue depuis le début du geste.
    travel: f64,
    swiped: bool,
    started: Instant,
    /// L'appui long a déjà ouvert le menu : la fin du geste ne valide rien.
    long_pressed: bool,
}

/// Bouton central enfoncé, pour reconnaître l'appui long.
struct SelectDown {
    since: Instant,
    long_pressed: bool,
}

struct Held {
    key: &'static str,
    since: Instant,
    last: Option<Instant>,
}

struct Seeking {
    offset: f64,
    next: Instant,
    until: Instant,
}

struct PendingVerify {
    shared: [u8; 32],
    client_public: [u8; 32],
    server_public: [u8; 32],
}

/// Ce qui a été annoncé à l'iPhone, pour ne renvoyer que les changements.
#[derive(Clone, PartialEq)]
struct Published {
    playing: bool,
    paused: bool,
    live: bool,
    title: String,
    duration: i64,
    captions: Option<bool>,
}

struct Session {
    stream: TcpStream,
    buffer: Vec<u8>,
    shared: Arc<Shared>,
    signing: SigningKey,
    setup: Option<(SrpServer, u64)>,
    setup_proved: bool,
    pending: Option<PendingVerify>,
    cipher: Option<CompanionCipher>,
    session_id: u32,
    next_event: u32,
    identifier: Option<String>,
    remote_open: bool,
    published: Option<Published>,
    touch: Option<Touch>,
    touch_size: (f64, f64),
    last_select: Option<(Instant, Source)>,
    held: Option<Held>,
    select_down: Option<SelectDown>,
    seeking: Option<Seeking>,
    accepted: Instant,
    activity: Instant,
}

pub(super) fn serve(stream: TcpStream, shared: Arc<Shared>) {
    let peer = stream.peer_addr().map(|address| address.to_string()).unwrap_or_default();
    trace!("{peer} : connexion");
    let mut session = Session {
        buffer: Vec::new(),
        signing: SigningKey::from_bytes(&shared.identity.private_key),
        shared,
        stream,
        setup: None,
        setup_proved: false,
        pending: None,
        cipher: None,
        session_id: 0,
        next_event: 0x8000_0000 | rand::random::<u32>(),
        identifier: None,
        remote_open: false,
        published: None,
        touch: None,
        touch_size: (SURFACE, SURFACE),
        last_select: None,
        held: None,
        select_down: None,
        seeking: None,
        accepted: Instant::now(),
        activity: Instant::now(),
    };
    if let Err(error) = session.run() {
        trace!("{peer} : fin ({error})");
    }
}

fn field<'a>(value: &'a Value, key: &str) -> Result<&'a [u8]> {
    value.get(key).and_then(Value::as_bytes).ok_or_else(|| format!("champ {key} absent"))
}

fn tlv_field(data: &Tlv, tag: u8) -> Result<&[u8]> {
    data.get(&tag).map(Vec::as_slice).ok_or_else(|| format!("champ d'appairage {tag} absent"))
}

fn check_signature(key: &VerifyingKey, data: &Tlv, message: &[u8]) -> Result<()> {
    let signature = Signature::from_slice(tlv_field(data, tags::SIGNATURE)?).map_err(|_| "signature invalide")?;
    key.verify_strict(message, &signature).map_err(|_| "signature refusée".to_owned())
}

fn verification_cipher(shared: &[u8; 32]) -> Result<CompanionCipher> {
    let key = hkdf_expand("Pair-Verify-Encrypt-Salt", "Pair-Verify-Encrypt-Info", shared);
    CompanionCipher::new(&key, &key, 8)
}

fn awake() -> Value {
    Value::dict(vec![Value::kv("state", Value::Int(3))])
}

fn hid_button(code: u64) -> Option<&'static str> {
    Some(match code {
        1 => "up",
        2 => "down",
        3 => "left",
        4 => "right",
        5 => "menu",
        6 => "select",
        7 => "home",
        8 => "volume_up",
        9 => "volume_down",
        14 => "play_pause",
        15 => "channel_up",
        16 => "channel_down",
        17 => "guide",
        18 => "mute",
        _ => return None,
    })
}

impl Session {
    fn run(&mut self) -> Result<()> {
        self.stream.set_nodelay(true).map_err(|error| error.to_string())?;
        loop {
            // Délai de lecture court tant qu'une touche est maintenue, pour la répétition.
            let pressing = self.select_down.is_some() || self.touch.as_ref().is_some_and(|touch| !touch.swiped);
            let wait = if self.held.is_some() || self.seeking.is_some() || pressing { 40 } else { 500 };
            let _ = self.stream.set_read_timeout(Some(Duration::from_millis(wait)));
            if let Some((header, payload)) = self.read_frame()? {
                self.handle_frame(header, &payload)?;
            }
            self.repeat();
            let limit = if self.cipher.is_some() { IDLE_LIMIT } else { UNVERIFIED_LIMIT };
            let since = if self.cipher.is_some() { self.activity } else { self.accepted };
            if since.elapsed() > limit {
                return Err("connexion inactive".into());
            }
            if self.remote_open {
                self.publish(false)?;
            }
        }
    }

    /// Prochaine trame complète, ou rien si aucune n'est arrivée dans le délai.
    fn read_frame(&mut self) -> Result<Option<([u8; 4], Vec<u8>)>> {
        loop {
            if self.buffer.len() >= 4 {
                let size = u32::from_be_bytes([0, self.buffer[1], self.buffer[2], self.buffer[3]]) as usize;
                if size > MAX_FRAME {
                    return Err("trame trop grande".into());
                }
                if self.buffer.len() >= 4 + size {
                    let header: [u8; 4] = self.buffer[..4].try_into().expect("4 octets");
                    let payload = self.buffer[4..4 + size].to_vec();
                    self.buffer.drain(..4 + size);
                    self.activity = Instant::now();
                    return Ok(Some((header, payload)));
                }
            }
            let mut chunk = [0u8; 16 * 1024];
            match self.stream.read(&mut chunk) {
                Ok(0) => return Err("connexion fermée".into()),
                Ok(read) => self.buffer.extend_from_slice(&chunk[..read]),
                Err(error) if matches!(error.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {
                    return Ok(None)
                }
                Err(error) => return Err(error.to_string()),
            }
        }
    }

    // ---------------------------------------------------------------- envoi

    fn send_frame(&mut self, kind: u8, payload: &[u8]) -> Result<()> {
        let mut out = Vec::with_capacity(4 + payload.len());
        out.push(kind);
        out.extend_from_slice(&(payload.len() as u32).to_be_bytes()[1..]);
        out.extend_from_slice(payload);
        self.stream.write_all(&out).map_err(|error| error.to_string())
    }

    fn auth_reply(&mut self, kind: u8, entries: &[(u8, &[u8])]) -> Result<()> {
        let message = Value::dict(vec![Value::kv("_pd", Value::Bytes(tlv::write(entries)))]);
        self.send_frame(kind, &opack::pack(&message))
    }

    fn send_encrypted(&mut self, message: &Value) -> Result<()> {
        let cipher = self.cipher.as_mut().ok_or("session non chiffrée")?;
        let plain = opack::pack(message);
        let mut header = vec![E_OPACK];
        header.extend_from_slice(&((plain.len() + 16) as u32).to_be_bytes()[1..]);
        let encrypted = cipher.encrypt(&plain, Some(&header), None)?;
        header.extend_from_slice(&encrypted);
        self.stream.write_all(&header).map_err(|error| error.to_string())
    }

    fn respond(&mut self, xid: Option<u64>, content: Option<Value>, result_type: u64) -> Result<()> {
        let mut response =
            vec![Value::kv("_c", content.unwrap_or(Value::dict(vec![]))), Value::kv("_t", Value::Int(3))];
        let bare =
            matches!(self.identifier.as_deref(), Some("_systemInfo" | "SystemInfo" | "_sessionStart" | "_sessionStop"));
        if !bare {
            response.push(Value::kv("_rT", Value::Int(result_type)));
        }
        if let Some(xid) = xid {
            response.push(Value::kv("_x", Value::Int(xid)));
        }
        self.send_encrypted(&Value::Dict(response))
    }

    fn event(&mut self, name: &str, content: Value) -> Result<()> {
        let xid = self.next_event;
        self.next_event = self.next_event.wrapping_add(1) | 0x8000_0000;
        let event = Value::dict(vec![
            Value::kv("_i", Value::str(name)),
            Value::kv("_t", Value::Int(1)),
            Value::kv("_c", content),
            Value::kv("_x", Value::Int(xid.into())),
        ]);
        self.send_encrypted(&event)
    }

    // ---------------------------------------------------------------- trames

    fn handle_frame(&mut self, header: [u8; 4], payload: &[u8]) -> Result<()> {
        let kind = header[0];
        if self.cipher.is_none() {
            trace!("trame {kind} ({} octets)", payload.len());
        }
        if let Some(cipher) = self.cipher.as_mut() {
            if payload.is_empty() {
                return Ok(());
            }
            let plain = cipher.decrypt(payload, Some(&header), None)?;
            if matches!(kind, U_OPACK | E_OPACK | P_OPACK) {
                if let Ok((message, _)) = opack::unpack(&plain) {
                    self.handle_message(&message)?;
                }
            }
            return Ok(());
        }
        if !matches!(kind, PS_START | PS_NEXT | PV_START | PV_NEXT) {
            return Ok(());
        }
        let (message, _) = opack::unpack(payload).map_err(|error| error.to_string())?;
        let data = tlv::read(field(&message, "_pd")?);
        match (kind, data.get(&tags::SEQ_NO).map(Vec::as_slice)) {
            (PS_START, Some([1])) => self.pair_setup_1(),
            (PS_NEXT, Some([3])) => self.pair_setup_3(&data),
            (PS_NEXT, Some([5])) => self.pair_setup_5(&data),
            (PV_START, Some([1])) => self.pair_verify_1(&data),
            (PV_NEXT, Some([3])) => self.pair_verify_3(&data),
            _ => Err("étape d'appairage inattendue".into()),
        }
    }

    // ---------------------------------------------------------------- appairage (code PIN)

    fn pair_setup_1(&mut self) -> Result<()> {
        self.setup = None;
        self.setup_proved = false;
        let Some((pin, generation)) = self.shared.begin_pairing() else {
            trace!("appairage refusé : attente après des codes faux");
            return self.auth_reply(PS_NEXT, &[(tags::SEQ_NO, &[2]), (tags::ERROR, &[6])]);
        };
        let srp = SrpServer::new(pin, &rand::random::<[u8; 32]>());
        let reply = tlv::write(&[
            (tags::SEQ_NO, &[2]),
            (tags::SALT, srp.salt()),
            (tags::PUBLIC_KEY, &srp.public_key()),
            (27, &[1]),
        ]);
        self.setup = Some((srp, generation));
        let message = Value::dict(vec![Value::kv("_pd", Value::Bytes(reply)), Value::kv("_pwTy", Value::Int(1))]);
        self.send_frame(PS_NEXT, &opack::pack(&message))
    }

    fn pair_setup_3(&mut self, data: &Tlv) -> Result<()> {
        let (mut srp, generation) = self.setup.take().filter(|_| !self.setup_proved).ok_or("appairage non commencé")?;
        let public_key = tlv_field(data, tags::PUBLIC_KEY)?;
        if public_key.len() > 384 {
            return Err("clé SRP invalide".into());
        }
        srp.process(public_key)?;
        if !srp.verify_proof(tlv_field(data, tags::PROOF)?) {
            trace!("code faux");
            self.shared.reject_code();
            return self.auth_reply(PS_NEXT, &[(tags::SEQ_NO, &[4]), (tags::ERROR, &[2])]);
        }
        let proof = srp.key_proof_hash()?;
        self.setup = Some((srp, generation));
        self.setup_proved = true;
        self.auth_reply(PS_NEXT, &[(tags::SEQ_NO, &[4]), (tags::PROOF, &proof)])
    }

    fn pair_setup_5(&mut self, data: &Tlv) -> Result<()> {
        let (srp, generation) = self.setup.take().filter(|_| self.setup_proved).ok_or("code non vérifié")?;
        let secret = srp.session_key()?;
        let key = hkdf_expand("Pair-Setup-Encrypt-Salt", "Pair-Setup-Encrypt-Info", secret);
        let mut cipher = CompanionCipher::new(&key, &key, 8)?;
        let controller = tlv::read(&cipher.decrypt(tlv_field(data, tags::ENCRYPTED_DATA)?, None, Some(b"PS-Msg05"))?);
        let id = tlv_field(&controller, tags::IDENTIFIER)?;
        if id.is_empty() || id.len() > 256 {
            return Err("identifiant de l'iPhone invalide".into());
        }
        let public: [u8; 32] = tlv_field(&controller, tags::PUBLIC_KEY)?.try_into().map_err(|_| "clé invalide")?;
        let verifying = VerifyingKey::from_bytes(&public).map_err(|_| "clé invalide")?;
        let controller_x = hkdf_expand("Pair-Setup-Controller-Sign-Salt", "Pair-Setup-Controller-Sign-Info", secret);
        check_signature(&verifying, &controller, &[&controller_x, id, &public].concat())?;

        let accessory_x = hkdf_expand("Pair-Setup-Accessory-Sign-Salt", "Pair-Setup-Accessory-Sign-Info", secret);
        let identity = &self.shared.identity;
        let server_id = identity.identifier.as_bytes();
        let server_public = self.signing.verifying_key().to_bytes();
        let signature = self.signing.sign(&[&accessory_x[..], server_id, &server_public].concat());
        let extra = Value::dict(vec![
            Value::kv("altIRK", Value::Bytes(hkdf_expand("jf", "altIRK", &identity.private_key)[..16].to_vec())),
            Value::kv("accountID", Value::str(&identity.identifier)),
            Value::kv("model", Value::str(DEVICE_MODEL)),
            Value::kv("wifiMAC", Value::Bytes(identity.mac.to_vec())),
            Value::kv("name", Value::str(&self.shared.name)),
            Value::kv("mac", Value::Bytes(identity.mac.to_vec())),
        ]);
        let reply = tlv::write(&[
            (tags::IDENTIFIER, server_id),
            (tags::PUBLIC_KEY, &server_public),
            (tags::SIGNATURE, &signature.to_bytes()),
            (17, &opack::pack(&extra)),
        ]);
        let encrypted = cipher.encrypt(&reply, None, Some(b"PS-Msg06"))?;
        self.shared.trust(generation, id, public)?;
        self.auth_reply(PS_NEXT, &[(tags::SEQ_NO, &[6]), (tags::ENCRYPTED_DATA, &encrypted)])
    }

    fn pair_verify_1(&mut self, data: &Tlv) -> Result<()> {
        let client_public: [u8; 32] =
            tlv_field(data, tags::PUBLIC_KEY)?.try_into().map_err(|_| "clé X25519 invalide")?;
        let secret = StaticSecret::random_from_rng(OsRng);
        let server_public = PublicKey::from(&secret).to_bytes();
        let shared = secret.diffie_hellman(&PublicKey::from(client_public));
        if !shared.was_contributory() {
            return Err("échange X25519 invalide".into());
        }
        let shared = shared.to_bytes();
        let server_id = self.shared.identity.identifier.as_bytes();
        let signature = self.signing.sign(&[&server_public[..], server_id, &client_public].concat());
        let reply = tlv::write(&[(tags::IDENTIFIER, server_id), (tags::SIGNATURE, &signature.to_bytes())]);
        let encrypted = verification_cipher(&shared)?.encrypt(&reply, None, Some(b"PV-Msg02"))?;
        self.pending = Some(PendingVerify { shared, client_public, server_public });
        self.auth_reply(
            PV_NEXT,
            &[(tags::SEQ_NO, &[2]), (tags::PUBLIC_KEY, &server_public), (tags::ENCRYPTED_DATA, &encrypted)],
        )
    }

    fn pair_verify_3(&mut self, data: &Tlv) -> Result<()> {
        let pending = self.pending.take().ok_or("vérification non commencée")?;
        let controller = tlv::read(&verification_cipher(&pending.shared)?.decrypt(
            tlv_field(data, tags::ENCRYPTED_DATA)?,
            None,
            Some(b"PV-Msg03"),
        )?);
        let id = tlv_field(&controller, tags::IDENTIFIER)?;
        let known = self.shared.controller_key(id).and_then(|key| VerifyingKey::from_bytes(&key).ok());
        let verified = known.is_some_and(|key| {
            check_signature(&key, &controller, &[&pending.client_public[..], id, &pending.server_public].concat())
                .is_ok()
        });
        if !verified {
            // iPhone inconnu : iOS propose alors de saisir un nouveau code.
            trace!("vérification refusée : iPhone inconnu ou signature invalide");
            return self.auth_reply(PV_NEXT, &[(tags::SEQ_NO, &[4]), (tags::ERROR, &[2])]);
        }
        let digest = Sha256::new().chain_update(self.shared.identity.identifier.as_bytes()).chain_update(id).finalize();
        self.session_id = u32::from_le_bytes(digest[..4].try_into().expect("4 octets")).max(1);
        self.auth_reply(PV_NEXT, &[(tags::SEQ_NO, &[4])])?;
        self.cipher = Some(CompanionCipher::new(
            &hkdf_expand("", "ServerEncrypt-main", &pending.shared),
            &hkdf_expand("", "ClientEncrypt-main", &pending.shared),
            12,
        )?);
        trace!("iPhone vérifié, session chiffrée");
        Ok(())
    }

    // ---------------------------------------------------------------- messages chiffrés

    fn handle_message(&mut self, message: &Value) -> Result<()> {
        let is_event = match message.get("_t").and_then(Value::as_int) {
            Some(1) => true,
            Some(2) => false,
            _ => return Ok(()),
        };
        self.identifier = message
            .get("_i")
            .and_then(Value::as_str)
            .or_else(|| message.get("model").and_then(Value::as_str))
            .map(str::to_owned);
        let xid = message.get("_x").and_then(Value::as_int);
        let content = message.get("_c").unwrap_or(&Value::Null);
        trace!("message {:?} ({}) : {content}", self.identifier, if is_event { "événement" } else { "requête" });
        let reply = |session: &mut Self| if is_event { Ok(()) } else { session.respond(xid, None, 0) };

        match self.identifier.clone().as_deref() {
            Some("_sessionStart") => {
                if content.get("_srvT").is_none_or(|service| service.as_str() == Some(REMOTE_SERVICE)) {
                    self.remote_open = true;
                }
                let content = Value::dict(vec![Value::kv("_sid", Value::Int(self.session_id.into()))]);
                self.respond(xid, Some(content), 0)?;
            }
            Some("TVRCSessionStart") => {
                self.remote_open = true;
                let version = content.get("ProtocolVersionKey").and_then(Value::as_str).unwrap_or("1.2").to_owned();
                let content = Value::dict(vec![Value::kv("ProtocolVersionKey", Value::str(&version))]);
                self.respond(xid, Some(content), 0)?;
                self.publish(true)?;
            }
            Some(name @ ("_sessionStop" | "TVRCSessionStop" | "_touchStop" | "HIDTouchStop")) => {
                self.touch = None;
                self.held = None;
                self.seeking = None;
                if matches!(name, "_sessionStop" | "TVRCSessionStop") {
                    self.remote_open = false;
                    self.published = None;
                }
                self.respond(xid, None, 0)?;
            }
            Some(name @ ("_touchStart" | "HIDTouchStart")) => {
                let (key, width, height) = if name == "HIDTouchStart" {
                    ("Identifier", "Width", "Height")
                } else {
                    ("_i", "_width", "_height")
                };
                let size = |key| {
                    content
                        .get(key)
                        .and_then(Value::as_float)
                        .filter(|value| (1.0..=4000.0).contains(value))
                        .unwrap_or(SURFACE)
                };
                self.touch = None;
                self.touch_size = (size(width), size(height));
                self.respond(xid, Some(Value::dict(vec![Value::kv(key, Value::Int(1))])), 0)?;
            }
            Some(name @ ("_hidC" | "HIDCommand")) => {
                reply(self)?;
                let (command, state) =
                    if name == "HIDCommand" { ("HIDCommand", "HIDButtonState") } else { ("_hidC", "_hBtS") };
                let button = content.get(command).and_then(Value::as_int).and_then(hid_button);
                let state = content.get(state).and_then(Value::as_int);
                if let Some(button) = button {
                    self.button(button, state)?;
                }
            }
            Some(name @ ("_hidT" | "HIDTouch")) => {
                self.touch_event(content, name == "HIDTouch");
                reply(self)?;
            }
            Some("_interest") => {
                if let Some(Value::Array(events)) = content.get("_regEvents") {
                    let events: Vec<String> = events.iter().filter_map(Value::as_str).map(str::to_owned).collect();
                    for event in events {
                        match event.as_str() {
                            "MediaControlStatus" | "_iMC" | "NowPlayingInfo" => self.publish(true)?,
                            "SystemStatus" | "TVSystemStatus" => self.event(&event, awake())?,
                            _ => {}
                        }
                    }
                }
            }
            Some("_systemInfo" | "SystemInfo") => {
                let info = self.system_info();
                self.respond(xid, Some(info), 0)?;
                self.event("SystemStatus", awake())?;
                self.event("TVSystemStatus", awake())?;
            }
            Some("FetchAttentionState") => self.respond(xid, Some(awake()), 0)?,
            Some("FetchMediaControlStatus") => {
                let flags = self.flags_value();
                self.respond(xid, Some(flags), 0)?;
            }
            Some("FetchCurrentNowPlayingInfoEvent") => {
                reply(self)?;
                self.publish(true)?;
            }
            Some("MediaControlCommand" | "_mcc") => self.media_command(xid, content, is_event)?,
            Some("ToggleCaptions") => {
                self.toggle_captions(None);
                reply(self)?;
            }
            Some("FetchLaunchableApplicationsEvent") => {
                let content = Value::dict(vec![
                    Value::kv("AppCountKey", Value::Int(0)),
                    Value::kv("AppInfos", Value::dict(vec![])),
                ]);
                self.respond(xid, Some(content), 2)?;
            }
            _ if xid.is_some() && !is_event => self.respond(xid, None, 0)?,
            _ => {}
        }
        Ok(())
    }

    fn system_info(&self) -> Value {
        let identity = &self.shared.identity;
        let mac = identity.mac.map(|byte| format!("{byte:02X}")).join(":");
        Value::dict(vec![
            Value::kv("_sf", Value::Int(65536)),
            Value::kv("_cf", Value::Int(512)),
            Value::kv("_i", Value::str(&super::hex_upper(&identity.mac))),
            Value::kv("_pubID", Value::str(&mac)),
            Value::kv("_spID", Value::str(&identity.identifier)),
            Value::kv("_mrID", Value::str(&identity.identifier)),
            Value::kv("_stA", Value::Array(vec![Value::str(REMOTE_SERVICE)])),
            Value::kv("_sv", Value::str(SOURCE_VERSION)),
            Value::kv("model", Value::str(DEVICE_MODEL)),
            Value::kv("name", Value::str(&self.shared.name)),
            Value::kv("_lP", Value::Int(self.shared.companion_port.into())),
        ])
    }

    // ---------------------------------------------------------------- entrées

    /// Bouton de la télécommande. `state` : 1 appuyé, 2 relâché (absent : simple appui).
    fn button(&mut self, button: &'static str, state: Option<u64>) -> Result<()> {
        let direction = matches!(button, "up" | "down" | "left" | "right");
        match state {
            Some(1) if direction => {
                self.shared.key(button);
                self.held = Some(Held { key: button, since: Instant::now(), last: None });
                return Ok(());
            }
            Some(1) if button == "select" => {
                self.select_down = Some(SelectDown { since: Instant::now(), long_pressed: false });
                return Ok(());
            }
            Some(1) => return Ok(()),
            Some(2) | None if direction => {
                // Déjà envoyée à l'appui ; seul un relâchement isolé compte comme un appui.
                if self.held.take().is_none_or(|held| held.key != button) {
                    self.shared.key(button);
                }
                return Ok(());
            }
            Some(2) | None => {}
            _ => return Ok(()),
        }
        match button {
            "select" => {
                // Après un appui long, le relâchement ne valide pas en plus.
                if !self.select_down.take().is_some_and(|down| down.long_pressed) {
                    self.select(Source::Button);
                }
            }
            "menu" => self.shared.key("back"),
            "home" => self.shared.key("home"),
            "play_pause" => self.shared.key("playpause"),
            "channel_up" => self.shared.key("next"),
            "channel_down" => self.shared.key("previous"),
            "guide" => self.shared.key("menu"),
            "volume_up" | "volume_down" => {
                remote::change_volume(if button == "volume_up" { 1.0 / 16.0 } else { -1.0 / 16.0 });
                self.publish(true)?;
            }
            "mute" => remote::toggle_mute(),
            _ => {}
        }
        Ok(())
    }

    fn select(&mut self, source: Source) {
        let now = Instant::now();
        // Écho du même appui par l'autre canal : ignoré une fois.
        if let Some((at, previous)) = self.last_select {
            if previous != source && now.duration_since(at) < SELECT_ECHO {
                self.last_select = None;
                return;
            }
        }
        self.last_select = Some((now, source));
        self.shared.key("ok");
    }

    /// Pavé tactile : un glissement déplace d'un cran (et continue s'il se prolonge),
    /// un toucher bref valide.
    fn touch_event(&mut self, content: &Value, modern: bool) {
        let (phase_key, x_key, y_key) =
            if modern { ("TouchPhase", "CoordinateX", "CoordinateY") } else { ("_tPh", "_cx", "_cy") };
        #[derive(PartialEq)]
        enum Phase {
            Began,
            Moved,
            Ended,
        }
        let phase = match (modern, content.get(phase_key).and_then(Value::as_int)) {
            (_, Some(1)) => Phase::Began,
            (true, Some(2 | 5)) | (false, Some(2 | 3)) => Phase::Moved,
            (true, Some(3)) | (false, Some(4)) => Phase::Ended,
            _ => {
                self.touch = None;
                return;
            }
        };
        let coordinate =
            |key| content.get(key).and_then(Value::as_float).filter(|value| value.is_finite() && value.abs() <= 4000.0);
        let (width, height) = self.touch_size;
        let point = coordinate(x_key)
            .zip(coordinate(y_key))
            .map(|(x, y)| (x * SURFACE / width, y * SURFACE / height))
            .or_else(|| self.touch.as_ref().map(|touch| touch.last).filter(|_| phase == Phase::Ended));
        let Some(point) = point else { return };

        if phase == Phase::Began {
            let started = Instant::now();
            self.touch =
                Some(Touch { origin: point, last: point, travel: 0.0, swiped: false, started, long_pressed: false });
            return;
        }
        let Some(touch) = &mut self.touch else { return };
        touch.last = point;
        let (dx, dy) = (point.0 - touch.origin.0, point.1 - touch.origin.1);
        touch.travel = touch.travel.max(dx.abs().max(dy.abs()));
        if dx.abs().max(dy.abs()) >= SWIPE {
            let key = match (dx.abs() > dy.abs(), dx > 0.0, dy > 0.0) {
                (true, true, _) => "right",
                (true, false, _) => "left",
                (false, _, true) => "down",
                (false, _, false) => "up",
            };
            // Le geste continue depuis ici : un long glissement avance de plusieurs cartes.
            touch.origin = point;
            touch.swiped = true;
            self.shared.key(key);
        }
        if phase == Phase::Ended {
            let tap = !touch.swiped && touch.travel < TAP;
            let long_pressed = touch.long_pressed;
            self.touch = None;
            if long_pressed {
                // L'iPhone peut doubler la fin du geste d'un bouton « select » : ignoré.
                self.last_select = Some((Instant::now(), Source::Touch));
            } else if tap {
                self.select(Source::Touch);
            }
        }
    }

    /// Répète une flèche maintenue et l'avance ou le retour rapides.
    fn repeat(&mut self) {
        let now = Instant::now();
        // Appui long : le menu s'ouvre pendant l'appui, sans attendre le relâchement.
        let touch_long = self.touch.as_mut().filter(|touch| {
            !touch.swiped
                && !touch.long_pressed
                && touch.travel < TAP
                && now.duration_since(touch.started) >= LONG_PRESS
        });
        if let Some(touch) = touch_long {
            touch.long_pressed = true;
            self.shared.key("menu");
        }
        if let Some(down) = self.select_down.as_mut().filter(|down| !down.long_pressed) {
            if now.duration_since(down.since) >= LONG_PRESS {
                down.long_pressed = true;
                self.shared.key("menu");
            }
        }
        if let Some(held) = &mut self.held {
            let due = held.last.map_or(held.since + REPEAT_DELAY, |last| last + REPEAT_EVERY);
            if now >= due {
                held.last = Some(now);
                self.shared.key(held.key);
            }
            // Sans relâchement reçu, on arrête au bout d'un moment.
            if now.duration_since(held.since) > Duration::from_secs(15) {
                self.held = None;
            }
        }
        if let Some(seeking) = &mut self.seeking {
            if now >= seeking.until {
                self.seeking = None;
            } else if now >= seeking.next {
                seeking.next = now + SEEK_EVERY;
                let offset = seeking.offset;
                self.shared.send(Command::SeekBy(offset));
            }
        }
    }

    // ---------------------------------------------------------------- lecture

    fn media_command(&mut self, xid: Option<u64>, content: &Value, is_event: bool) -> Result<()> {
        let volume_key =
            if self.identifier.as_deref() == Some("MediaControlCommand") { "MessageKeyVolume" } else { "_vol" };
        let command = content.get("MediaControlCommand").or_else(|| content.get("_mcc")).and_then(Value::as_int);
        let mut response = None;
        match command {
            Some(1) => self.shared.key("play"),
            Some(2) => self.shared.key("pause"),
            Some(5) => {
                let volume = remote::volume().unwrap_or(0.5);
                response = Some(Value::dict(vec![Value::kv(volume_key, Value::Double(volume))]));
            }
            Some(6) => {
                let volume = content.get(volume_key).or_else(|| content.get("_vol")).and_then(Value::as_float);
                if let Some(volume) = volume.filter(|volume| volume.is_finite()) {
                    remote::set_volume(volume.clamp(0.0, 1.0));
                }
            }
            Some(7) => {
                let seconds =
                    content.get("_skpS").or_else(|| content.get("MessageKeySkipSeconds")).and_then(Value::as_float);
                if let Some(seconds) = seconds.filter(|seconds| seconds.is_finite() && *seconds != 0.0) {
                    self.shared.send(Command::SeekBy(seconds));
                }
            }
            Some(code @ (8 | 10)) => {
                let offset = if code == 8 { SEEK_STEP } else { -SEEK_STEP };
                let now = Instant::now();
                self.shared.send(Command::SeekBy(offset));
                self.seeking = Some(Seeking { offset, next: now + SEEK_EVERY, until: now + SEEK_LIMIT });
            }
            Some(9 | 11) => self.seeking = None,
            Some(12) => {
                let enabled = self.shared.state.lock().unwrap().subtitles.iter().any(|track| track.selected);
                response = Some(Value::dict(vec![Value::kv("_cse", Value::Bool(enabled))]));
            }
            Some(13) => {
                if let Some(Value::Bool(enabled)) = content.get("_cse") {
                    self.toggle_captions(Some(*enabled));
                }
            }
            _ => {}
        }
        if is_event {
            return Ok(());
        }
        self.respond(xid, response, 0)
    }

    /// Active les premiers sous-titres disponibles, ou les coupe.
    fn toggle_captions(&self, wanted: Option<bool>) {
        let (enabled, first) = {
            let state = self.shared.state.lock().unwrap();
            (state.subtitles.iter().any(|track| track.selected), state.subtitles.first().map(|track| track.id))
        };
        let enable = wanted.unwrap_or(!enabled);
        let id = if enable { first } else { None };
        if enable && id.is_none() {
            return;
        }
        self.shared.send(Command::Track(TrackKind::Subtitle, id));
    }

    fn current(&self) -> Published {
        let state = self.shared.state.lock().unwrap();
        Published {
            playing: state.playing,
            paused: state.paused,
            live: state.live,
            title: state.title.clone(),
            duration: state.duration.round() as i64,
            captions: (!state.subtitles.is_empty()).then(|| state.subtitles.iter().any(|track| track.selected)),
        }
    }

    fn flags(published: &Published) -> u64 {
        let mut flags = FLAG_VOLUME;
        if published.playing {
            flags |= FLAG_PLAY | FLAG_PAUSE;
            if !published.live {
                flags |= FLAG_SKIP_FORWARD | FLAG_SKIP_BACKWARD | FLAG_FAST_FORWARD | FLAG_REWIND;
            }
        }
        flags
    }

    fn flags_value(&self) -> Value {
        Value::dict(vec![Value::kv("MediaControlFlags", Value::Int(Self::flags(&self.current())))])
    }

    /// Envoie l'état de lecture s'il a changé (ou toujours, si `force`).
    fn publish(&mut self, force: bool) -> Result<()> {
        let current = self.current();
        if !force && self.published.as_ref() == Some(&current) {
            return Ok(());
        }
        let flags = Self::flags(&current);
        self.event("MediaControlStatus", Value::dict(vec![Value::kv("MediaControlFlags", Value::Int(flags))]))?;
        self.event("_iMC", Value::dict(vec![Value::kv("_mcF", Value::Int(flags))]))?;
        self.event("NowPlayingInfo", now_playing(&current))?;
        self.published = Some(current);
        Ok(())
    }
}

/// Lecture en cours, au format attendu par TVRemoteCore (archive NSKeyedArchiver).
fn now_playing(state: &Published) -> Value {
    let mut objects = vec![Plist::String("$null".into()), Plist::Boolean(false)];
    let mut add = |value| {
        objects.push(value);
        uid((objects.len() - 1) as u64)
    };
    let info_class = add(class("TVRCNowPlayingInfo"));
    let fields = if state.playing {
        let id = add(Plist::String("jf".into()));
        let title = add(Plist::String(state.title.clone()));
        let meta_class = add(class("TVRCNowPlayingMetadata"));
        let duration = if state.live || state.duration <= 0 { uid(0) } else { add(Plist::Real(state.duration as f64)) };
        let metadata = add(dictionary([("title", title), ("duration", duration), ("$class", meta_class)]));
        let playback = add(Plist::Integer(if state.paused { 2 } else { 1 }.into()));
        let rate = add(Plist::Real(if state.paused { 0.0 } else { 1.0 }));
        let captions = state.captions.map_or_else(|| uid(0), |enabled| add(Plist::Boolean(enabled)));
        let has_captions = add(Plist::Boolean(state.captions.is_some()));
        [id, metadata, playback, rate, captions, has_captions]
    } else {
        [uid(0), uid(0), uid(0), uid(0), uid(0), uid(0)]
    };
    let [id, metadata, playback, rate, captions, has_captions] = fields;
    objects[1] = dictionary([
        ("$class", info_class),
        ("playerIdentifier", id.clone()),
        ("identifier", id),
        ("metadata", metadata),
        ("playbackState", playback),
        ("playbackRate", rate),
        ("captionsEnabled", captions),
        ("hasValidCaptionOptions", has_captions),
    ]);
    let archive = super::archive::encode("NSKeyedArchiver", dictionary([("root", uid(1))]), objects);
    Value::dict(vec![Value::kv("NowPlayingInfoKey", Value::Bytes(archive))])
}
