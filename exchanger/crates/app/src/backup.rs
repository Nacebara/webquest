//! Резервные копии базы (веха 10): `pg_dump` в папку из `config.toml`, каждая копия сразу
//! проверяется восстановлением во временную базу, расписание — внутри `exch run`, без
//! планировщика Windows. Сбой копии — тревога в пульт; удача закрывает её.
//!
//! Копия «проверена», только если она восстановилась целиком: те же миграции, леджер сходится
//! по каждому активу, а записей в таблицах, которые только дописываются (заявки, проводки,
//! журнал), не меньше, чем было перед снятием копии. Временная база — всегда одна и та же,
//! `exch_restore_check`; другие базы этот модуль не создаёт и не удаляет.
//!
//! Пароль базы не попадает в командную строку `pg_dump` (её видят другие процессы): только в
//! переменную окружения `PGPASSWORD` дочернего процесса.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context as _, bail};
use chrono::{DateTime, Utc};
use secrecy::{ExposeSecret as _, SecretString};
use serde_json::json;
use sqlx::{Connection as _, PgConnection};
use storage::PgPool;
use storage::alerts::{Level, NewAlert, Topic};
use tokio_util::sync::CancellationToken;

/// Временная база проверки восстановления (в запросах — тем же словом, литералом).
pub const CHECK_DB: &str = "exch_restore_check";
/// Ключ `settings` с итогом последней копии.
pub const LAST_KEY: &str = "backup_last";
/// Ключ тревоги о сбое копии.
const ALERT_KEY: &str = "backup_failed";
/// Таблицы, которые только дописываются: в восстановленной копии записей не меньше, чем было
/// перед её снятием.
const APPEND_ONLY: [&str; 4] = [
    "orders",
    "ledger_transactions",
    "ledger_entries",
    "admin_audit",
];

/// Настройки копий из `config.toml` (`[backup]`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupSettings {
    /// Папка копий; `None` — копии выключены (`dir = "off"`).
    pub dir: Option<PathBuf>,
    /// Время ежедневной копии в поясе владельца, `ЧЧ:ММ`.
    pub at: String,
    /// Сколько последних копий хранить.
    pub keep: usize,
    /// Папка `pg_dump` / `pg_restore`; `None` — искать самому.
    pub pg_bin: Option<PathBuf>,
}

impl BackupSettings {
    /// Из переменных `BACKUP_DIR`, `BACKUP_AT`, `BACKUP_KEEP`, `PG_BIN`; относительная папка —
    /// от папки с `config.toml`.
    pub fn from_lookup(base: &Path, get: impl Fn(&str) -> Option<String>) -> anyhow::Result<Self> {
        let dir = match get("BACKUP_DIR").as_deref().map(str::trim) {
            Some("off") => None,
            Some(d) if !d.is_empty() => Some(base.join(d)),
            _ => Some(base.join("backups")),
        };
        let at = get("BACKUP_AT").unwrap_or_else(|| "04:30".to_owned());
        if parse_hhmm(&at).is_none() {
            bail!("BACKUP_AT: нужно время ЧЧ:ММ, например 04:30, а не {at:?}");
        }
        let keep = match get("BACKUP_KEEP") {
            Some(v) => v
                .trim()
                .parse::<usize>()
                .ok()
                .filter(|n| *n >= 1)
                .with_context(|| format!("BACKUP_KEEP: нужно число от 1, а не {v:?}"))?,
            None => 14,
        };
        let pg_bin = get("PG_BIN")
            .map(|p| PathBuf::from(p.trim()))
            .filter(|p| !p.as_os_str().is_empty());
        Ok(Self {
            dir,
            at,
            keep,
            pg_bin,
        })
    }
}

fn parse_hhmm(s: &str) -> Option<(u32, u32)> {
    let (h, m) = s.trim().split_once(':')?;
    let (h, m) = (h.parse::<u32>().ok()?, m.parse::<u32>().ok()?);
    (h < 24 && m < 60).then_some((h, m))
}

