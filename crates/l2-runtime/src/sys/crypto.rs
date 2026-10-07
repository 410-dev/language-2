//! Random generators, hashing, key derivation, authenticated encryption and RSA (wrapping the
//! RustCrypto crates).

use super::{closed, handle, str_array, tuple, Args, Resource, SysOp};
use crate::value::*;
use rand_chacha::ChaCha20Rng;
use rand_core::{CryptoRng, OsRng, RngCore, SeedableRng};
use std::rc::Rc;

pub struct Rng {
    pub r: ChaCha20Rng,
    pub seeded: bool,
}

fn crypto_err<H: Host>(h: &mut H, msg: impl Into<String>) -> H::Err {
    h.throw(ExcKind::Crypto, msg.into())
}

fn ill<H: Host>(h: &mut H, msg: impl Into<String>) -> H::Err {
    h.throw(ExcKind::IllegalArgument, msg.into())
}

/// Uniform integer in `[0, n)` without modulo bias.
fn below(r: &mut impl RngCore, n: u64) -> u64 {
    if n == 0 {
        return r.next_u64();
    }
    let zone = u64::MAX - (u64::MAX - n + 1) % n;
    loop {
        let v = r.next_u64();
        if v <= zone {
            return v % n;
        }
    }
}

fn unit_f64(r: &mut impl RngCore) -> f64 {
    (r.next_u64() >> 11) as f64 / (1u64 << 53) as f64
}

/// Runs `f` with the generator of handle argument `i`.
fn with_rng<H: Host, R>(a: &Args, i: usize, h: &mut H, f: impl FnOnce(&mut Rng, &mut H) -> Result<R, H::Err>) -> Result<R, H::Err> {
    let x = a.handle(i, h)?;
    let mut res = x.res.borrow_mut();
    match &mut *res {
        Resource::Rng(r) => f(r, h),
        _ => Err(closed("random generator", h)),
    }
}

// ---------------------------------------------------------------------------------------------
// hashing

fn norm(alg: &str) -> String {
    alg.to_ascii_uppercase().replace(['-', '_', ' '], "")
}

const HASHES: &[&str] = &["MD5", "SHA-1", "SHA-224", "SHA-256", "SHA-384", "SHA-512", "SHA3-224", "SHA3-256", "SHA3-384", "SHA3-512"];

fn hasher(alg: &str) -> Option<Box<dyn digest::DynDigest>> {
    Some(match norm(alg).as_str() {
        "MD5" => Box::new(md5::Md5::default()),
        "SHA1" => Box::new(sha1::Sha1::default()),
        "SHA224" => Box::new(sha2::Sha224::default()),
        "SHA256" => Box::new(sha2::Sha256::default()),
        "SHA384" => Box::new(sha2::Sha384::default()),
        "SHA512" => Box::new(sha2::Sha512::default()),
        "SHA3224" => Box::new(sha3::Sha3_224::default()),
        "SHA3256" => Box::new(sha3::Sha3_256::default()),
        "SHA3384" => Box::new(sha3::Sha3_384::default()),
        "SHA3512" => Box::new(sha3::Sha3_512::default()),
        _ => return None,
    })
}

fn unknown_hash<H: Host>(alg: &str, h: &mut H) -> H::Err {
    ill(h, format!("unknown hash algorithm \"{}\" (available: {})", alg, HASHES.join(", ")))
}

macro_rules! with_digest {
    ($alg:expr, $D:ident => $body:expr, $none:expr) => {
        match norm($alg).as_str() {
            "MD5" => { type $D = md5::Md5; $body }
            "SHA1" => { type $D = sha1::Sha1; $body }
            "SHA224" => { type $D = sha2::Sha224; $body }
            "SHA256" => { type $D = sha2::Sha256; $body }
            "SHA384" => { type $D = sha2::Sha384; $body }
            "SHA512" => { type $D = sha2::Sha512; $body }
            "SHA3224" => { type $D = sha3::Sha3_224; $body }
            "SHA3256" => { type $D = sha3::Sha3_256; $body }
            "SHA3384" => { type $D = sha3::Sha3_384; $body }
            "SHA3512" => { type $D = sha3::Sha3_512; $body }
            _ => $none,
        }
    };
}

