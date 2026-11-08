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
#[command(
    name = "exch",
    version,
    about = "Бот-обменник xRocket ⇄ CryptoBot",
    long_about = "Бот-обменник xRocket ⇄ CryptoBot.\n\nБез команды — запустить бота (первый раз — мастер настройки). Секреты — в .env, остальные настройки — в config.toml в папке проекта."
)]
struct Cli {
    /// Файл секретов (по умолчанию .env в папке проекта: рядом, в target/release или выше).
    #[arg(long, global = true)]
    env_file: Option<PathBuf>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Применить миграции и работать до Ctrl-C (то же, что запуск без команды).
    Run,
    /// Мастер настройки: база, бот, владелец, ключи, вход юзерботом. Можно запускать повторно.
    Setup,
    /// Проверить всё по-русски: ✅ готово, ⚠️ стоит поправить, ❌ не заработает.
    #[command(alias = "check-config")]
    Check,
    /// Демо: настоящий бот и пульт на симуляторе кошельков — без денег и юзербота, на
    /// отдельной базе `<база>_demo` (пересоздаётся при запуске). Пробы — команда /demo в боте.
    Demo {
        /// Сразу прислать владельцу «Предпросмотр экранов» (все экраны клиента и пульта).
        #[arg(long)]
        preview: bool,
        /// Сразу прислать владельцу только экраны пульта (в боте — команда /preview_panel).
        #[arg(long)]
        preview_panel: bool,
        /// Сразу прислать владельцу экраны клиента, в названии которых есть это слово
        /// (в боте — «/preview счёт»).
        #[arg(long, value_name = "СЛОВО")]
        preview_only: Vec<String>,
    },
    /// Применить миграции и выйти.
    Migrate,
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
    /// Напечатать новый случайный ключ для `SESSION_KEY` (вставить в .env один раз; потеряете —
    /// юзерботам придётся войти заново).
    GenSessionKey,
    /// Самопроверка на живых кошельках: на каждом — чек через меню на `amount` USDT и его же
    /// активация; деньги возвращаются, леджер не меняется. Запускать при остановленном
    /// `exch run` (одна сессия Telegram — один процесс) или кнопкой «Самопроверка» в пульте.
    Selftest {
        #[arg(long, default_value = "1")]
        amount: domain::Decimal,
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
    /// Слушатель для ноутбука (docs/RECORDING.md): войти в тестовый аккаунт прямо здесь и
    /// записывать ответы @send и @xrocket в файл. Без базы данных; сессия только в памяти, на
    /// выходе — выход из аккаунта. Ничего не отправляет. Нужны TG_API_ID и TG_API_HASH (или ввод).
    Listen {
        /// Куда писать (JSON lines).
        #[arg(long, default_value = "exch-record.jsonl")]
        out: PathBuf,
        /// Сначала записать столько последних сообщений каждого чата.
        #[arg(long, default_value_t = 300)]
        history: u32,
    },
    /// Премиум-эмодзи клиентского бота: вариация набора, свои эмодзи по ролям, установка
    /// наборов exch в Telegram. Вариация и свои эмодзи есть и в пульте; клиентский бот
    /// подхватывает изменения в течение минуты.
    #[command(subcommand)]
    Emoji(EmojiCommand),
}

#[derive(Debug, Subcommand)]
enum EmojiCommand {
    /// Вариация, наборы и источник эмодзи каждой роли.
    Show,
    /// Вариация набора: animated, static или off (обычные символы).
    Style {
        #[arg(value_parser = parse_style)]
        style: ui::EmojiStyle,
    },
    /// Свой премиум-эмодзи на роль (сильнее набора): `exch emoji set ok 5368324170671202286`.
    /// id — из сущности `custom_emoji` сообщения с этим эмодзи.
    Set { role: String, id: String },
    /// Вернуть роли эмодзи набора.
    Reset { role: String },
    /// Взять уже установленные наборы exch другого бота, ничего не загружая:
    /// `exch emoji adopt --from @my_old_bot`. Нужен Premium у владельца этого бота.
    Adopt {
        #[arg(long)]
        from: String,
        #[arg(long)]
        dir: Option<PathBuf>,
    },
    /// Создать или дописать наборы exch (анимированные и статичные) от имени клиентского бота
    /// и сохранить их id. Владелец набора — `--owner` (Telegram id, писал боту /start).
    Install {
        #[arg(long)]
        owner: i64,
        /// Папка набора (`manifest.json`, `color/`, `mono/`, `static/`).
        #[arg(long)]
        dir: Option<PathBuf>,
    },
}

fn parse_style(s: &str) -> Result<ui::EmojiStyle, String> {
    ui::EmojiStyle::from_key(s).ok_or_else(|| format!("unknown style {s}: animated, static or off"))
}

fn parse_platform(s: &str) -> Result<domain::Platform, String> {
    domain::Platform::from_db_name(s)
        .ok_or_else(|| format!("unknown platform {s}: use cryptobot or xrocket"))
}

/// Прочитать файл переменных. Блокнот Windows сохраняет UTF-8 с BOM в начале — его убираем,
/// иначе первая строка не разберётся.
fn load_env_file(path: &std::path::Path) -> Result<(), String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    dotenvy::from_read(std::io::Cursor::new(text.as_bytes())).map_err(|e| e.to_string())
}