/// Адрес базы без пароля и сам пароль (из `postgres://user:pass@host:port/db?…`).
#[derive(Debug)]
pub struct DbTarget {
    /// Адрес для `--dbname` без пароля.
    pub uri: String,
    pub password: Option<SecretString>,
    /// Имя базы.
    pub database: String,
}

impl DbTarget {
    pub fn parse(url: &str) -> anyhow::Result<Self> {
        let (scheme, rest) = url
            .split_once("://")
            .context("DATABASE_URL: нет схемы postgres://")?;
        let path_at = rest.find('/').unwrap_or(rest.len());
        let (authority, tail) = rest.split_at(path_at);
        let (userinfo, host) = match authority.rfind('@') {
            Some(i) => (Some(&authority[..i]), &authority[i + 1..]),
            None => (None, authority),
        };
        let (user, password) = match userinfo {
            Some(u) => match u.split_once(':') {
                Some((name, pass)) => (Some(name), Some(percent_decode(pass)?)),
                None => (Some(u), None),
            },
            None => (None, None),
        };
        let database = tail
            .trim_start_matches('/')
            .split('?')
            .next()
            .unwrap_or_default()
            .to_owned();
        if database.is_empty() {
            bail!("DATABASE_URL: нет имени базы");
        }
        let uri = match user {
            Some(u) => format!("{scheme}://{u}@{host}{tail}"),
            None => format!("{scheme}://{host}{tail}"),
        };
        Ok(Self {
            uri,
            password: password.map(SecretString::from),
            database,
        })
    }

    /// Тот же сервер, другая база.
    pub fn with_database(&self, name: &str) -> Self {
        let (head, query) = match self.uri.split_once('?') {
            Some((h, q)) => (h, Some(q)),
            None => (self.uri.as_str(), None),
        };
        let base = head.rsplit_once('/').map_or(head, |(b, _)| b);
        let uri = match query {
            Some(q) => format!("{base}/{name}?{q}"),
            None => format!("{base}/{name}"),
        };
        Self {
            uri,
            password: self.password.clone(),
            database: name.to_owned(),
        }
    }

    /// Полный адрес для подключения sqlx (с паролем) — только в памяти процесса.
    fn connect_url(&self) -> String {
        match &self.password {
            Some(p) => {
                let (scheme, rest) = self.uri.split_once("://").unwrap_or(("postgres", ""));
                let (user, host) = rest.split_once('@').unwrap_or(("", rest));
                format!(
                    "{scheme}://{user}:{}@{host}",
                    percent_encode(p.expose_secret())
                )
            }
            None => self.uri.clone(),
        }
    }
}

fn percent_decode(s: &str) -> anyhow::Result<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = s.get(i + 1..i + 3).context("DATABASE_URL: обрывок %XX")?;
            out.push(u8::from_str_radix(hex, 16).context("DATABASE_URL: неверный %XX")?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).context("DATABASE_URL: пароль не UTF-8")
}

fn percent_encode(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

/// Найти `pg_dump` / `pg_restore`: папка из настроек, затем PATH, затем на Windows —
/// `C:\Program Files\PostgreSQL\<версия>\bin` (самая новая версия).
pub fn find_tool(name: &str, pg_bin: Option<&Path>) -> anyhow::Result<PathBuf> {
    let exe = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_owned()
    };
    if let Some(dir) = pg_bin {
        let p = dir.join(&exe);
        if p.is_file() {
            return Ok(p);
        }
        bail!("{} не найден в PG_BIN ({})", exe, dir.display());
    }
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            let p = dir.join(&exe);
            if p.is_file() {
                return Ok(p);
            }
        }
    }
    if cfg!(windows) {
        let root = Path::new(r"C:\Program Files\PostgreSQL");
        let mut versions: Vec<(u32, PathBuf)> = std::fs::read_dir(root)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let v = e.file_name().to_string_lossy().parse::<u32>().ok()?;
                let p = e.path().join("bin").join(&exe);
                p.is_file().then_some((v, p))
            })
            .collect();
        versions.sort();
        if let Some((_, p)) = versions.pop() {
            return Ok(p);
        }
    }
    bail!(
        "не нашёл {exe}: укажите папку PostgreSQL bin в config.toml ([backup] pg_bin), например C:\\Program Files\\PostgreSQL\\16\\bin"
    )
}

