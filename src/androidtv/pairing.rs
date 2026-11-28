//! Pairing (port 6467): Google's "Polo" protocol. The phone asks to pair, both sides
//! agree on a 6-character hexadecimal code, jf shows it, and the phone proves it typed
//! it with a hash over both RSA keys and the code. Its certificate is then trusted on
//! the remote port.

use std::io;
use std::time::Duration;

use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use super::cert::{self, PublicKey};
use super::proto::{field, Inbox, Message, Value};
use super::{finish, handshake, receive, send, trace, Shared, Tls, PAIRING_WINDOW};

const PROTOCOL_VERSION: u64 = 2;
const STATUS_OK: u64 = 200;
const STATUS_ERROR: u64 = 400;
const STATUS_BAD_CONFIGURATION: u64 = 401;
const STATUS_BAD_SECRET: u64 = 402;
const ENCODING_HEXADECIMAL: u64 = 3;
const CODE_LENGTH: u64 = 6;
const ROLE_INPUT: u64 = 1;

// Fields of the outer message.
const PAIRING_REQUEST: u32 = 10;
const PAIRING_REQUEST_ACK: u32 = 11;
const OPTIONS: u32 = 20;
const CONFIGURATION: u32 = 30;
const CONFIGURATION_ACK: u32 = 31;
const SECRET: u32 = 40;
const SECRET_ACK: u32 = 41;

fn reply(status: u64) -> Message {
    Message::new().uint(1, PROTOCOL_VERSION).uint(2, status)
}

/// Hexadecimal code of 6 symbols, 6 bytes.
fn encoding() -> Message {
    Message::new().uint(1, ENCODING_HEXADECIMAL).uint(2, CODE_LENGTH)
}

/// SHA-256 of the phone's key, jf's key and the last two bytes of the code; the first
/// byte of the code is the first byte of this hash, which lets the phone check a typo.
pub(super) fn secret(phone: &PublicKey, jf: &PublicKey, nonce: &[u8; 2]) -> [u8; 32] {
    Sha256::new()
        .chain_update(&phone.modulus)
        .chain_update(&phone.exponent)
        .chain_update(&jf.modulus)
        .chain_update(&jf.exponent)
        .chain_update(nonce)
        .finalize()
        .into()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub(super) fn serve(mut tls: Tls, shared: &Shared, id: u64) -> io::Result<()> {
    tls.sock.set_read_timeout(Some(Duration::from_millis(500)))?;
    handshake(&mut tls, Duration::from_secs(10))?;
    let result = pair(&mut tls, shared, id);
    shared.close_window(id);
    finish(&mut tls);
    result
}

fn pair(tls: &mut Tls, shared: &Shared, id: u64) -> io::Result<()> {
    let certificate = tls.conn.peer_certificates().and_then(|chain| chain.first()).map(|c| c.to_vec());
    let Some(certificate) = certificate else {
        return Err(io::Error::other("the phone sent no certificate"));
    };
    let started = std::time::Instant::now();
    let mut inbox = Inbox::default();
    // Hash expected once the code is on screen.
    let mut expected: Option<[u8; 32]> = None;
    loop {
        if started.elapsed() > PAIRING_WINDOW + Duration::from_secs(10) {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "no code entered"));
        }
        let Some(message) = receive(tls, &mut inbox)? else { continue };
        let status = field(&message, 2).and_then(Value::int);
        if status != Some(STATUS_OK) {
            trace!("#{id}: phone gave up (status {status:?}, message {})", hex(&message));
            return Ok(());
        }
        if let Some(request) = field(&message, PAIRING_REQUEST).and_then(Value::bytes) {
            let service = field(request, 1).map(Value::text).unwrap_or_default();
            let client = field(request, 2).map(Value::text).unwrap_or_default();
            trace!("#{id}: pairing request from {client:?} ({service})");
            send(tls, &reply(STATUS_OK).message(PAIRING_REQUEST_ACK, Message::new().text(1, &shared.name)))?;
        } else if let Some(options) = field(&message, OPTIONS).and_then(Value::bytes) {
            trace!("#{id}: phone options {}", hex(options));
            // The phone types the code (input) that jf shows (output): both are offered, the
            // Google TV app gives up (status 401) without an output encoding.
            let options = Message::new().message(1, encoding()).message(2, encoding()).uint(3, ROLE_INPUT);
            send(tls, &reply(STATUS_OK).message(OPTIONS, options))?;
        } else if let Some(configuration) = field(&message, CONFIGURATION).and_then(Value::bytes) {
            let chosen = field(configuration, 1).and_then(Value::bytes).unwrap_or_default();
            let hexadecimal = field(chosen, 1).and_then(Value::int) == Some(ENCODING_HEXADECIMAL)
                && field(chosen, 2).and_then(Value::int) == Some(CODE_LENGTH);
            let Some(phone) = cert::certificate_public(&certificate).filter(|_| hexadecimal) else {
                trace!("#{id}: unsupported configuration or certificate");
                return send(tls, &reply(STATUS_BAD_CONFIGURATION));
            };
            let nonce: [u8; 2] = rand::random();
            let hash = secret(&phone, &shared.server_key, &nonce);
            let code = format!("{:02X}{:02X}{:02X}", hash[0], nonce[0], nonce[1]);
            if !shared.open_window(id, code) {
                trace!("#{id}: pairing paused after wrong codes");
                return send(tls, &reply(STATUS_ERROR));
            }
            trace!("#{id}: code shown on screen");
            expected = Some(hash);
            send(tls, &reply(STATUS_OK).message(CONFIGURATION_ACK, Message::new()))?;
        } else if let Some(attempt) = field(&message, SECRET).and_then(Value::bytes) {
            let given = field(attempt, 1).and_then(Value::bytes).unwrap_or_default();
            let Some(hash) = expected.take().filter(|_| shared.window_open(id)) else {
                trace!("#{id}: code entered too late");
                return send(tls, &reply(STATUS_ERROR));
            };
            if !bool::from(given.ct_eq(&hash)) {
                trace!("#{id}: wrong code");
                shared.reject_code(id);
                return send(tls, &reply(STATUS_BAD_SECRET));
            }
            shared.trust(id, &certificate)?;
            trace!("#{id}: phone paired");
            return send(tls, &reply(STATUS_OK).message(SECRET_ACK, Message::new().bytes(1, &hash)));
        } else {
            trace!("#{id}: unexpected pairing message ignored");
        }
    }
}
