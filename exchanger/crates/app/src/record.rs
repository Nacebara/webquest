//! `exch record --label <метка> --out <файл.jsonl>` (HANDOVER §5, M2.3): подключиться и
//! ничего не отправлять; каждое сообщение из чатов с @send, @xrocket и «Избранного» — строкой
//! JSON (`userbot::RawMessage`) в файл и в `wallet_messages`. Нужно владельцу, чтобы записать
//! настоящие ответы ботов для фикстур парсеров (HANDOVER §7, п. 6).
//!
//! В файле — живые ссылки на чеки и счета: хранить вне репозитория, на unix файл создаётся
//! с правами `0600`.

use std::path::Path;

use tokio::io::{AsyncWrite, AsyncWriteExt};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use userbot::{Chat, MessageLog, RawMessage, Transport};

/// Итог записи.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RecordStats {
    /// Строк записано в файл.
    pub written: u64,
    /// Сколько сообщений пропущено из-за отставания от потока (`broadcast::Lagged`).
    pub lagged: u64,
}

/// Одна строка файла: `RawMessage` в JSON и перевод строки.
pub fn json_line(msg: &RawMessage) -> Result<Vec<u8>, serde_json::Error> {
    let mut line = serde_json::to_vec(msg)?;
    line.push(b'\n');
    Ok(line)
}

async fn write_one<W: AsyncWrite + Unpin>(
    out: &mut W,
    log: &dyn MessageLog,
    account: &str,
    msg: &RawMessage,
) -> std::io::Result<()> {
    // Сначала журнал (CLAUDE.md п. 8: до любого разбора), потом файл.
    log.record(account, msg, None).await;
    let line = json_line(msg).map_err(std::io::Error::other)?;
    out.write_all(&line).await?;
    // Каждая строка — сразу на диск: при аварии файл обрывается на целой строке.
    out.flush().await
}

/// Писать сообщения из потока, пока не отменят или поток не закроется.
pub async fn record_stream<W: AsyncWrite + Unpin>(
    mut rx: broadcast::Receiver<RawMessage>,
    out: &mut W,
    log: &dyn MessageLog,
    account: &str,
    cancel: &CancellationToken,
) -> std::io::Result<RecordStats> {
    let mut stats = RecordStats::default();
    loop {
        let next = tokio::select! {
            biased;
            () = cancel.cancelled() => break,
            next = rx.recv() => next,
        };
        match next {
            Ok(msg) => {
                write_one(out, log, account, &msg).await?;
                stats.written += 1;
            }
            Err(broadcast::error::RecvError::Lagged(skipped)) => {
                tracing::warn!(account, skipped, "recorder fell behind the update stream; rerun with --history to fill the gap");
                stats.lagged += skipped;
            }
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
    out.flush().await?;
    Ok(stats)
}

/// Чаты, которые пишет рекордер.
pub const CHATS: [Chat; 3] = [
    Chat::WalletBot(domain::Platform::CryptoBot),
    Chat::WalletBot(domain::Platform::XRocket),
    Chat::SavedMessages,
];

/// Последние `limit` сообщений каждого чата (только чтение истории). Ошибка чтения одного
/// чата — предупреждение, остальные чаты пишутся.
pub async fn backfill<T: Transport + ?Sized, W: AsyncWrite + Unpin>(
    transport: &T,
    limit: u32,
    out: &mut W,
    log: &dyn MessageLog,
    account: &str,
) -> std::io::Result<u64> {
    let mut written = 0;
    if limit == 0 {
        return Ok(0);
    }
    for chat in CHATS {
        match transport.recent(chat, limit).await {
            Ok(messages) => {
                for msg in &messages {
                    write_one(out, log, account, msg).await?;
                    written += 1;
                }
            }
            Err(e) => {
                tracing::warn!(account, ?chat, error = %e, "cannot read chat history");
            }
        }
    }
    Ok(written)
}

/// Открыть файл для дописывания (`0600` на unix).
pub async fn open_output(path: &Path) -> std::io::Result<tokio::fs::File> {
    let mut options = tokio::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    options.mode(0o600);
    options.open(path).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::Platform;
    use userbot::flows::MemoryLog;

    fn msg(id: i32) -> RawMessage {
        RawMessage {
            chat: Chat::WalletBot(Platform::XRocket),
            id,
            out: false,
            date: 1_760_000_000 + i64::from(id),
            edit_date: None,
            text: format!("ответ {id}"),
            entity_urls: vec![],
            buttons: vec![],
            via_bot: None,
        }
    }

    #[tokio::test]
    async fn stream_is_written_as_json_lines_and_journaled() {
        let (tx, rx) = broadcast::channel(16);
        let log = MemoryLog::default();
        let cancel = CancellationToken::new();
        let mut out = Vec::new();
        for id in 1..=3 {
            tx.send(msg(id)).unwrap();
        }
        drop(tx);
        let stats = record_stream(rx, &mut out, &log, "ub-1", &cancel)
            .await
            .unwrap();
        assert_eq!(stats, RecordStats { written: 3, lagged: 0 });

        let lines: Vec<RawMessage> = String::from_utf8(out)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines, vec![msg(1), msg(2), msg(3)]);
        let logged = log.entries();
        assert_eq!(logged.len(), 3);
        assert!(logged.iter().all(|e| e.account == "ub-1" && e.op.is_none()));
    }

    #[tokio::test]
    async fn lag_is_counted_and_recording_continues() {
        let (tx, rx) = broadcast::channel(2);
        let log = MemoryLog::default();
        let cancel = CancellationToken::new();
        for id in 1..=5 {
            tx.send(msg(id)).unwrap();
        }
        drop(tx);
        let mut out = Vec::new();
        let stats = record_stream(rx, &mut out, &log, "ub-1", &cancel)
            .await
            .unwrap();
        assert_eq!(stats, RecordStats { written: 2, lagged: 3 });
        assert_eq!(String::from_utf8(out).unwrap().lines().count(), 2);
    }

    #[tokio::test]
    async fn cancellation_stops_recording() {
        let (tx, rx) = broadcast::channel(16);
        let log = MemoryLog::default();
        let cancel = CancellationToken::new();
        let task = {
            let cancel = cancel.clone();
            tokio::spawn(async move {
                let mut out = Vec::new();
                let stats = record_stream(rx, &mut out, &log, "ub-1", &cancel).await;
                (stats.unwrap(), out)
            })
        };
        tx.send(msg(1)).unwrap();
        tokio::task::yield_now().await;
        cancel.cancel();
        let (stats, out) = task.await.unwrap();
        assert!(stats.written <= 1);
        assert_eq!(
            String::from_utf8(out).unwrap().lines().count() as u64,
            stats.written
        );
        drop(tx);
    }
}