/// Переменные из `config.toml`: только те, что не заданы окружением или `.env` (пустое
/// значение = не задано). Вызывается до запуска рантайма — потоков ещё нет.
fn load_settings_file(path: &std::path::Path) -> Result<(), String> {
    let mut values = app::settings_file::load(path)?;
    values.retain(|k, _| std::env::var(k).map_or(true, |v| v.trim().is_empty()));
    let text = app::project::dotenv_text(&values);
    dotenvy::from_read_override(std::io::Cursor::new(text.into_bytes())).map_err(|e| e.to_string())
}

fn main() -> ExitCode {
    // Двойной щелчок по exch.exe: аргументов нет, окно закроется сразу после выхода.
    let bare = std::env::args_os().len() <= 1;
    let cli = Cli::parse();
    let paths = match cli.env_file.clone() {
        Some(env) => app::project::Paths::for_env_file(env),
        None => app::project::locate(),
    };
    let has_env = paths.env.is_file();
    // Первый запуск без команды и без .env — сразу мастер настройки.
    let command = cli
        .command
        .unwrap_or(if has_env || std::env::var_os("DATABASE_URL").is_some() {
            Command::Run
        } else {
            Command::Setup
        });
    let code = start(command, &paths, has_env, bare);
    if bare && code != ExitCode::SUCCESS {
        app::prompt::pause();
    }
    code
}

