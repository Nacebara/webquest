//! Шифрование сессии MTProto: XChaCha20-Poly1305, ключ 32 байта из `SESSION_KEY`, случайный
//! nonce 24 байта на каждое сохранение (SPEC §10, CLAUDE.md п. 13).
//!
//! - Открытый текст сессии (auth key!) живёт только в памяти, в `Zeroizing`, и на диск не пишется.
//! - AAD связывает шифротекст с версией ключа и меткой аккаунта: сессию одного аккаунта нельзя
//!   подставить другому, а подмена `key_version` обнаруживается.
//! - Ключ хранится в `SecretBox` (стирается при drop); шифр собирается на время операции и тоже
//!   стирает ключ (feature `zeroize` у `chacha20poly1305`).

use chacha20poly1305::aead::{AeadInOut, Generate, KeyInit};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use secrecy::{ExposeSecret, SecretBox};
use zeroize::Zeroizing;

/// Длина nonce XChaCha20.
pub const NONCE_LEN: usize = 24;
/// Длина тега Poly1305.
pub const TAG_LEN: usize = 16;

const AAD_PREFIX: &[u8] = b"exch/mtproto/session/v1\0";
const ENVELOPE_MAGIC: &[u8; 4] = b"EXS1";

/// Ключ шифрования сессий с номером версии (`userbot_accounts.key_version`).
pub struct SessionKey {
    version: i16,
    key: SecretBox<[u8; 32]>,
}

impl SessionKey {
    /// Ключ из готового секрета (`app::config::SessionKey`).
    pub fn new(version: i16, key: SecretBox<[u8; 32]>) -> Self {
        Self { version, key }
    }

    /// Ключ из байтов; исходный буфер стирает вызывающий.
    pub fn from_bytes(version: i16, key: &[u8; 32]) -> Self {
        Self {
            version,
            key: SecretBox::new(Box::new(*key)),
        }
    }

    pub fn version(&self) -> i16 {
        self.version
    }

    fn cipher(&self) -> Result<XChaCha20Poly1305, CryptoError> {
        XChaCha20Poly1305::new_from_slice(self.key.expose_secret())
            .map_err(|_| CryptoError::Encrypt)
    }
}

impl std::fmt::Debug for SessionKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SessionKey(v{}, [REDACTED])", self.version)
    }
}

/// Зашифрованная сессия: ровно то, что лежит в `userbot_accounts`
/// (`session_ciphertext`, `session_nonce`, `key_version`).
#[derive(Clone, PartialEq, Eq)]
pub struct SealedSession {
    pub key_version: i16,
    pub nonce: [u8; NONCE_LEN],
    /// Шифротекст вместе с тегом Poly1305 (последние 16 байт).
    pub ciphertext: Vec<u8>,
}

impl std::fmt::Debug for SealedSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SealedSession")
            .field("key_version", &self.key_version)
            .field("ciphertext_len", &self.ciphertext.len())
            .finish_non_exhaustive()
    }
}

impl SealedSession {
    /// Однострочный конверт (`EXS1 | key_version BE | nonce | ciphertext`) — для файла или одной
    /// колонки. Внутри только шифротекст, поэтому писать его на диск можно.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(4 + 2 + NONCE_LEN + self.ciphertext.len());
        out.extend_from_slice(ENVELOPE_MAGIC);
        out.extend_from_slice(&self.key_version.to_be_bytes());
        out.extend_from_slice(&self.nonce);
        out.extend_from_slice(&self.ciphertext);
        out
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, CryptoError> {
        let rest = bytes
            .strip_prefix(ENVELOPE_MAGIC.as_slice())
            .ok_or(CryptoError::BadEnvelope)?;
        if rest.len() < 2 + NONCE_LEN + TAG_LEN {
            return Err(CryptoError::BadEnvelope);
        }
        let (version, rest) = rest.split_at(2);
        let (nonce, ciphertext) = rest.split_at(NONCE_LEN);
        Ok(Self {
            key_version: i16::from_be_bytes([version[0], version[1]]),
            nonce: nonce.try_into().map_err(|_| CryptoError::BadEnvelope)?,
            ciphertext: ciphertext.to_vec(),
        })
    }