async fn run_tool(tool: &Path, args: &[String], target: &DbTarget) -> anyhow::Result<()> {
    let mut cmd = tokio::process::Command::new(tool);
    cmd.args(args).kill_on_drop(true);
    if let Some(p) = &target.password {
        cmd.env("PGPASSWORD", p.expose_secret());
    }
    let out = tokio::time::timeout(Duration::from_secs(30 * 60), cmd.output())
        .await
        .with_context(|| format!("{} не закончил за 30 минут", tool.display()))?
        .with_context(|| format!("не запустился {}", tool.display()))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        bail!(
            "{} завершился с ошибкой: {}",
            tool.file_name().unwrap_or_default().to_string_lossy(),
            err.trim()
        );
    }
    Ok(())
}

/// Итог одной копии.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub file: PathBuf,
    pub bytes: u64,
    pub at: DateTime<Utc>,
    /// Что проверено при восстановлении.
    pub checked: Checked,
}

/// Что показало восстановление во временную базу.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checked {
    pub migration: i64,
    /// Записей по таблицам, которые только дописываются.
    pub rows: Vec<(String, i64)>,
}

async fn counts(conn: &mut PgConnection) -> anyhow::Result<Vec<(String, i64)>> {
    let (a, b, c, d): (i64, i64, i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM orders), (SELECT count(*) FROM ledger_transactions), \
                (SELECT count(*) FROM ledger_entries), (SELECT count(*) FROM admin_audit)",
    )
    .fetch_one(&mut *conn)
    .await?;
    Ok(APPEND_ONLY
        .iter()
        .zip([a, b, c, d])
        .map(|(t, n)| ((*t).to_owned(), n))
        .collect())
}

async fn last_migration(conn: &mut PgConnection) -> anyhow::Result<i64> {
    Ok(
        sqlx::query_scalar("SELECT COALESCE(max(version), 0) FROM _sqlx_migrations WHERE success")
            .fetch_one(conn)
            .await?,
    )
}

/// Снять копию в папку настроек и сразу проверить её восстановлением. Файл появляется под
/// своим именем, только когда `pg_dump` закончил: оборванная копия не выглядит целой.
pub async fn make(
    pool: &PgPool,
    database_url: &SecretString,
    s: &BackupSettings,
) -> anyhow::Result<Outcome> {
    let dir = s
        .dir
        .as_ref()
        .context("копии выключены: [backup] dir = \"off\"")?;
    std::fs::create_dir_all(dir)
        .with_context(|| format!("не удалось создать папку копий {}", dir.display()))?;
    let target = DbTarget::parse(database_url.expose_secret())?;
    let dump = find_tool("pg_dump", s.pg_bin.as_deref())?;
    let (before, migration) = {
        let mut conn = pool.acquire().await?;
        (counts(&mut conn).await?, last_migration(&mut conn).await?)
    };
    let at = Utc::now();
    let name = format!("exch-{}.dump", at.format("%Y-%m-%d-%H%M%S"));
    let part = dir.join(format!("{name}.part"));
    let file = dir.join(&name);
    let args = vec![
        "--format=custom".to_owned(),
        "--no-owner".to_owned(),
        "--no-privileges".to_owned(),
        format!("--file={}", part.display()),
        format!("--dbname={}", target.uri),
    ];
    if let Err(e) = run_tool(&dump, &args, &target).await {
        let _ = std::fs::remove_file(&part);
        return Err(e);
    }
    std::fs::rename(&part, &file)
        .with_context(|| format!("не удалось назвать {}", file.display()))?;
    let bytes = std::fs::metadata(&file)?.len();
    let checked = verify(&file, database_url, s, Some((&before, migration))).await?;
    prune(dir, s.keep)?;
    Ok(Outcome {
        file,
        bytes,
        at,
        checked,
    })
}