fn hmac_bytes(alg: &str, key: &[u8], data: &[u8]) -> Option<Vec<u8>> {
    use hmac::Mac;
    with_digest!(alg, D => {
        let mut m = <hmac::Hmac<D> as Mac>::new_from_slice(key).ok()?;
        m.update(data);
        Some(m.finalize().into_bytes().to_vec())
    }, None)
}

fn pbkdf2_bytes(alg: &str, pw: &[u8], salt: &[u8], iters: u32, len: usize) -> Option<Vec<u8>> {
    let mut out = vec![0u8; len];
    match norm(alg).as_str() {
        "SHA1" => pbkdf2::pbkdf2_hmac::<sha1::Sha1>(pw, salt, iters, &mut out),
        "SHA256" => pbkdf2::pbkdf2_hmac::<sha2::Sha256>(pw, salt, iters, &mut out),
        "SHA384" => pbkdf2::pbkdf2_hmac::<sha2::Sha384>(pw, salt, iters, &mut out),
        "SHA512" => pbkdf2::pbkdf2_hmac::<sha2::Sha512>(pw, salt, iters, &mut out),
        _ => return None,
    }
    Some(out)
}

fn hkdf_bytes(alg: &str, ikm: &[u8], salt: &[u8], info: &[u8], len: usize) -> Option<Result<Vec<u8>, ()>> {
    let mut out = vec![0u8; len];
    let salt = (!salt.is_empty()).then_some(salt);
    let r = match norm(alg).as_str() {
        "SHA256" => hkdf::Hkdf::<sha2::Sha256>::new(salt, ikm).expand(info, &mut out).map_err(|_| ()),
        "SHA384" => hkdf::Hkdf::<sha2::Sha384>::new(salt, ikm).expand(info, &mut out).map_err(|_| ()),
        "SHA512" => hkdf::Hkdf::<sha2::Sha512>::new(salt, ikm).expand(info, &mut out).map_err(|_| ()),
        _ => return None,
    };
    Some(r.map(|_| out))
}

pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut d = 0u8;
    for (x, y) in a.iter().zip(b) {
        d |= x ^ y;
    }
    std::hint::black_box(d) == 0
}

// ---------------------------------------------------------------------------------------------
// symmetric encryption

const SYMS: &[&str] = &["AES-128", "AES-192", "AES-256", "ChaCha20-Poly1305"];

/// (id, key size) of a symmetric algorithm. All are AEAD modes: AES in GCM mode, ChaCha20 with
/// Poly1305.
fn sym(alg: &str) -> Option<(u8, usize)> {
    Some(match norm(alg).as_str() {
        "AES128" | "AES128GCM" => (1, 16),
        "AES192" | "AES192GCM" => (2, 24),
        "AES" | "AES256" | "AES256GCM" => (3, 32),
        "CHACHA20" | "CHACHA20POLY1305" => (4, 32),
        _ => return None,
    })
}

fn sym_name(id: u8) -> &'static str {
    match id {
        1 => "AES-128",
        2 => "AES-192",
        3 => "AES-256",
        _ => "ChaCha20-Poly1305",
    }
}

fn aead_seal(id: u8, key: &[u8], nonce: &[u8], plain: &[u8], aad: &[u8]) -> Result<Vec<u8>, ()> {
    use aes_gcm::aead::{Aead, KeyInit, Payload};
    let p = Payload { msg: plain, aad };
    let n = aes_gcm::Nonce::from_slice(nonce);
    match id {
        1 => aes_gcm::Aes128Gcm::new_from_slice(key).map_err(|_| ())?.encrypt(n, p).map_err(|_| ()),
        2 => aes_gcm::AesGcm::<aes_gcm::aes::Aes192, aes_gcm::aead::consts::U12>::new_from_slice(key).map_err(|_| ())?.encrypt(n, p).map_err(|_| ()),
        3 => aes_gcm::Aes256Gcm::new_from_slice(key).map_err(|_| ())?.encrypt(n, p).map_err(|_| ()),
        _ => chacha20poly1305::ChaCha20Poly1305::new_from_slice(key).map_err(|_| ())?.encrypt(chacha20poly1305::Nonce::from_slice(nonce), p).map_err(|_| ()),
    }
}

