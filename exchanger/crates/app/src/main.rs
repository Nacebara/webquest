//! `exch` — процесс бота-обменника (SPEC §10.1, §12).

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::Context as _;
use clap::{Parser, Subcommand};
use secrecy::ExposeSecret;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;

use std::sync::Arc;

use app::config::Config;
use app::login::MtprotoAuthenticator;
use app::supervisor::{self, RestartPolicy, supervise};
use app::telegram::TelegramSettings;

/// Сколько ждать задачи после SIGTERM (Docker даёт 90 с, SPEC §12, шаг 6).
const SHUTDOWN_GRACE: Duration = Duration::from_secs(60);

#[derive(Parser)]
#[command(name = "exch", version, about = "Бот-обменник xRocket ⇄ CryptoBot")]
struct Cli {
    /// Файл с переменными окружения для локальной разработки (по умолчанию .env, если есть).
    #[arg(long, global = true)]
    env_file: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Применить миграции и работать до SIGTERM / Ctrl-C.
    Run,
    /// Применить миграции и выйти.
    Migrate,
    /// Проверить переменные окружения; значения секретов не печатаются.
    CheckConfig,
    /// Войти аккаунтом юзербота и привязать его к кошельку платформы. Телефон, код и пароль
    /// 2FA спрашиваются в терминале и никуда не сохраняются, сессия — только зашифрованной.
    Login {
        /// Метка аккаунта: a-z, 0-9, '-', '_' (например, ub-1).
        #[arg(long)]
        label: String,
        /// Кошелёк какой платформы — баланс этого аккаунта: cryptobot или xrocket.
        #[arg(long, value_parser = parse_platform)]
        platform: domain::Platform,
    },
    /// Записывать ответы ботов кошельков в файл JSON lines (для фикстур парсеров), ничего
    /// не отправляя. В файле — живые ссылки на чеки: хранить вне репозитория.
    Record {
        #[arg(long)]
        label: String,
        #[arg(long)]
        out: PathBuf,
        /// Сначала записать столько последних сообщений каждого чата.
        #[arg(long, default_value_t = 0)]
        history: u32,
    },
}

fn parse_platform(s: &str) -> Result<domain::Platform, String> {
    domain::Platform::from_db_name(s)
        .ok_or_else(|| format!("unknown platform {s}: use cryptobot or xrocket"))
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match &cli.env_file {
        Some(path) => {
            if let Err(e) = dotenvy::from_path(path) {
                eprintln!("cannot read {}: {e}", path.display());
                return ExitCode::FAILURE;
            }
        }
        None => {
            // .env необязателен: на сервере переменные приходят из docker compose.
            let _ = dotenvy::dotenv();
        }
    }
    init_tracing();

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("cannot start tokio runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    let result = runtime.block_on(async {
        match cli.command {
            Command::Run => run().await,
            Command::Migrate => migrate().await,
            Command::CheckConfig => check_config().await,
            Command::Login { label, platform } => login(label, platform).await,
            Command::Record {
                label,
                out,
                history,
            } => record(label, out, history).await,
        }
    });
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!(error = format!("{e:#}"), "exch stopped with an error");
            ExitCode::FAILURE
        }
    }
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(filter)
        .with_current_span(true)
        .init();
}

fn load_config() -> anyhow::Result<Config> {
    Config::from_env().context("invalid configuration (see .env.example and SPEC §12, step 8)")
}

async fn connect_and_migrate(cfg: &Config) -> anyhow::Result<storage::PgPool> {
    let pool = storage::connect(cfg.database_url.expose_secret())
        .await
        .context("cannot connect to PostgreSQL")?;
    storage::migrate(&pool).await.context("migrations failed")?;
    tracing::info!("migrations applied");
    Ok(pool)
}

async fn migrate() -> anyhow::Result<()> {
    let cfg = load_config()?;
    connect_and_migrate(&cfg).await.map(|_| ())
}

async fn check_config() -> anyhow::Result<()> {
    let cfg = load_config()?;
    tracing::info!(owner_tg_id = cfg.owner_tg_id, tz_owner = %cfg.tz_owner, "configuration is valid");
    for p in cfg.pending() {
        tracing::warn!(
            var = p.var,
            milestone = p.milestone,
            "not set yet, required from this milestone"
        );
    }
    if let Err(e) = TelegramSettings::from_config(&cfg) {
        tracing::warn!(error = %e, "userbots cannot connect yet");
    }
    // Аккаунты и кошельки — только метки, маски телефонов и статусы, без секретов.
    let pool = storage::connect(cfg.database_url.expose_secret())
        .await
        .context("cannot connect to PostgreSQL")?;
    let mut conn = pool.acquire().await?;
    for acc in storage::sessions::list(&mut conn).await? {
        tracing::info!(
            account = %acc.label,
            phone = %acc.phone_masked,
            status = acc.status.db_name(),
            has_session = acc.has_session,
            tg_user_id = acc.tg_user_id,
            "userbot account"
        );
    }
    for w in storage::wallets::list(&mut conn).await? {
        tracing::info!(
            wallet = %w.label,
            platform = w.platform.db_name(),
            active = w.is_active,
            account = %w.userbot_label,
            "wallet"
        );
    }
    Ok(())
}

async fn login(label: String, platform: domain::Platform) -> anyhow::Result<()> {
    let cfg = load_config()?;
    let tg = TelegramSettings::from_config(&cfg)?;
    let pool = connect_and_migrate(&cfg).await?;
    let mut auth = MtprotoAuthenticator {
        cfg: tg.mtproto(&label),
        key: tg.session_key(),
        prompt: mtproto::TerminalPrompt,
    };
    let target = app::login::LoginTarget { label, platform };
    app::login::run_login(&pool, &target, &mut auth).await?;
    Ok(())
}

