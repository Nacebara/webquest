//! Служба Windows (веха 10, решение владельца 17.11): `exch service install` регистрирует exch
//! настоящей службой — запуск при старте компьютера (отложенный, когда сеть уже есть), после
//! службы PostgreSQL, перезапуск при сбое (10 с, 30 с, 60 с; счётчик сбрасывается через сутки).
//! Остановка службы — та же мягкая остановка, что Ctrl+C: задачи доводятся до 60 с.
//!
//! Служба запускает `exch.exe service-run --env-file <полный путь к .env>`: у службы рабочая
//! папка — `C:\Windows\System32`, поэтому путь к настройкам передаётся явно.

use std::path::Path;

/// Имя службы (видно в «Службах» и в `sc query exch`).
pub const NAME: &str = "exch";
/// Как служба называется в списке «Службы».
pub const DISPLAY: &str = "exch — обменник CryptoBot ⇄ xRocket";
/// Описание в «Службах».
pub const DESCRIPTION: &str = "Бот-обменник exch. Остановка — мягкая: начатые заявки доводятся. \
Управление: exch service status | start | stop | uninstall.";
/// Службы PostgreSQL, которые стоит ждать: первая найденная станет зависимостью.
pub const POSTGRES_SERVICES: [&str; 6] = [
    "postgresql-x64-18",
    "postgresql-x64-17",
    "postgresql-x64-16",
    "postgresql-x64-15",
    "postgresql-x64-14",
    "postgresql",
];

/// Аргументы, с которыми служба запускает exch.
pub fn launch_arguments(env_file: &Path) -> Vec<String> {
    vec![
        "--env-file".to_owned(),
        env_file.display().to_string(),
        "service-run".to_owned(),
    ]
}

#[cfg(windows)]
pub use imp::{dispatch, install, start, status, stop, uninstall};

#[cfg(not(windows))]
mod other {
    use std::path::Path;

    const ONLY_WINDOWS: &str =
        "служба — только на Windows; здесь exch запускают systemd или вручную";

    pub fn install(_env_file: &Path, _pg_service: Option<&str>) -> anyhow::Result<String> {
        anyhow::bail!(ONLY_WINDOWS)
    }
    pub fn uninstall() -> anyhow::Result<()> {
        anyhow::bail!(ONLY_WINDOWS)
    }
    pub fn start() -> anyhow::Result<()> {
        anyhow::bail!(ONLY_WINDOWS)
    }
    pub fn stop() -> anyhow::Result<()> {
        anyhow::bail!(ONLY_WINDOWS)
    }
    pub fn status() -> anyhow::Result<String> {
        anyhow::bail!(ONLY_WINDOWS)
    }
    pub fn dispatch(_run: fn() -> i32) -> anyhow::Result<()> {
        anyhow::bail!(ONLY_WINDOWS)
    }
}

#[cfg(not(windows))]
pub use other::{dispatch, install, start, status, stop, uninstall};

#[cfg(windows)]
mod imp {
    use std::ffi::OsString;
    use std::path::Path;
    use std::sync::OnceLock;
    use std::time::Duration;

    use anyhow::Context as _;
    use windows_service::service::{
        ServiceAccess, ServiceAction, ServiceActionType, ServiceControl, ServiceControlAccept,
        ServiceDependency, ServiceErrorControl, ServiceExitCode, ServiceFailureActions,
        ServiceFailureResetPeriod, ServiceInfo, ServiceStartType, ServiceState, ServiceStatus,
        ServiceType,
    };
    use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
    use windows_service::{define_windows_service, service_dispatcher};

    use super::{DESCRIPTION, DISPLAY, NAME, POSTGRES_SERVICES, launch_arguments};

    fn explain(e: windows_service::Error) -> anyhow::Error {
        let text = e.to_string();
        if text.contains("Access is denied") || text.contains("os error 5") {
            anyhow::anyhow!(
                "нет прав: откройте PowerShell от имени администратора и повторите ({text})"
            )
        } else {
            anyhow::anyhow!(text)
        }
    }

    fn manager(access: ServiceManagerAccess) -> anyhow::Result<ServiceManager> {
        ServiceManager::local_computer(None::<&str>, access).map_err(explain)
    }

    /// Зарегистрировать службу. Возвращает имя службы PostgreSQL, от которой она зависит.
    pub fn install(env_file: &Path, pg_service: Option<&str>) -> anyhow::Result<String> {
        let m = manager(ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE)?;
        let pg = match pg_service {
            Some(name) => name.to_owned(),
            None => POSTGRES_SERVICES
                .iter()
                .find(|n| m.open_service(n, ServiceAccess::QUERY_STATUS).is_ok())
                .map(|n| (*n).to_owned())
                .context(
                    "не нашёл службу PostgreSQL (postgresql-x64-16 и т. п.): укажите её имя — exch service install --postgres <имя>",
                )?,
        };
        let exe = std::env::current_exe().context("не удалось узнать путь к exch.exe")?;
        let info = ServiceInfo {
            name: OsString::from(NAME),
            display_name: OsString::from(DISPLAY),
            service_type: ServiceType::OWN_PROCESS,
            start_type: ServiceStartType::AutoStart,
            error_control: ServiceErrorControl::Normal,
            executable_path: exe,
            launch_arguments: launch_arguments(env_file)
                .into_iter()
                .map(OsString::from)
                .collect(),
            dependencies: vec![ServiceDependency::Service(OsString::from(&pg))],
            account_name: None,
            account_password: None,
        };
        let service = m
            .create_service(
                &info,
                ServiceAccess::CHANGE_CONFIG | ServiceAccess::START | ServiceAccess::QUERY_STATUS,
            )
            .map_err(explain)?;
        service.set_description(DESCRIPTION).map_err(explain)?;
        // После перезагрузки — когда сеть уже поднялась.
        service.set_delayed_auto_start(true).map_err(explain)?;
        let restart = |secs| ServiceAction {
            action_type: ServiceActionType::Restart,
            delay: Duration::from_secs(secs),
        };
        service
            .update_failure_actions(ServiceFailureActions {
                reset_period: ServiceFailureResetPeriod::After(Duration::from_secs(24 * 3600)),
                reboot_msg: None,
                command: None,
                actions: Some(vec![restart(10), restart(30), restart(60)]),
            })
            .map_err(explain)?;
        // Перезапуск и тогда, когда exch вышел с ошибкой сам (а не только при падении).
        service
            .set_failure_actions_on_non_crash_failures(true)
            .map_err(explain)?;
        Ok(pg)
    }