fn aead_open(id: u8, key: &[u8], nonce: &[u8], data: &[u8], aad: &[u8]) -> Result<Vec<u8>, ()> {
    use aes_gcm::aead::{Aead, KeyInit, Payload};
    let p = Payload { msg: data, aad };
    let n = aes_gcm::Nonce::from_slice(nonce);
    match id {
        1 => aes_gcm::Aes128Gcm::new_from_slice(key).map_err(|_| ())?.decrypt(n, p).map_err(|_| ()),
        2 => aes_gcm::AesGcm::<aes_gcm::aes::Aes192, aes_gcm::aead::consts::U12>::new_from_slice(key).map_err(|_| ())?.decrypt(n, p).map_err(|_| ()),
        3 => aes_gcm::Aes256Gcm::new_from_slice(key).map_err(|_| ())?.decrypt(n, p).map_err(|_| ()),
        _ => chacha20poly1305::ChaCha20Poly1305::new_from_slice(key).map_err(|_| ())?.decrypt(chacha20poly1305::Nonce::from_slice(nonce), p).map_err(|_| ()),
    }
}

const MAGIC: &[u8; 3] = b"L2C";

/// Encrypted container: `"L2C" | 1 | algorithm | kdf (0 raw key, 1 PBKDF2-HMAC-SHA256) |
/// iterations (u32 BE) | salt length | salt | 12-byte nonce | ciphertext and tag`; everything
/// before the nonce is authenticated as associated data.
#[allow(clippy::too_many_arguments)]
fn sym_encrypt<H: Host>(alg: &str, key: &[u8], salt: Option<&[u8]>, kdf: &str, iters: i64, plain: &[u8], h: &mut H) -> Result<Vec<u8>, H::Err> {
    let Some((id, ksize)) = sym(alg) else {
        return Err(ill(h, format!("unknown symmetric algorithm \"{}\" (available: {})", alg, SYMS.join(", "))));
    };
    let (kdf_id, real_key, salt) = match kdf.to_ascii_lowercase().as_str() {
        "none" | "raw" => {
            if key.len() != ksize {
                return Err(ill(h, format!("{} needs a {}-byte key, got {} byte(s); use keyDerivation(\"pbkdf2\") to derive keys from passwords", sym_name(id), ksize, key.len())));
            }
            (0u8, key.to_vec(), Vec::new())
        }
        "pbkdf2" | "" => {
            if !(1..=u32::MAX as i64).contains(&iters) {
                return Err(ill(h, format!("invalid iteration count {}", iters)));
            }
            let salt = match salt {
                Some(s) if !s.is_empty() => {
                    if s.len() > 255 {
                        return Err(ill(h, "salt longer than 255 bytes"));
                    }
                    s.to_vec()
                }
                _ => {
                    let mut s = vec![0u8; 16];
                    OsRng.fill_bytes(&mut s);
                    s
                }
            };
            let k = pbkdf2_bytes("SHA-256", key, &salt, iters as u32, ksize).unwrap_or_default();
            (1u8, k, salt)
        }
        other => return Err(ill(h, format!("unknown key derivation \"{}\" (expected \"pbkdf2\" or \"none\")", other))),
    };
    let mut out = Vec::with_capacity(plain.len() + 64);
    out.extend_from_slice(MAGIC);
    out.push(1);
    out.push(id);
    out.push(kdf_id);
    out.extend_from_slice(&(if kdf_id == 1 { iters as u32 } else { 0 }).to_be_bytes());
    out.push(salt.len() as u8);
    out.extend_from_slice(&salt);
    let header_len = out.len();
    let mut nonce = [0u8; 12];
    OsRng.fill_bytes(&mut nonce);
    let sealed = aead_seal(id, &real_key, &nonce, plain, &out[..header_len]).map_err(|_| crypto_err(h, "encryption failed"))?;
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&sealed);
    Ok(out)
}

