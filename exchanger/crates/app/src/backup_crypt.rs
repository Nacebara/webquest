//! Шифрование резервных копий базы (Б-19.1, решение владельца 20.11.2026: ключ — в `.env`).
//!
//! В копии — ссылки на чеки, которые клиенты ещё не забрали (деньги на предъявителя), и данные
//! клиентов, а папку копий советуют держать в облаке. Поэтому в папку копий попадает только шифр:
//! XChaCha20-Poly1305 тем же 32-байтовым `SESSION_KEY`, что и сессии юзерботов, с отдельной меткой
//! назначения в AAD (сессию нельзя выдать за копию и наоборот). Копия без `.env` бесполезна.
//!
//! Формат файла `exch-…dump.enc` — потоковая схема STREAM: заголовок `EXB1` и 19 случайных байт
//! префикса nonce, затем куски `длина (u32 BE) | шифротекст с тегом`. Nonce куска — префикс,
//! номер куска (u32 BE) и признак последнего куска: перестановку, подмену и обрезанный хвост
//! проверка тега ловит.

use std::io::{Read, Write};
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context as _, bail};
use chacha20poly1305::aead::{AeadInOut, Generate, KeyInit};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use secrecy::{ExposeSecret as _, SecretBox};
use zeroize::Zeroizing;

/// Подпись зашифрованной копии.
const MAGIC: &[u8; 4] = b"EXB1";
/// Метка назначения ключа в AAD.
const AAD: &[u8] = b"exch/backup/v1\0";
/// Длина префикса nonce: 24 байта XChaCha минус номер куска (4) и признак последнего (1).
const PREFIX_LEN: usize = 19;
/// Размер куска открытого текста.
const CHUNK: usize = 1 << 20;
const TAG_LEN: usize = 16;

/// Ключ шифрования копий — `SESSION_KEY` из `.env`.
#[derive(Clone)]
pub struct BackupKey(Arc<SecretBox<[u8; 32]>>);

impl BackupKey {
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(Arc::new(SecretBox::new(Box::new(bytes))))
    }

    /// Из `SESSION_KEY` (base64 32 байт), как его читает конфигурация.
    pub fn from_session_key(key: &crate::config::SessionKey) -> Self {
        Self::from_bytes(*key.secret().expose_secret())
    }

    fn cipher(&self) -> anyhow::Result<XChaCha20Poly1305> {
        XChaCha20Poly1305::new_from_slice(self.0.expose_secret())
            .map_err(|_| anyhow::anyhow!("ключ копий неверной длины"))
    }
}

impl std::fmt::Debug for BackupKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BackupKey([REDACTED])")
    }
}

impl PartialEq for BackupKey {
    fn eq(&self, other: &Self) -> bool {
        self.0.expose_secret() == other.0.expose_secret()
    }
}

impl Eq for BackupKey {}

fn nonce(prefix: &[u8; PREFIX_LEN], counter: u32, last: bool) -> XNonce {
    let mut n = [0u8; 24];
    n[..PREFIX_LEN].copy_from_slice(prefix);
    n[PREFIX_LEN..PREFIX_LEN + 4].copy_from_slice(&counter.to_be_bytes());
    n[23] = u8::from(last);
    XNonce::from(n)
}

/// Сколько байт прочитано — до `buf.len()`, меньше только в конце файла.
fn fill(r: &mut impl Read, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut got = 0;
    while got < buf.len() {
        match r.read(&mut buf[got..])? {
            0 => break,
            n => got += n,
        }
    }
    Ok(got)
}