/// Восстановить копию во временную базу [`CHECK_DB`] и проверить её. `expect` — записи и
/// миграция перед снятием копии (при проверке старой копии — `None`).
pub async fn verify(
    file: &Path,
    database_url: &SecretString,
    s: &BackupSettings,
    expect: Option<(&[(String, i64)], i64)>,
) -> anyhow::Result<Checked> {
    let target = DbTarget::parse(database_url.expose_secret())?;
    if target.database == CHECK_DB {
        bail!("рабочая база называется {CHECK_DB} — проверка восстановления её бы стёрла");
    }
    let restore = find_tool("pg_restore", s.pg_bin.as_deref())?;
    let check = target.with_database(CHECK_DB);
    // Создать и удалить временную базу — из служебной базы `postgres`: рабочей может ещё не быть
    // (восстановление на новом компьютере).
    {
        let mut admin =
            PgConnection::connect(&target.with_database("postgres").connect_url()).await?;
        // Только своя временная база: прошлая проверка могла оборваться.
        sqlx::query("DROP DATABASE IF EXISTS exch_restore_check")
            .execute(&mut admin)
            .await?;
        sqlx::query("CREATE DATABASE exch_restore_check")
            .execute(&mut admin)
            .await?;
    }
    let args = vec![
        "--no-owner".to_owned(),
        "--no-privileges".to_owned(),
        "--exit-on-error".to_owned(),
        format!("--dbname={}", check.uri),
        file.display().to_string(),
    ];
    let result = async {
        run_tool(&restore, &args, &check).await?;
        let mut conn = PgConnection::connect(&check.connect_url()).await?;
        let migration = last_migration(&mut conn).await?;
        let unbalanced: Vec<(String, domain::Decimal)> = sqlx::query_as(
            "SELECT a.asset, sum(e.amount) FROM ledger_entries e \
               JOIN ledger_accounts a ON a.id = e.account_id \
              GROUP BY a.asset HAVING sum(e.amount) <> 0",
        )
        .fetch_all(&mut conn)
        .await?;
        if !unbalanced.is_empty() {
            bail!("в копии леджер не сходится: {unbalanced:?}");
        }
        let rows = counts(&mut conn).await?;
        conn.close().await.ok();
        if let Some((before, expected_migration)) = expect {
            if migration != expected_migration {
                bail!("в копии миграция {migration}, а в базе {expected_migration}");
            }
            for ((table, was), (_, now)) in before.iter().zip(&rows) {
                if now < was {
                    bail!("в копии {table}: {now} записей, а перед копией было {was}");
                }
            }
        }
        Ok(Checked { migration, rows })
    }
    .await;
    let mut admin = PgConnection::connect(&target.with_database("postgres").connect_url()).await?;
    sqlx::query("DROP DATABASE IF EXISTS exch_restore_check")
        .execute(&mut admin)
        .await?;
    result
}

/// Почему копию нельзя восстановить в эту базу: в ней уже есть деньги (заявки или проводки).
/// `None` — база пустая или её нет: восстанавливать можно.
pub async fn restore_blocker(database_url: &SecretString) -> anyhow::Result<Option<String>> {
    use sqlx::migrate::MigrateDatabase as _;
    let url = database_url.expose_secret();
    if !sqlx::Postgres::database_exists(url).await? {
        return Ok(None);
    }
    let mut conn = PgConnection::connect(url).await?;
    let has_schema: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM information_schema.tables WHERE table_name = 'orders')",
    )
    .fetch_one(&mut conn)
    .await?;
    if !has_schema {
        return Ok(None);
    }
    let (orders, txs): (i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM orders), (SELECT count(*) FROM ledger_transactions)",
    )
    .fetch_one(&mut conn)
    .await?;
    Ok((orders > 0 || txs > 0).then(|| {
        format!("в базе уже {orders} заявок и {txs} проводок — восстановление стёрло бы их")
    }))
}