fn sym_decrypt<H: Host>(key: &[u8], salt: Option<&[u8]>, data: &[u8], h: &mut H) -> Result<Vec<u8>, H::Err> {
    let damaged = |h: &mut H| crypto_err(h, "not data produced by SymmetricCryptography.encrypt (bad header)");
    if data.len() < 10 || &data[..3] != MAGIC || data[3] != 1 {
        return Err(damaged(h));
    }
    let (id, kdf_id) = (data[4], data[5]);
    let iters = u32::from_be_bytes([data[6], data[7], data[8], data[9]]);
    let slen = *data.get(10).ok_or_else(|| damaged(h))? as usize;
    let header_len = 11 + slen;
    if !(1..=4).contains(&id) || kdf_id > 1 || data.len() < header_len + 12 + 16 {
        return Err(damaged(h));
    }
    let stored_salt = &data[11..header_len];
    if let Some(s) = salt {
        if !s.is_empty() && kdf_id == 1 && !constant_time_eq(s, stored_salt) {
            return Err(crypto_err(h, "the salt does not match the one used for encryption"));
        }
    }
    let ksize = match id {
        1 => 16,
        2 => 24,
        _ => 32,
    };
    let real_key = if kdf_id == 1 {
        pbkdf2_bytes("SHA-256", key, stored_salt, iters, ksize).unwrap_or_default()
    } else {
        if key.len() != ksize {
            return Err(ill(h, format!("{} needs a {}-byte key, got {} byte(s)", sym_name(id), ksize, key.len())));
        }
        key.to_vec()
    };
    let nonce = &data[header_len..header_len + 12];
    aead_open(id, &real_key, nonce, &data[header_len + 12..], &data[..header_len]).map_err(|_| crypto_err(h, "decryption failed: wrong key or damaged data"))
}

// ---------------------------------------------------------------------------------------------
// RSA

/// Adapts a language-level generator (or the OS generator) for key generation and padding.
struct AnyRng<'a>(Option<&'a mut ChaCha20Rng>);

impl RngCore for AnyRng<'_> {
    fn next_u32(&mut self) -> u32 {
        match &mut self.0 {
            Some(r) => r.next_u32(),
            None => OsRng.next_u32(),
        }
    }
    fn next_u64(&mut self) -> u64 {
        match &mut self.0 {
            Some(r) => r.next_u64(),
            None => OsRng.next_u64(),
        }
    }
    fn fill_bytes(&mut self, d: &mut [u8]) {
        match &mut self.0 {
            Some(r) => r.fill_bytes(d),
            None => OsRng.fill_bytes(d),
        }
    }
    fn try_fill_bytes(&mut self, d: &mut [u8]) -> Result<(), rand_core::Error> {
        self.fill_bytes(d);
        Ok(())
    }
}

impl CryptoRng for AnyRng<'_> {}

fn rsa_key<H: Host>(a: &Args, i: usize, h: &mut H) -> Result<Rc<super::Handle>, H::Err> {
    let x = a.handle(i, h)?;
    if !matches!(&*x.res.borrow(), Resource::RsaPublic(_) | Resource::RsaPrivate(_)) {
        return Err(closed("key", h));
    }
    Ok(x)
}

fn public_of(r: &Resource) -> Option<rsa::RsaPublicKey> {
    match r {
        Resource::RsaPublic(k) => Some((**k).clone()),
        Resource::RsaPrivate(k) => Some(k.to_public_key()),
        _ => None,
    }
}

fn keypair(k: rsa::RsaPrivateKey) -> Value {
    let p = k.to_public_key();
    tuple(vec![handle("PublicKey", Resource::RsaPublic(Box::new(p))), handle("PrivateKey", Resource::RsaPrivate(Box::new(k)))])
}

fn check_bits<H: Host>(bits: i64, h: &mut H) -> Result<usize, H::Err> {
    if !(2048..=8192).contains(&bits) || bits % 8 != 0 {
        return Err(ill(h, format!("RSA key size {} is not supported (use 2048, 3072 or 4096)", bits)));
    }
    Ok(bits as usize)
}