fn start(command: Command, paths: &app::project::Paths, has_env: bool, bare: bool) -> ExitCode {
    // Мастер читает и пишет файлы сам: переменные из старого .env ему не нужны.
    let wizard = matches!(command, Command::Setup);
    if !wizard {
        // .env необязателен (на сервере переменные приходят из окружения), но если он есть
        // и в нём ошибка — сказать об этом, а не молча работать без него.
        if has_env && let Err(e) = load_env_file(&paths.env) {
            eprintln!("Ошибка в {}: {e}", paths.env.display());
            return ExitCode::FAILURE;
        }
        // Ошибку в config.toml покажет `exch check` строкой отчёта.
        if let Err(e) = load_settings_file(&paths.config)
            && !matches!(command, Command::Check)
        {
            eprintln!("Ошибка в {}: {e}", paths.config.display());
            return ExitCode::FAILURE;
        }
    }
    // Вывод для человека: короткие строки, только предупреждения.
    let human = matches!(
        command,
        Command::Listen { .. } | Command::Emoji(_) | Command::Setup | Command::Check
    );
    // LOG_FORMAT=text — обычный текст, json — для сервера; не задан — текст в окне программы,
    // JSON, когда вывод идёт в файл или журнал сервера.
    let text = match std::env::var("LOG_FORMAT") {
        Ok(v) if !v.trim().is_empty() => v.trim().eq_ignore_ascii_case("text"),
        _ => std::io::IsTerminal::is_terminal(&std::io::stderr()),
    };
    init_tracing(human, text);

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
        match command {
            Command::Run => run().await,
            Command::Demo {
                preview,
                preview_panel,
                preview_only,
            } => demo(preview, preview_panel, preview_only).await,
            Command::Setup => setup(paths, bare).await,
            Command::Check => check(paths).await,
            Command::Migrate => migrate().await,
            Command::Selftest { amount } => selftest(amount).await,
            Command::GenSessionKey => {
                use base64::Engine as _;
                let key = mtproto::SessionKey::random_bytes()
                    .map_err(|e| anyhow::anyhow!("cannot generate a key: {e}"))?;
                println!(
                    "SESSION_KEY={}",
                    base64::engine::general_purpose::STANDARD.encode(key)
                );
                Ok(())
            }
            Command::Login { label, platform } => login(label, platform).await,
            Command::Record {
                label,
                out,
                history,
            } => record(label, out, history).await,
            Command::Listen { out, history } => listen(out, history).await,
            Command::Emoji(cmd) => emoji(cmd).await,
        }
    });
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) if human => {
            eprintln!("Ошибка: {e:#}");
            ExitCode::FAILURE
        }
        Err(e) => {
            tracing::error!(error = format!("{e:#}"), "exch stopped with an error");
            ExitCode::FAILURE
        }
    }
}

fn init_tracing(human: bool, text: bool) {
    if human || text {
        let default = if human { "warn" } else { "info" };
        let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default));
        tracing_subscriber::fmt()
            .compact()
            // Старая консоль Windows (cmd) показывает цветовые коды как мусор.
            .with_ansi(!cfg!(windows))
            .with_target(false)
            .with_env_filter(filter)
            .with_writer(std::io::stderr)
            .init();
        return;
    }
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(filter)
        .with_current_span(true)
        .init();
}

async fn listen(out: PathBuf, history: u32) -> anyhow::Result<()> {
    app::listen::run(&out, history).await
}

async fn emoji(cmd: EmojiCommand) -> anyhow::Result<()> {
    let cfg = load_config()?;
    let pool = connect_and_migrate(&cfg).await?;
    let mut conn = pool.acquire().await?;
    match cmd {
        EmojiCommand::Show => {
            print!(
                "{}",
                app::emoji::describe(&app::emoji::load(&mut conn).await?)
            );
        }
        EmojiCommand::Style { style } => {
            app::emoji::set_style(&mut conn, style).await?;
            println!("Вариация: {}. Перезапустите exch run.", style.key());
        }
        EmojiCommand::Set { role, id } => {
            let role = app::emoji::parse_role(&role)?;
            app::emoji::set_custom(&mut conn, role, &id).await?;
            println!("{}: свой эмодзи {id}. Перезапустите exch run.", role.key());
        }
        EmojiCommand::Reset { role } => {
            let role = app::emoji::parse_role(&role)?;
            let removed = app::emoji::reset_custom(&mut conn, role).await?;
            println!(
                "{}: {}",
                role.key(),
                if removed {
                    "снова из набора"
                } else {
                    "своего эмодзи не было"
                }
            );
        }
        EmojiCommand::Adopt { from, dir } => {
            let token = cfg
                .client_bot_token
                .clone()
                .context("CLIENT_BOT_TOKEN is not set: sets are read through the bot")?;
            let bot = app::telegram::bot_api(token)?;
            let dir = dir.unwrap_or_else(app::emoji::default_dir);
            let packs = app::emoji::adopt(&mut conn, &bot, &from, &dir).await?;
            println!(
                "Наборы взяты: анимированных {}, статичных {}. Перезапустите exch run.",
                packs.animated.len(),
                packs.still.len()
            );
        }
        EmojiCommand::Install { owner, dir } => {
            let token = cfg
                .client_bot_token
                .clone()
                .context("CLIENT_BOT_TOKEN is not set: the sets belong to the client bot")?;
            let bot = app::telegram::bot_api(token)?;
            let dir = dir.unwrap_or_else(app::emoji::default_dir);
            let packs = app::emoji::install(&mut conn, &bot, owner, &dir).await?;
            println!(
                "Наборы установлены: анимированных {}, статичных {}. Перезапустите exch run.",
                packs.animated.len(),
                packs.still.len()
            );
        }
    }
    Ok(())
}