/// Зашифровать `src` (открытый `pg_dump`) в `dst`.
pub fn encrypt_file(key: &BackupKey, src: &Path, dst: &Path) -> anyhow::Result<()> {
    let cipher = key.cipher()?;
    let random = XNonce::try_generate()
        .map_err(|_| anyhow::anyhow!("системный генератор случайных чисел недоступен"))?;
    let mut prefix = [0u8; PREFIX_LEN];
    prefix.copy_from_slice(&random[..PREFIX_LEN]);
    let mut input = std::io::BufReader::new(
        std::fs::File::open(src).with_context(|| format!("не открыть {}", src.display()))?,
    );
    let mut out = std::io::BufWriter::new(
        std::fs::File::create(dst).with_context(|| format!("не создать {}", dst.display()))?,
    );
    out.write_all(MAGIC)?;
    out.write_all(&prefix)?;
    let mut plain = Zeroizing::new(vec![0u8; CHUNK]);
    let mut next = Zeroizing::new(vec![0u8; CHUNK]);
    let mut len = fill(&mut input, &mut plain)?;
    let mut counter: u32 = 0;
    loop {
        // Последний ли кусок, видно, только прочитав следующий.
        let next_len = if len == CHUNK {
            fill(&mut input, &mut next)?
        } else {
            0
        };
        let last = next_len == 0;
        let mut buf = Zeroizing::new(Vec::with_capacity(len + TAG_LEN));
        buf.extend_from_slice(&plain[..len]);
        cipher
            .encrypt_in_place(&nonce(&prefix, counter, last), AAD, &mut *buf)
            .map_err(|_| anyhow::anyhow!("копия не зашифровалась"))?;
        let size = u32::try_from(buf.len()).context("кусок копии слишком велик")?;
        out.write_all(&size.to_be_bytes())?;
        out.write_all(&buf)?;
        if last {
            break;
        }
        std::mem::swap(&mut plain, &mut next);
        len = next_len;
        counter = counter
            .checked_add(1)
            .context("копия больше 4 ПБ — так не бывает")?;
    }
    out.flush()?;
    out.into_inner()
        .map_err(|e| e.into_error())?
        .sync_all()
        .with_context(|| format!("не записать {}", dst.display()))?;
    Ok(())
}

/// Зашифрованная ли это копия (по подписи в начале файла).
pub fn is_encrypted(path: &Path) -> anyhow::Result<bool> {
    let mut head = [0u8; 4];
    let mut f =
        std::fs::File::open(path).with_context(|| format!("не открыть {}", path.display()))?;
    Ok(fill(&mut f, &mut head)? == 4 && &head == MAGIC)
}

/// Расшифровать `src` в `dst` (открытый `pg_dump`). Чужой ключ, подмена или обрезанный файл —
/// ошибка, `dst` удаляется.
pub fn decrypt_file(key: &BackupKey, src: &Path, dst: &Path) -> anyhow::Result<()> {
    let r = decrypt_inner(key, src, dst);
    if r.is_err() {
        let _ = std::fs::remove_file(dst);
    }
    r
}