    /// Разобрать колонки БД (`session_nonce` хранится отдельно).
    pub fn from_parts(
        key_version: i16,
        nonce: &[u8],
        ciphertext: Vec<u8>,
    ) -> Result<Self, CryptoError> {
        let nonce: [u8; NONCE_LEN] = nonce.try_into().map_err(|_| CryptoError::BadEnvelope)?;
        if ciphertext.len() < TAG_LEN {
            return Err(CryptoError::BadEnvelope);
        }
        Ok(Self {
            key_version,
            nonce,
            ciphertext,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CryptoError {
    /// Сессия зашифрована другой версией ключа: нужен старый ключ или повторный вход.
    #[error("session is sealed with key version {found}, current key version is {expected}")]
    KeyVersionMismatch { expected: i16, found: i16 },
    /// Неверный ключ, чужая метка аккаунта или изменённый шифротекст.
    #[error("session ciphertext failed authentication (wrong key, account or tampered data)")]
    Tampered,
    #[error("malformed sealed session")]
    BadEnvelope,
    #[error("session encryption failed")]
    Encrypt,
    #[error("system random generator failed")]
    Random,
}

fn aad(key_version: i16, account: &str) -> Vec<u8> {
    let mut aad = Vec::with_capacity(AAD_PREFIX.len() + 2 + account.len());
    aad.extend_from_slice(AAD_PREFIX);
    aad.extend_from_slice(&key_version.to_be_bytes());
    aad.extend_from_slice(account.as_bytes());
    aad
}

/// Зашифровать сессию аккаунта `account` текущим ключом со свежим случайным nonce.
pub fn seal(
    key: &SessionKey,
    account: &str,
    plaintext: &[u8],
) -> Result<SealedSession, CryptoError> {
    let nonce = XNonce::try_generate().map_err(|_| CryptoError::Random)?;
    seal_with_nonce(key, account, plaintext, nonce.into())
}

fn seal_with_nonce(
    key: &SessionKey,
    account: &str,
    plaintext: &[u8],
    nonce: [u8; NONCE_LEN],
) -> Result<SealedSession, CryptoError> {
    let cipher = key.cipher()?;
    // Буфер с запасом под тег: шифрование на месте без перевыделения, иначе в освобождённой
    // памяти осталась бы копия открытого текста.
    let mut buf = Zeroizing::new(Vec::with_capacity(plaintext.len() + TAG_LEN));
    buf.extend_from_slice(plaintext);
    cipher
        .encrypt_in_place(&XNonce::from(nonce), &aad(key.version, account), &mut *buf)
        .map_err(|_| CryptoError::Encrypt)?;
    Ok(SealedSession {
        key_version: key.version,
        nonce,
        ciphertext: std::mem::take(&mut *buf),
    })
}

/// Расшифровать сессию аккаунта `account`. Открытый текст — в `Zeroizing`.
pub fn open(
    key: &SessionKey,
    account: &str,
    sealed: &SealedSession,
) -> Result<Zeroizing<Vec<u8>>, CryptoError> {
    if sealed.key_version != key.version {
        return Err(CryptoError::KeyVersionMismatch {
            expected: key.version,
            found: sealed.key_version,
        });
    }
    let cipher = key.cipher()?;
    let mut buf = Zeroizing::new(sealed.ciphertext.clone());
    cipher
        .decrypt_in_place(
            &XNonce::from(sealed.nonce),
            &aad(sealed.key_version, account),
            &mut *buf,
        )
        .map_err(|_| CryptoError::Tampered)?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(v: i16, b: u8) -> SessionKey {
        SessionKey::from_bytes(v, &[b; 32])
    }

    #[test]
    fn seal_open_round_trip() {
        let k = key(1, 7);
        let secret = b"auth key and dc options".to_vec();
        let sealed = seal(&k, "ub-1", &secret).unwrap();
        assert_eq!(sealed.key_version, 1);
        assert_eq!(sealed.ciphertext.len(), secret.len() + TAG_LEN);
        assert_ne!(&sealed.ciphertext[..secret.len()], secret.as_slice());
        let opened = open(&k, "ub-1", &sealed).unwrap();
        assert_eq!(opened.as_slice(), secret.as_slice());
    }

    #[test]
    fn nonce_is_fresh_for_every_seal() {
        let k = key(1, 7);
        let a = seal(&k, "ub-1", b"same").unwrap();
        let b = seal(&k, "ub-1", b"same").unwrap();
        assert_ne!(a.nonce, b.nonce);
        assert_ne!(a.ciphertext, b.ciphertext);
    }

    #[test]
    fn tampering_is_detected() {
        let k = key(1, 7);
        let sealed = seal(&k, "ub-1", b"session bytes").unwrap();

        for i in 0..sealed.ciphertext.len() {
            let mut bad = sealed.clone();
            bad.ciphertext[i] ^= 0x01;
            assert_eq!(
                open(&k, "ub-1", &bad),
                Err(CryptoError::Tampered),
                "byte {i}"
            );
        }
        let mut bad = sealed.clone();
        bad.nonce[0] ^= 0x80;
        assert_eq!(open(&k, "ub-1", &bad), Err(CryptoError::Tampered));
        let mut bad = sealed.clone();
        bad.ciphertext.truncate(bad.ciphertext.len() - 1);
        assert_eq!(open(&k, "ub-1", &bad), Err(CryptoError::Tampered));
    }

    #[test]
    fn wrong_key_or_account_is_rejected() {
        let sealed = seal(&key(1, 7), "ub-1", b"session").unwrap();
        assert_eq!(
            open(&key(1, 8), "ub-1", &sealed),
            Err(CryptoError::Tampered)
        );
        assert_eq!(
            open(&key(1, 7), "ub-2", &sealed),
            Err(CryptoError::Tampered)
        );
        assert_eq!(
            open(&key(2, 7), "ub-1", &sealed),
            Err(CryptoError::KeyVersionMismatch {
                expected: 2,
                found: 1
            })
        );
        // Подмена номера версии в записи при том же ключе ломает AAD.
        let mut relabeled = sealed.clone();
        relabeled.key_version = 2;
        assert_eq!(
            open(&key(2, 7), "ub-1", &relabeled),
            Err(CryptoError::Tampered)
        );
    }

    #[test]
    fn known_answer_with_fixed_nonce() {
        // Детерминированный вектор: тот же ключ, nonce, AAD и текст дают тот же шифротекст.
        let k = key(3, 0x42);
        let a = seal_with_nonce(&k, "acc", b"hello", [9; NONCE_LEN]).unwrap();
        let b = seal_with_nonce(&k, "acc", b"hello", [9; NONCE_LEN]).unwrap();
        assert_eq!(a, b);
        assert_eq!(open(&k, "acc", &a).unwrap().as_slice(), b"hello");
    }

    #[test]
    fn envelope_round_trip_and_validation() {
        let k = key(5, 1);
        let sealed = seal(&k, "ub-1", b"payload").unwrap();
        let bytes = sealed.to_bytes();
        assert!(bytes.starts_with(b"EXS1"));
        let back = SealedSession::from_bytes(&bytes).unwrap();
        assert_eq!(back, sealed);
        assert_eq!(open(&k, "ub-1", &back).unwrap().as_slice(), b"payload");

        assert_eq!(
            SealedSession::from_bytes(b"EXS0whatever-whatever-whatever-whatever-whatever"),
            Err(CryptoError::BadEnvelope)
        );
        assert_eq!(
            SealedSession::from_bytes(&bytes[..4 + 2 + NONCE_LEN + TAG_LEN - 1]),
            Err(CryptoError::BadEnvelope)
        );
        let parts = SealedSession::from_parts(5, &sealed.nonce, sealed.ciphertext.clone()).unwrap();
        assert_eq!(parts, sealed);
        assert_eq!(
            SealedSession::from_parts(5, &sealed.nonce[..10], sealed.ciphertext.clone()),
            Err(CryptoError::BadEnvelope)
        );
    }

    #[test]
    fn debug_does_not_leak() {
        let k = key(1, 0xAB);
        assert_eq!(format!("{k:?}"), "SessionKey(v1, [REDACTED])");
        let sealed = seal(&k, "ub-1", b"x").unwrap();
        let dbg = format!("{sealed:?}");
        assert!(dbg.contains("ciphertext_len"));
        assert!(!dbg.contains("nonce"));
    }
}