fn load_config() -> anyhow::Result<Config> {
    Config::from_env().context("invalid configuration (see .env.example and SPEC §12, step 8)")
}

async fn connect_and_migrate(cfg: &Config) -> anyhow::Result<storage::PgPool> {
    app::db::connect_and_migrate(cfg.database_url.expose_secret()).await
}

async fn migrate() -> anyhow::Result<()> {
    let cfg = load_config()?;
    connect_and_migrate(&cfg).await.map(|_| ())
}

/// Мастер; если владелец хочет запустить бота сразу — новый процесс `exch run` в этом же
/// окне: он прочитает только что записанные .env и config.toml как при обычном запуске.
async fn setup(paths: &app::project::Paths, bare: bool) -> anyhow::Result<()> {
    if !app::setup::run(paths, bare).await? {
        if bare {
            app::prompt::pause();
        }
        return Ok(());
    }
    let exe = std::env::current_exe().context("cannot find exch.exe")?;
    let env = paths.env.clone();
    let status = tokio::task::spawn_blocking(move || {
        std::process::Command::new(exe)
            .arg("--env-file")
            .arg(env)
            .arg("run")
            .status()
    })
    .await
    .context("cannot start exch run")?
    .context("cannot start exch run")?;
    anyhow::ensure!(status.success(), "exch run stopped with {status}");
    Ok(())
}

async fn check(paths: &app::project::Paths) -> anyhow::Result<()> {
    println!("Проверка exch — {}\n", paths.dir.display());
    let report = app::check::run(paths).await;
    print!("{report}");
    anyhow::ensure!(
        report.count(app::check::Mark::Fail) == 0,
        "исправьте строки с ❌ и запустите exch check ещё раз"
    );
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

async fn selftest(amount: domain::Decimal) -> anyhow::Result<()> {
    let cfg = load_config()?;
    let pool = connect_and_migrate(&cfg).await?;
    let tg = TelegramSettings::from_config(&cfg)?;
    let log = Arc::new(app::wallet_log::WalletLog::new(
        pool.clone(),
        tg.bots().clone(),
    ));
    let userbots = app::runtime::connect_userbots(&pool, &tg, &log).await?;
    anyhow::ensure!(
        !userbots.is_empty(),
        "no active wallet accounts: run `exch login` first"
    );
    let engine = app::runtime::engine(&pool, &userbots);
    let steps = engine.selftest(amount).await?;
    let mut ok = !steps.is_empty();
    for s in &steps {
        ok &= s.ok;
        println!(
            "{} {:<9} {:<13} {}",
            if s.ok { "OK  " } else { "FAIL" },
            s.platform.db_name(),
            s.step,
            s.detail
        );
    }
    anyhow::ensure!(ok, "selftest failed: see the steps above");
    println!("selftest passed");
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
                        spawn_bot(&mut tasks, &cancel, &pool, engine, &cfg, token, None).await?;
                    }
                }
            }
        }
    }

    wait(tasks, cancel).await
}