async fn record(label: String, out: PathBuf, history: u32) -> anyhow::Result<()> {
    let cfg = load_config()?;
    let tg = TelegramSettings::from_config(&cfg)?;
    let pool = connect_and_migrate(&cfg).await?;
    let transport = app::runtime::connect_account(&pool, &tg, &label).await?;
    let log = app::wallet_log::WalletLog::new(pool.clone(), tg.bots().clone());
    let mut file = app::record::open_output(&out)
        .await
        .with_context(|| format!("cannot open {}", out.display()))?;
    let rx = userbot::Transport::subscribe(transport.as_ref());
    let backfilled =
        app::record::backfill(transport.as_ref(), history, &mut file, &log, &label).await?;
    tracing::info!(account = %label, backfilled, "recording: nothing is sent, Ctrl-C to stop");
    let cancel = CancellationToken::new();
    let stop = cancel.clone();
    tokio::spawn(async move {
        shutdown_signal().await;
        stop.cancel();
    });
    let stats = app::record::record_stream(rx, &mut file, &log, &label, &cancel).await?;
    tracing::info!(account = %label, written = stats.written, lagged = stats.lagged, "recording stopped");
    Ok(())
}

async fn run() -> anyhow::Result<()> {
    let cfg = load_config()?;
    let pool = connect_and_migrate(&cfg).await?;

    let cancel = CancellationToken::new();
    let mut tasks: JoinSet<Result<(), supervisor::GaveUp>> = JoinSet::new();
    {
        let pool = pool.clone();
        tasks.spawn(supervise(
            "heartbeat",
            cancel.clone(),
            RestartPolicy::default(),
            move |token| heartbeat(pool.clone(), token),
        ));
    }

    // Юзерботы и движок: без них процесс только держит пульс и миграции.
    match TelegramSettings::from_config(&cfg) {
        Err(e) => {
            tracing::warn!(error = %e, "userbots are not configured: the engine is not started")
        }
        Ok(tg) => {
            let log = Arc::new(app::wallet_log::WalletLog::new(
                pool.clone(),
                tg.bots().clone(),
            ));
            let userbots = app::runtime::connect_userbots(&pool, &tg, &log).await?;
            if userbots.is_empty() {
                tracing::warn!(
                    "no active wallet accounts: run `exch login` first; the engine is not started"
                );
            } else {
                let engine = Arc::new(app::runtime::engine(&pool, &userbots));
                engine.ensure_ledger_accounts().await?;
                {
                    let engine = engine.clone();
                    tasks.spawn(supervise(
                        "engine",
                        cancel.clone(),
                        RestartPolicy::default(),
                        move |token| {
                            let engine = engine.clone();
                            async move { engine.run(token).await.map_err(anyhow::Error::from) }
                        },
                    ));
                }
                // Клиентский бот — рядом с движком: подаёт ссылки в `submit_check` и правит
                // сообщения заявок по `engine.subscribe()`.
                match cfg.client_bot_token.clone() {
                    None => {
                        tracing::warn!("CLIENT_BOT_TOKEN is not set: the client bot is not started")
                    }
                    Some(token) => {
                        let bot = app::runtime::client_bot(
                            &pool,
                            token,
                            engine,
                            cfg.support_contact.clone(),
                        )?;
                        tasks.spawn(supervise(
                            "client-bot",
                            cancel.clone(),
                            RestartPolicy::default(),
                            move |token| {
                                let bot = bot.clone();
                                async move { bot.run(token).await.map_err(anyhow::Error::from) }
                            },
                        ));
                    }
                }
            }
        }
    }

    let outcome = tokio::select! {
        () = shutdown_signal() => {
            tracing::info!("shutdown signal received");
            Ok(())
        }
        Some(joined) = tasks.join_next() => match joined {
            Ok(Ok(())) => Ok(()),
            Ok(Err(gave_up)) => Err(anyhow::Error::new(gave_up)),
            Err(join) => Err(anyhow::Error::new(join).context("supervisor panicked")),
        },
    };

    cancel.cancel();
    if tokio::time::timeout(SHUTDOWN_GRACE, async {
        while tasks.join_next().await.is_some() {}
    })
    .await
    .is_err()
    {
        tracing::warn!(
            "tasks did not stop within the grace period; in-flight work resumes on restart"
        );
    }
    tracing::info!("stopped");
    outcome
}

/// Пульс процесса: раз в минуту проверяет БД. Пинг healthchecks.io добавится вместе с
/// HTTP-клиентом в M2 (SPEC §10.1, задача `heartbeat`).
async fn heartbeat(pool: storage::PgPool, cancel: CancellationToken) -> anyhow::Result<()> {
    let mut tick = tokio::time::interval(Duration::from_secs(60));
    loop {
        tokio::select! {
            () = cancel.cancelled() => return Ok(()),
            _ = tick.tick() => {
                let mut conn = pool.acquire().await.context("heartbeat: no database connection")?;
                let balances = storage::ledger::wallet_balances(&mut conn).await?;
                tracing::info!(wallets = balances.len(), "alive");
            }
        }
    }
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut term) => {
                tokio::select! {
                    _ = term.recv() => {}
                    _ = tokio::signal::ctrl_c() => {}
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "cannot listen for SIGTERM, falling back to Ctrl-C");
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
