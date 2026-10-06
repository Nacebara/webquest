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

use app::config::Config;
use app::supervisor::{self, RestartPolicy, supervise};

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
            Command::CheckConfig => check_config(),
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

fn check_config() -> anyhow::Result<()> {
    let cfg = load_config()?;
    tracing::info!(owner_tg_id = cfg.owner_tg_id, tz_owner = %cfg.tz_owner, "configuration is valid for M1");
    for p in cfg.pending() {
        tracing::warn!(
            var = p.var,
            milestone = p.milestone,
            "not set yet, required from this milestone"
        );
    }
    Ok(())
}

async fn run() -> anyhow::Result<()> {
    let cfg = load_config()?;
    let pool = connect_and_migrate(&cfg).await?;
    tracing::info!(
        "warming up: client intake stays closed until wallets and bots are connected (M2-M5)"
    );

    let cancel = CancellationToken::new();
    let mut tasks: JoinSet<Result<(), supervisor::GaveUp>> = JoinSet::new();
    tasks.spawn(supervise(
        "heartbeat",
        cancel.clone(),
        RestartPolicy::default(),
        move |token| heartbeat(pool.clone(), token),
    ));

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