/// Клиентский бот и пульт в нём (один токен) поверх движка. В демо перед пультом стоит
/// перехватчик проб `/demo`.
async fn spawn_bot(
    tasks: &mut JoinSet<Result<(), supervisor::GaveUp>>,
    cancel: &CancellationToken,
    pool: &storage::PgPool,
    engine: Arc<engine::Engine>,
    cfg: &Config,
    token: secrecy::SecretString,
    demo: Option<&app::demo::DemoWorld>,
) -> anyhow::Result<()> {
    // Один бот: админ-панель — внутри клиентского (решение владельца).
    let panel = app::runtime::ops_bot(pool, token.clone(), engine.clone(), cfg.owner_tg_id)?;
    {
        let panel = panel.clone();
        tasks.spawn(supervise(
            "panel-alerts",
            cancel.clone(),
            RestartPolicy::default(),
            move |token| {
                let panel = panel.clone();
                async move { panel.run(token).await.map_err(anyhow::Error::from) }
            },
        ));
    }
    let emoji = app::runtime::emoji_set(pool).await;
    let hook: Arc<dyn tgbot::UpdateHook> = match demo {
        Some(world) => Arc::new(app::demo::DemoHook::new(
            token.clone(),
            world,
            Arc::new(panel),
            emoji.clone(),
        )?),
        None => Arc::new(panel),
    };
    let bot = app::runtime::client_bot(pool, token, engine, cfg.support_contact.clone(), emoji)?
        .with_hook(hook);
    tasks.spawn(supervise(
        "client-bot",
        cancel.clone(),
        RestartPolicy::default(),
        move |token| {
            let bot = bot.clone();
            async move { bot.run(token).await.map_err(anyhow::Error::from) }
        },
    ));
    Ok(())
}

/// `exch demo`: тот же бот и пульт, кошельки — симулятор, база — `<база>_demo`.
async fn demo(preview: bool, preview_panel: bool, preview_only: Vec<String>) -> anyhow::Result<()> {
    let cfg = load_config()?;
    let token = cfg
        .client_bot_token
        .clone()
        .context("CLIENT_BOT_TOKEN is not set: the demo runs the real bot on the simulator")?;
    let url = app::demo::demo_database_url(cfg.database_url.expose_secret())?;
    let pool = app::demo::fresh_database(&url).await?;
    let base = engine::Engine::new(pool.clone(), engine::EngineConfig::default());
    let world = app::demo::seed(&pool, &base).await?;
    match app::demo::copy_emoji(cfg.database_url.expose_secret(), &pool).await {
        Ok(n) => tracing::info!(
            settings = n,
            "demo: emoji settings copied from the main database"
        ),
        Err(e) => {
            tracing::warn!(error = %e, "demo: emoji settings not copied, plain emoji are used")
        }
    }
    let engine = app::demo::wallets(&pool, &world)
        .await?
        .into_iter()
        .fold(base, engine::Engine::with_wallet);
    let engine = Arc::new(engine);
    tracing::warn!(
        "DEMO: wallets are simulated, no money moves; separate database *_demo; send /demo to the bot"
    );
    let cancel = CancellationToken::new();
    let mut tasks: JoinSet<Result<(), supervisor::GaveUp>> = JoinSet::new();
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
    spawn_bot(
        &mut tasks,
        &cancel,
        &pool,
        engine,
        &cfg,
        token.clone(),
        Some(&world),
    )
    .await?;
    if preview || preview_panel || !preview_only.is_empty() {
        let api = app::telegram::bot_api(token)?;
        let look = ops::Look::new(app::runtime::emoji_set(&pool).await);
        if preview {
            ops::preview::send_preview(&api, &look, cfg.owner_tg_id).await;
        } else if preview_panel {
            ops::preview::send_panel_preview(&api, &look, cfg.owner_tg_id).await;
        }
        for word in &preview_only {
            ops::preview::send_client_preview(&api, &look, cfg.owner_tg_id, Some(word)).await;
        }
    }
    wait(tasks, cancel).await
}

/// Работать до сигнала остановки или до отказа задачи; затем мягкая остановка.
async fn wait(
    mut tasks: JoinSet<Result<(), supervisor::GaveUp>>,
    cancel: CancellationToken,
) -> anyhow::Result<()> {
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