/// Runs `f` with the generator handle at `i` (`None` = the operating system's).
fn rsa_with_rng<H: Host, R>(a: &Args, i: Option<usize>, h: &mut H, f: impl FnOnce(&mut AnyRng, &mut H) -> Result<R, H::Err>) -> Result<R, H::Err> {
    match i {
        Some(i) => with_rng(a, i, h, |r, h| f(&mut AnyRng(Some(&mut r.r)), h)),
        None => f(&mut AnyRng(None), h),
    }
}

pub(super) fn call<H: Host>(op: SysOp, a: &Args, h: &mut H) -> Result<Value, H::Err> {
    use SysOp::*;
    Ok(match op {
        rngNew => {
            let seeded = a.bool(1);
            let r = if seeded { ChaCha20Rng::seed_from_u64(a.int(0) as u64) } else { ChaCha20Rng::from_rng(OsRng).map_err(|e| crypto_err(h, e.to_string()))? };
            handle("Random", Resource::Rng(Box::new(Rng { r, seeded })))
        }
        rngInt => {
            let (lo, hi) = (a.int(1), a.int(2));
            if hi <= lo {
                return Err(ill(h, format!("bound must be greater than origin ({} <= {})", hi, lo)));
            }
            let span = (hi as i128 - lo as i128) as u128;
            with_rng(a, 0, h, |r, _| Ok(Value::i64((lo as i128 + below(&mut r.r, span as u64) as i128) as i64)))?
        }
        rngLong => with_rng(a, 0, h, |r, _| Ok(Value::i64(r.r.next_u64() as i64)))?,
        rngFloat => with_rng(a, 0, h, |r, _| Ok(Value::Float(FloatTy::F64, unit_f64(&mut r.r))))?,
        rngBytes => {
            let n = a.int(1).max(0) as usize;
            with_rng(a, 0, h, |r, _| {
                let mut b = vec![0u8; n];
                r.r.fill_bytes(&mut b);
                Ok(Value::bytes(b))
            })?
        }
        rngGaussian => with_rng(a, 0, h, |r, _| {
            // Box-Muller
            let u1 = 1.0 - unit_f64(&mut r.r);
            let u2 = unit_f64(&mut r.r);
            Ok(Value::Float(FloatTy::F64, (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()))
        })?,
        rngIsSeeded => with_rng(a, 0, h, |r, _| Ok(Value::Bool(r.seeded)))?,
        secureBytes => {
            let mut b = vec![0u8; a.int(0).max(0) as usize];
            OsRng.fill_bytes(&mut b);
            Value::bytes(b)
        }
        hashAlgorithms => str_array(HASHES.iter().map(|s| s.to_string()).collect()),
        hashDigest => {
            let alg = a.str(0);
            let Some(mut d) = hasher(&alg) else { return Err(unknown_hash(&alg, h)) };
            a.with_bytes(1, h, |b, _| {
                d.update(b);
                Ok(())
            })?;
            Value::bytes(d.finalize().to_vec())
        }
        hashNew => {
            let alg = a.str(0);
            let Some(d) = hasher(&alg) else { return Err(unknown_hash(&alg, h)) };
            handle("Hasher", Resource::Hasher(d))
        }
        hashUpdate => {
            let x = a.handle(0, h)?;
            let data = a.bytes(1, h)?;
            match &mut *x.res.borrow_mut() {
                Resource::Hasher(d) => d.update(&data),
                _ => return Err(closed("digest", h)),
            }
            Value::Void
        }
        hashFinish => {
            let x = a.handle(0, h)?;
            let mut r = x.res.borrow_mut();
            let out = match &mut *r {
                Resource::Hasher(d) => d.finalize_reset().to_vec(),
                _ => return Err(closed("digest", h)),
            };
            Value::bytes(out)
        }
        hmac => {
            let alg = a.str(0);
            let (k, d) = (a.bytes(1, h)?, a.bytes(2, h)?);
            match hmac_bytes(&alg, &k, &d) {
                Some(m) => Value::bytes(m),
                None => return Err(unknown_hash(&alg, h)),
            }
        }
        pbkdf2 => {
            let alg = a.str(0);
            let (pw, salt) = (a.bytes(1, h)?, a.bytes(2, h)?);
            let (iters, len) = (a.int(3), a.int(4));
            if !(1..=u32::MAX as i64).contains(&iters) || !(1..=1 << 20).contains(&len) {
                return Err(ill(h, "invalid PBKDF2 iteration count or length"));
            }
            match pbkdf2_bytes(&alg, &pw, &salt, iters as u32, len as usize) {
                Some(k) => Value::bytes(k),
                None => return Err(ill(h, format!("PBKDF2 supports SHA-1, SHA-256, SHA-384 and SHA-512, not \"{}\"", alg))),
            }
        }
        hkdf => {
            let alg = a.str(0);
            let (ikm, salt, info) = (a.bytes(1, h)?, a.bytes(2, h)?, a.bytes(3, h)?);
            match hkdf_bytes(&alg, &ikm, &salt, &info, a.int(4).max(0) as usize) {
                Some(Ok(k)) => Value::bytes(k),
                Some(Err(())) => return Err(ill(h, "HKDF output too long")),
                None => return Err(ill(h, format!("HKDF supports SHA-256, SHA-384 and SHA-512, not \"{}\"", alg))),
            }
        }
        constantTimeEquals => {
            let (x, y) = (a.bytes(0, h)?, a.bytes(1, h)?);
            Value::Bool(constant_time_eq(&x, &y))
        }
        symAlgorithms => str_array(SYMS.iter().map(|s| s.to_string()).collect()),
        symKeySize => match sym(&a.str(0)) {
            Some((_, k)) => Value::i64(k as i64),
            None => return Err(ill(h, format!("unknown symmetric algorithm \"{}\" (available: {})", a.str(0), SYMS.join(", ")))),
        },
        symEncrypt => {
            // alg, key, salt, hasSalt, kdf, iterations, plain
            let (key, salt) = (a.bytes(1, h)?, a.bytes(2, h)?);
            let plain = a.bytes(6, h)?;
            let salt = a.bool(3).then_some(&salt[..]);
            Value::bytes(sym_encrypt(&a.str(0), &key, salt, &a.str(4), a.int(5), &plain, h)?)
        }
        symDecrypt => {
            // alg, key, salt, hasSalt, data
            let (key, salt) = (a.bytes(1, h)?, a.bytes(2, h)?);
            let data = a.bytes(4, h)?;
            let salt = a.bool(3).then_some(&salt[..]);
            Value::bytes(sym_decrypt(&key, salt, &data, h)?)
        }
        rsaGenerate | rsaGenerateOs => {
            let bits = check_bits(a.int(0), h)?;
            let k = rsa_with_rng(a, (op == rsaGenerate).then_some(1), h, |r, h| rsa::RsaPrivateKey::new(r, bits).map_err(|e| crypto_err(h, e.to_string())))?;
            keypair(k)
        }
        rsaPublicOf => {
            let x = rsa_key(a, 0, h)?;
            let p = public_of(&x.res.borrow()).unwrap();
            handle("PublicKey", Resource::RsaPublic(Box::new(p)))
        }
        rsaToPem => {
            use rsa::pkcs8::{EncodePrivateKey, EncodePublicKey, LineEnding};
            let x = rsa_key(a, 0, h)?;
            let r = x.res.borrow();
            let pem = match &*r {
                Resource::RsaPrivate(k) => k.to_pkcs8_pem(LineEnding::LF).map(|s| s.to_string()).map_err(|e| e.to_string()),
                Resource::RsaPublic(k) => k.to_public_key_pem(LineEnding::LF).map_err(|e| e.to_string()),
                _ => unreachable!(),
            };
            Value::str(pem.map_err(|e| crypto_err(h, e))?)
        }
        rsaFromPem => {
            use rsa::pkcs1::{DecodeRsaPrivateKey, DecodeRsaPublicKey};
            use rsa::pkcs8::{DecodePrivateKey, DecodePublicKey};
            let s = a.str(0);
            if let Ok(k) = rsa::RsaPrivateKey::from_pkcs8_pem(&s).or_else(|_| rsa::RsaPrivateKey::from_pkcs1_pem(&s)) {
                handle("PrivateKey", Resource::RsaPrivate(Box::new(k)))
            } else if let Ok(k) = rsa::RsaPublicKey::from_public_key_pem(&s).or_else(|_| rsa::RsaPublicKey::from_pkcs1_pem(&s)) {
                handle("PublicKey", Resource::RsaPublic(Box::new(k)))
            } else {
                return Err(ill(h, "not a PEM-encoded RSA key (PKCS#8, SPKI or PKCS#1)"));
            }
        }
        rsaIsPrivate => Value::Bool(matches!(&*rsa_key(a, 0, h)?.res.borrow(), Resource::RsaPrivate(_))),
        rsaBits => {
            use rsa::traits::PublicKeyParts;
            let x = rsa_key(a, 0, h)?;
            let n = public_of(&x.res.borrow()).unwrap().size() * 8;
            Value::i64(n as i64)
        }
        rsaEncrypt | rsaEncryptOs => {
            let x = rsa_key(a, 0, h)?;
            let k = public_of(&x.res.borrow()).unwrap();
            let data = a.bytes(1, h)?;
            let c = rsa_with_rng(a, (op == rsaEncrypt).then_some(2), h, |r, h| k.encrypt(r, rsa::Oaep::new::<sha2::Sha256>(), &data).map_err(|e| crypto_err(h, format!("RSA encryption failed: {}", e))))?;
            Value::bytes(c)
        }
        rsaDecrypt => {
            let x = rsa_key(a, 0, h)?;
            let data = a.bytes(1, h)?;
            let r = x.res.borrow();
            let Resource::RsaPrivate(k) = &*r else { return Err(ill(h, "decryption needs a private key")) };
            Value::bytes(k.decrypt(rsa::Oaep::new::<sha2::Sha256>(), &data).map_err(|_| crypto_err(h, "decryption failed: wrong key or damaged data"))?)
        }
        rsaSign | rsaSignOs => {
            use rsa::signature::{RandomizedSigner, SignatureEncoding};
            let x = rsa_key(a, 0, h)?;
            let data = a.bytes(1, h)?;
            let key = match &*x.res.borrow() {
                Resource::RsaPrivate(k) => (**k).clone(),
                _ => return Err(ill(h, "signing needs a private key")),
            };
            let sk = rsa::pss::BlindedSigningKey::<sha2::Sha256>::new(key);
            let sig = rsa_with_rng(a, (op == rsaSign).then_some(2), h, |r, h| sk.try_sign_with_rng(r, &data).map_err(|e| crypto_err(h, e.to_string())))?;
            Value::bytes(sig.to_vec())
        }
        rsaVerify => {
            use rsa::signature::Verifier;
            let x = rsa_key(a, 0, h)?;
            let k = public_of(&x.res.borrow()).unwrap();
            let (msg, sig) = (a.bytes(1, h)?, a.bytes(2, h)?);
            let vk = rsa::pss::VerifyingKey::<sha2::Sha256>::new(k);
            Value::Bool(match rsa::pss::Signature::try_from(&sig[..]) {
                Ok(s) => vk.verify(&msg, &s).is_ok(),
                Err(_) => false,
            })
        }
        _ => unreachable!(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digests_and_kdf() {
        let mut d = hasher("SHA-256").unwrap();
        d.update(b"abc");
        assert_eq!(super::super::encoding::to_hex(&d.finalize(), false), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        let m = hmac_bytes("SHA-256", b"key", b"The quick brown fox jumps over the lazy dog").unwrap();
        assert_eq!(super::super::encoding::to_hex(&m, false), "f7bc83f430538424b13298e6aa6fb143ef4d59a14946175997479dbc2d1a3cd8");
        assert!(constant_time_eq(b"ab", b"ab") && !constant_time_eq(b"ab", b"ac"));
    }
}