    pub fn uninstall() -> anyhow::Result<()> {
        let m = manager(ServiceManagerAccess::CONNECT)?;
        let s = m
            .open_service(
                NAME,
                ServiceAccess::QUERY_STATUS | ServiceAccess::STOP | ServiceAccess::DELETE,
            )
            .map_err(explain)?;
        if s.query_status().map_err(explain)?.current_state != ServiceState::Stopped {
            s.stop().map_err(explain)?;
        }
        s.delete().map_err(explain)?;
        Ok(())
    }

    pub fn start() -> anyhow::Result<()> {
        let m = manager(ServiceManagerAccess::CONNECT)?;
        let s = m
            .open_service(NAME, ServiceAccess::START)
            .map_err(explain)?;
        s.start::<&str>(&[]).map_err(explain)
    }

    pub fn stop() -> anyhow::Result<()> {
        let m = manager(ServiceManagerAccess::CONNECT)?;
        let s = m.open_service(NAME, ServiceAccess::STOP).map_err(explain)?;
        s.stop().map_err(explain)?;
        Ok(())
    }

    pub fn status() -> anyhow::Result<String> {
        let m = manager(ServiceManagerAccess::CONNECT)?;
        let s = match m.open_service(NAME, ServiceAccess::QUERY_STATUS) {
            Ok(s) => s,
            Err(_) => return Ok("не установлена — exch service install".to_owned()),
        };
        let st = s.query_status().map_err(explain)?;
        Ok(match st.current_state {
            ServiceState::Running => "работает".to_owned(),
            ServiceState::Stopped => "остановлена — exch service start".to_owned(),
            ServiceState::StartPending => "запускается".to_owned(),
            ServiceState::StopPending => "останавливается (доводит начатые заявки)".to_owned(),
            other => format!("{other:?}"),
        })
    }

    /// Что запустить внутри службы: обычный `exch run`.
    static RUN: OnceLock<fn() -> i32> = OnceLock::new();
    /// Ручка состояния службы: «останавливаюсь» сообщается из обработчика команд.
    static HANDLE: OnceLock<service_control_handler::ServiceStatusHandle> = OnceLock::new();

    define_windows_service!(ffi_service_main, service_main);

    /// Точка входа службы: Windows вызывает её из диспетчера.
    pub fn dispatch(run: fn() -> i32) -> anyhow::Result<()> {
        let _ = RUN.set(run);
        service_dispatcher::start(NAME, ffi_service_main)
            .context("exch service-run запускает только диспетчер служб Windows")
    }

    fn status_of(state: ServiceState, code: u32, wait: Duration) -> ServiceStatus {
        ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: state,
            controls_accepted: if state == ServiceState::Running {
                ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN
            } else {
                ServiceControlAccept::empty()
            },
            exit_code: ServiceExitCode::Win32(code),
            checkpoint: 0,
            wait_hint: wait,
            process_id: None,
        }
    }

    fn service_main(_arguments: Vec<OsString>) {
        let handle = service_control_handler::register(NAME, |control| match control {
            ServiceControl::Stop | ServiceControl::Shutdown => {
                // Мягкая остановка до 60 с: просим Windows подождать чуть дольше.
                if let Some(h) = HANDLE.get() {
                    let _ = h.set_service_status(status_of(
                        ServiceState::StopPending,
                        0,
                        crate::stop::GRACE + Duration::from_secs(10),
                    ));
                }
                crate::stop::request_service_stop();
                ServiceControlHandlerResult::NoError
            }
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
            _ => ServiceControlHandlerResult::NotImplemented,
        });
        let Ok(handle) = handle else {
            return;
        };
        let _ = HANDLE.set(handle);
        let _ = handle.set_service_status(status_of(ServiceState::Running, 0, Duration::ZERO));
        let code = RUN.get().map_or(1, |run| run());
        let code = u32::try_from(code).unwrap_or(1);
        let _ = handle.set_service_status(status_of(ServiceState::Stopped, code, Duration::ZERO));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_service_starts_exch_with_its_settings_file() {
        let args = launch_arguments(Path::new(r"C:\exch\.env"));
        assert_eq!(args, ["--env-file", r"C:\exch\.env", "service-run"]);
        assert!(POSTGRES_SERVICES.contains(&"postgresql-x64-16"));
    }
}