/// Восстановить копию в рабочую базу (компьютер сломался, база потеряна). Только в пустую базу
/// или в новую: база с деньгами не трогается. Сначала копия проверяется во временной базе.
pub async fn restore(
    file: &Path,
    database_url: &SecretString,
    s: &BackupSettings,
) -> anyhow::Result<Checked> {
    use sqlx::migrate::MigrateDatabase as _;
    if let Some(why) = restore_blocker(database_url).await? {
        bail!("{why}. Укажите в DATABASE_URL новую базу (например, exch_restored) и повторите");
    }
    let checked = verify(file, database_url, s, None).await?;
    let url = database_url.expose_secret();
    if !sqlx::Postgres::database_exists(url).await? {
        sqlx::Postgres::create_database(url).await?;
    }
    let target = DbTarget::parse(url)?;
    let restore = find_tool("pg_restore", s.pg_bin.as_deref())?;
    // В пустой базе могут быть таблицы после `exch setup` — их заменяет копия.
    let args = vec![
        "--clean".to_owned(),
        "--if-exists".to_owned(),
        "--no-owner".to_owned(),
        "--no-privileges".to_owned(),
        "--exit-on-error".to_owned(),
        format!("--dbname={}", target.uri),
        file.display().to_string(),
    ];
    run_tool(&restore, &args, &target).await?;
    Ok(checked)
}

/// Оставить `keep` последних копий (по имени: в нём дата и время).
pub fn prune(dir: &Path, keep: usize) -> anyhow::Result<Vec<PathBuf>> {
    let mut files = list(dir)?;
    let mut removed = Vec::new();
    while files.len() > keep {
        let old = files.remove(0);
        std::fs::remove_file(&old)?;
        removed.push(old);
    }
    Ok(removed)
}

/// Копии в папке, старые первыми.
pub fn list(dir: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut files: Vec<PathBuf> = match std::fs::read_dir(dir) {
        Ok(rd) => rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("exch-") && n.ends_with(".dump"))
            })
            .collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e.into()),
    };
    files.sort();
    Ok(files)
}

/// Когда делать следующую копию: ближайшее `at` в поясе владельца (считает PostgreSQL — он
/// знает пояса). Последняя копия старше суток — через пять минут после запуска.
pub async fn next_run(
    pool: &PgPool,
    s: &BackupSettings,
    tz: &str,
) -> anyhow::Result<DateTime<Utc>> {
    let mut conn = pool.acquire().await?;
    let planned: DateTime<Utc> = sqlx::query_scalar(
        "SELECT CASE WHEN t > now() THEN t ELSE t + interval '1 day' END \
           FROM (SELECT (date_trunc('day', now() AT TIME ZONE $1) + $2::time) AT TIME ZONE $1 AS t) x",
    )
    .bind(tz)
    .bind(&s.at)
    .fetch_one(&mut *conn)
    .await?;
    let last: Option<DateTime<Utc>> = storage::settings::get(&mut conn, LAST_KEY)
        .await?
        .and_then(|v| v.get("at").and_then(|a| a.as_str()).map(str::to_owned))
        .and_then(|a| DateTime::parse_from_rfc3339(&a).ok())
        .map(|d| d.with_timezone(&Utc));
    let soon = Utc::now() + chrono::Duration::minutes(5);
    Ok(match last {
        Some(l) if Utc::now() - l < chrono::Duration::hours(26) => planned,
        _ => planned.min(soon),
    })
}

/// Записать итог и закрыть или поднять тревогу.
pub async fn record(pool: &PgPool, r: &anyhow::Result<Outcome>) -> anyhow::Result<()> {
    let mut conn = pool.acquire().await?;
    match r {
        Ok(o) => {
            storage::settings::set(
                &mut conn,
                LAST_KEY,
                &json!({
                    "at": o.at.to_rfc3339(),
                    "file": o.file.file_name().map(|n| n.to_string_lossy().into_owned()),
                    "bytes": o.bytes,
                    "migration": o.checked.migration,
                    "rows": o.checked.rows.iter().map(|(t, n)| json!([t, n])).collect::<Vec<_>>(),
                    "checked": true,
                }),
                None,
            )
            .await?;
            storage::alerts::resolve(&mut conn, ALERT_KEY).await?;
        }
        Err(e) => {
            storage::alerts::raise_condition(
                &mut conn,
                &NewAlert {
                    level: Level::Warn,
                    topic: Topic::Admin,
                    dedup_key: ALERT_KEY,
                    title: ALERT_KEY,
                    payload: json!({ "error": format!("{e:#}") }),
                },
            )
            .await?;
        }
    }
    Ok(())
}

