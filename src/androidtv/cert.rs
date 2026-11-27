//! Certificates for the Android TV remote. Pairing hashes the RSA public keys of both
//! certificates, so jf needs an RSA certificate of its own (self-signed, like a real TV),
//! and reads the RSA key out of the phone's certificate.

use rsa::pkcs8::EncodePrivateKey;
use rsa::signature::{SignatureEncoding, Signer};
use rsa::traits::PublicKeyParts;
use rsa::RsaPrivateKey;
use sha2::{Digest, Sha256};

/// RSA public key as the pairing hash takes it: big-endian bytes without leading zeros.
#[derive(Clone)]
pub struct PublicKey {
    pub modulus: Vec<u8>,
    pub exponent: Vec<u8>,
}

/// Key and certificate of jf's "TV", in DER (PKCS #8 key, X.509 certificate).
pub struct Identity {
    pub key: Vec<u8>,
    pub certificate: Vec<u8>,
}

// ---------------------------------------------------------------- DER writing

fn der(tag: u8, content: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    let length = content.len();
    if length < 0x80 {
        out.push(length as u8);
    } else {
        let bytes: Vec<u8> = length.to_be_bytes().into_iter().skip_while(|byte| *byte == 0).collect();
        out.push(0x80 | bytes.len() as u8);
        out.extend_from_slice(&bytes);
    }
    out.extend_from_slice(content);
    out
}

fn sequence(parts: &[&[u8]]) -> Vec<u8> {
    der(0x30, &parts.concat())
}

/// CN=`name`
fn name(name: &str) -> Vec<u8> {
    let common_name = sequence(&[&der(0x06, &[0x55, 0x04, 0x03]), &der(0x0c, name.as_bytes())]);
    sequence(&[&der(0x31, &common_name)])
}

/// Stable address derived from the public key, announced like a TV's Bluetooth address.
pub fn address(key: &PublicKey) -> String {
    let hash = Sha256::new().chain_update(b"jf android tv").chain_update(&key.modulus).finalize();
    let mut mac: [u8; 6] = hash[..6].try_into().expect("6 bytes");
    // Locally administered, not multicast.
    mac[0] = (mac[0] | 2) & !1;
    mac.map(|byte| format!("{byte:02X}")).join(":")
}

/// Generates a 2048-bit RSA key and a self-signed certificate valid until 9999, named
/// like a real device's (`atvremote/<device>/<product>/<model>/<address>`).
pub fn generate(model: &str) -> Result<Identity, String> {
    let mut rng = rand::rngs::OsRng;
    let key = RsaPrivateKey::new(&mut rng, 2048).map_err(|error| error.to_string())?;
    let public_key = PublicKey { modulus: key.n().to_bytes_be(), exponent: key.e().to_bytes_be() };
    let common_name = format!("atvremote/jf/jf/{model}/{}", address(&public_key));
    let public = rsa::pkcs8::EncodePublicKey::to_public_key_der(&key.to_public_key()).map_err(|e| e.to_string())?;
    // sha256WithRSAEncryption
    let algorithm = sequence(&[&der(0x06, &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x0b]), &der(0x05, &[])]);
    let mut serial: [u8; 16] = rand::random();
    serial[0] &= 0x7f;
    serial[0] |= 0x01;
    let validity = sequence(&[&der(0x17, b"250101000000Z"), &der(0x18, b"99991231235959Z")]);
    let subject = name(&common_name);
    let version = der(0xa0, &der(0x02, &[2]));
    let tbs = sequence(&[&version, &der(0x02, &serial), &algorithm, &subject, &validity, &subject, public.as_bytes()]);
    let signature = rsa::pkcs1v15::SigningKey::<Sha256>::new(key.clone()).sign(&tbs).to_vec();
    let mut bits = vec![0u8];
    bits.extend_from_slice(&signature);
    let certificate = sequence(&[&tbs, &algorithm, &der(0x03, &bits)]);
    let key = key.to_pkcs8_der().map_err(|error| error.to_string())?.as_bytes().to_vec();
    Ok(Identity { key, certificate })
}

/// Public key of a PKCS #8 RSA private key.
pub fn key_public(pkcs8: &[u8]) -> Option<PublicKey> {
    let key = <RsaPrivateKey as rsa::pkcs8::DecodePrivateKey>::from_pkcs8_der(pkcs8).ok()?;
    Some(PublicKey { modulus: key.n().to_bytes_be(), exponent: key.e().to_bytes_be() })
}

// ---------------------------------------------------------------- DER reading

/// Splits the first DER element: (tag, content, rest).
fn element(input: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let (&tag, rest) = input.split_first()?;
    let (&first, rest) = rest.split_first()?;
    let (length, rest) = if first < 0x80 {
        (usize::from(first), rest)
    } else {
        let count = usize::from(first & 0x7f);
        if count == 0 || count > 4 {
            return None;
        }
        let (bytes, rest) = rest.split_at_checked(count)?;
        (bytes.iter().fold(0usize, |length, byte| length << 8 | usize::from(*byte)), rest)
    };
    let (content, rest) = rest.split_at_checked(length)?;
    Some((tag, content, rest))
}

fn unsigned(integer: &[u8]) -> Vec<u8> {
    integer.iter().copied().skip_while(|byte| *byte == 0).collect()
}

/// RSA public key of an X.509 certificate (the phone's); `None` for another key type.
pub fn certificate_public(certificate: &[u8]) -> Option<PublicKey> {
    let (0x30, certificate, _) = element(certificate)? else { return None };
    let (0x30, mut tbs, _) = element(certificate)? else { return None };
    // version (optional), serial, signature, issuer, validity, subject, then the key.
    let mut index = 0;
    let key = loop {
        let (tag, content, rest) = element(tbs)?;
        tbs = rest;
        if tag == 0xa0 {
            continue;
        }
        if index == 5 {
            break content;
        }
        index += 1;
    };
    let (0x30, algorithm, rest) = element(key)? else { return None };
    let (0x06, oid, _) = element(algorithm)? else { return None };
    // rsaEncryption
    if oid != [0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x01] {
        return None;
    }
    let (0x03, bits, _) = element(rest)? else { return None };
    let (0x30, numbers, _) = element(bits.strip_prefix(&[0])?)? else { return None };
    let (0x02, modulus, rest) = element(numbers)? else { return None };
    let (0x02, exponent, _) = element(rest)? else { return None };
    Some(PublicKey { modulus: unsigned(modulus), exponent: unsigned(exponent) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_certificate_reads_back() {
        let identity = generate("jf").unwrap();
        let from_certificate = certificate_public(&identity.certificate).unwrap();
        let from_key = key_public(&identity.key).unwrap();
        assert_eq!(from_certificate.modulus, from_key.modulus);
        assert_eq!(from_certificate.exponent, [1, 0, 1]);
        assert_eq!(from_certificate.modulus.len(), 256);
    }
}