fn decrypt_inner(key: &BackupKey, src: &Path, dst: &Path) -> anyhow::Result<()> {
    const WRONG: &str = "копию не расшифровать: другой SESSION_KEY (нужен тот, что был в .env, когда \
                         снималась копия) или файл повреждён";
    let cipher = key.cipher()?;
    let mut input = std::io::BufReader::new(
        std::fs::File::open(src).with_context(|| format!("не открыть {}", src.display()))?,
    );
    let mut head = [0u8; 4 + PREFIX_LEN];
    if fill(&mut input, &mut head)? != head.len() || &head[..4] != MAGIC {
        bail!("{} — не зашифрованная копия exch", src.display());
    }
    let mut prefix = [0u8; PREFIX_LEN];
    prefix.copy_from_slice(&head[4..]);
    let mut out = std::io::BufWriter::new(
        std::fs::File::create(dst).with_context(|| format!("не создать {}", dst.display()))?,
    );
    // Длина следующего куска читается заранее: нет её — текущий кусок последний.
    let mut size_buf = [0u8; 4];
    if fill(&mut input, &mut size_buf)? != 4 {
        bail!(WRONG);
    }
    let mut counter: u32 = 0;
    loop {
        let size = usize::try_from(u32::from_be_bytes(size_buf)).unwrap_or(usize::MAX);
        if !(TAG_LEN..=CHUNK + TAG_LEN).contains(&size) {
            bail!(WRONG);
        }
        let mut buf = Zeroizing::new(vec![0u8; size]);
        if fill(&mut input, &mut buf)? != size {
            bail!(WRONG);
        }
        let mut next = [0u8; 4];
        let last = match fill(&mut input, &mut next)? {
            0 => true,
            4 => false,
            _ => bail!(WRONG),
        };
        cipher
            .decrypt_in_place(&nonce(&prefix, counter, last), AAD, &mut *buf)
            .map_err(|_| anyhow::anyhow!(WRONG))?;
        out.write_all(&buf)?;
        if last {
            break;
        }
        size_buf = next;
        counter = counter.checked_add(1).context("слишком много кусков")?;
    }
    out.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn tmp(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("exch-crypt-test-{}-{name}", std::process::id()))
    }

    fn roundtrip(len: usize) {
        let key = BackupKey::from_bytes([3; 32]);
        let (src, enc, out) = (
            tmp(&format!("{len}.src")),
            tmp(&format!("{len}.enc")),
            tmp(&format!("{len}.out")),
        );
        let data: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
        std::fs::write(&src, &data).unwrap();
        encrypt_file(&key, &src, &enc).unwrap();
        assert!(is_encrypted(&enc).unwrap());
        assert!(!is_encrypted(&src).unwrap() || len < 4);
        decrypt_file(&key, &enc, &out).unwrap();
        assert_eq!(std::fs::read(&out).unwrap(), data, "длина {len}");
        for p in [src, enc, out] {
            let _ = std::fs::remove_file(p);
        }
    }

    #[test]
    fn any_length_comes_back_whole() {
        for len in [0, 1, 100, CHUNK - 1, CHUNK, CHUNK + 1, 2 * CHUNK + 5] {
            roundtrip(len);
        }
    }

    #[test]
    fn wrong_key_cut_tail_and_swapped_chunks_are_refused() {
        let key = BackupKey::from_bytes([5; 32]);
        let (src, enc, out) = (tmp("bad.src"), tmp("bad.enc"), tmp("bad.out"));
        let data: Vec<u8> = (0..(2 * CHUNK + 10)).map(|i| (i % 13) as u8).collect();
        std::fs::write(&src, &data).unwrap();
        encrypt_file(&key, &src, &enc).unwrap();
        let sealed = std::fs::read(&enc).unwrap();

        let other = BackupKey::from_bytes([6; 32]);
        let err = decrypt_file(&other, &enc, &out).unwrap_err();
        assert!(err.to_string().contains("SESSION_KEY"), "{err:#}");
        assert!(!out.exists(), "недорасшифрованный файл удалён");

        // Обрезан хвост ровно по границе куска: без последнего куска — отказ.
        let first_two = 4 + PREFIX_LEN + 2 * (4 + CHUNK + TAG_LEN);
        std::fs::write(&enc, &sealed[..first_two]).unwrap();
        assert!(decrypt_file(&key, &enc, &out).is_err(), "обрезанная копия");

        // Куски переставлены местами.
        let head = 4 + PREFIX_LEN;
        let piece = 4 + CHUNK + TAG_LEN;
        let mut swapped = sealed[..head].to_vec();
        swapped.extend_from_slice(&sealed[head + piece..head + 2 * piece]);
        swapped.extend_from_slice(&sealed[head..head + piece]);
        swapped.extend_from_slice(&sealed[head + 2 * piece..]);
        std::fs::write(&enc, &swapped).unwrap();
        assert!(
            decrypt_file(&key, &enc, &out).is_err(),
            "переставленные куски"
        );

        for p in [src, enc, out] {
            let _ = std::fs::remove_file(p);
        }
    }
}