/// Расписание копий внутри `exch run`.
pub async fn run(
    pool: PgPool,
    database_url: SecretString,
    s: BackupSettings,
    tz: String,
    token: CancellationToken,
) -> anyhow::Result<()> {
    if s.dir.is_none() {
        tracing::warn!("backups are off ([backup] dir = \"off\")");
        token.cancelled().await;
        return Ok(());
    }
    loop {
        let when = next_run(&pool, &s, &tz).await?;
        let wait = (when - Utc::now()).to_std().unwrap_or_default();
        tracing::info!(at = %when, "next database backup");
        tokio::select! {
            () = token.cancelled() => return Ok(()),
            () = tokio::time::sleep(wait) => {}
        }
        let r = make(&pool, &database_url, &s).await;
        match &r {
            Ok(o) => {
                tracing::info!(file = %o.file.display(), bytes = o.bytes, "database backup made and restore-checked")
            }
            Err(e) => tracing::error!(error = %format!("{e:#}"), "database backup failed"),
        }
        record(&pool, &r).await?;
        // Не чаще раза в минуту, даже если часы прыгнули.
        tokio::select! {
            () = token.cancelled() => return Ok(()),
            () = tokio::time::sleep(Duration::from_secs(60)) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn database_url_is_split_into_uri_and_password() {
        let t = DbTarget::parse("postgres://postgres:p%40ss:w@localhost:5432/exch?sslmode=disable")
            .unwrap();
        assert_eq!(
            t.uri,
            "postgres://postgres@localhost:5432/exch?sslmode=disable"
        );
        assert_eq!(t.password.as_ref().unwrap().expose_secret(), "p@ss:w");
        assert_eq!(t.database, "exch");
        let c = t.with_database(CHECK_DB);
        assert_eq!(
            c.uri,
            "postgres://postgres@localhost:5432/exch_restore_check?sslmode=disable"
        );
        assert_eq!(
            c.connect_url(),
            "postgres://postgres:p%40ss%3Aw@localhost:5432/exch_restore_check?sslmode=disable"
        );
        let bare = DbTarget::parse("postgres://localhost/exch").unwrap();
        assert!(bare.password.is_none());
        assert_eq!(bare.uri, "postgres://localhost/exch");
        assert!(DbTarget::parse("postgres://localhost").is_err());
    }

    #[test]
    fn settings_default_and_validate() {
        let base = Path::new("/srv/exch");
        let s = BackupSettings::from_lookup(base, |_| None).unwrap();
        assert_eq!(s.dir, Some(base.join("backups")));
        assert_eq!((s.at.as_str(), s.keep), ("04:30", 14));
        let off = BackupSettings::from_lookup(base, |k| (k == "BACKUP_DIR").then(|| "off".into()))
            .unwrap();
        assert_eq!(off.dir, None);
        assert!(
            BackupSettings::from_lookup(base, |k| (k == "BACKUP_AT").then(|| "25:00".into()))
                .is_err()
        );
        assert!(
            BackupSettings::from_lookup(base, |k| (k == "BACKUP_KEEP").then(|| "0".into()))
                .is_err()
        );
    }

    #[test]
    fn only_the_newest_copies_are_kept() {
        let dir = std::env::temp_dir().join(format!("exch-prune-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for d in ["2026-11-01", "2026-11-03", "2026-11-02"] {
            std::fs::write(dir.join(format!("exch-{d}-043000.dump")), b"x").unwrap();
        }
        std::fs::write(dir.join("exch-2026-11-04-043000.dump.part"), b"x").unwrap();
        std::fs::write(dir.join("notes.txt"), b"x").unwrap();
        let removed = prune(&dir, 2).unwrap();
        assert_eq!(removed.len(), 1);
        assert!(removed[0].ends_with("exch-2026-11-01-043000.dump"));
        let left: Vec<String> = list(&dir)
            .unwrap()
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            left,
            ["exch-2026-11-02-043000.dump", "exch-2026-11-03-043000.dump"]
        );
        assert!(
            dir.join("exch-2026-11-04-043000.dump.part").exists(),
            "незаконченная копия не трогается"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
