//! Running hopperd in the background, started with the machine:
//! `hopperd service install` makes it a Windows service or a systemd unit;
//! `uninstall` and `status` do what they say. Installing needs an admin
//! (Windows) or root (Linux) shell.
//!
//! Minecraft servers keep running when the service stops or restarts, as
//! they do when hopperd is closed.

use anyhow::{Context as _, bail};
use std::path::{Path, PathBuf};

pub const NAME: &str = "hopperd";
const DISPLAY: &str = "Hopper Server";
const ABOUT: &str = "Hopper Server: Minecraft server panel (web UI and API).";

/// `hopperd service <what> [--data <dir>] [--servers-root <dir>]`.
pub fn command(args: &[String]) -> anyhow::Result<()> {
    let what = args.first().map(String::as_str).unwrap_or("");
    let mut data: Option<PathBuf> = None;
    let mut it = args.iter().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--data" => data = Some(it.next().context("--data needs a folder")?.into()),
            other => bail!("Unknown option {other}. Use: hopperd service install|uninstall|status [--data <dir>]"),
        }
    }
    match what {
        "install" => {
            let exe = std::env::current_exe()?;
            let data = crate::absolute(&data.unwrap_or_else(default_data));
            std::fs::create_dir_all(&data).with_context(|| format!("creating {}", data.display()))?;
            imp::install(&exe, &data)?;
            println!("Installed and started the {DISPLAY} service.");
            println!("Data: {}", data.display());
            println!("Open the panel at http://localhost:{} (the setup code is in {}).", port_of(&data), data.join(crate::SETUP_FILE).display());
            Ok(())
        }
        "uninstall" => {
            imp::uninstall()?;
            println!("Removed the {DISPLAY} service. Its data folder is still there.");
            Ok(())
        }
        "status" => {
            println!("{}", imp::status()?);
            Ok(())
        }
        _ => bail!("Use: hopperd service install|uninstall|status [--data <dir>]"),
    }
}

fn port_of(data: &Path) -> u16 {
    crate::config::Config::load_or_create(data).map_or(crate::config::DEFAULT_PORT, |c| c.port)
}

/// The data folder of whoever installs (on Linux, the user behind sudo).
fn default_data() -> PathBuf {
    #[cfg(unix)]
    if let Some(home) = imp::sudo_home() {
        return home.join(".local/share/hopperd");
    }
    crate::config::data_dir()
}

#[cfg(windows)]
mod imp {
    use super::*;
    use std::ffi::OsString;
    use std::sync::OnceLock;
    use std::time::Duration;
    use windows_service::service::{
        ServiceAccess, ServiceControl, ServiceControlAccept, ServiceErrorControl, ServiceExitCode, ServiceInfo, ServiceStartType,
        ServiceState, ServiceStatus, ServiceType,
    };
    use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};

    fn manager(access: ServiceManagerAccess) -> anyhow::Result<ServiceManager> {
        ServiceManager::local_computer(None::<&str>, access)
            .context("opening the service manager (run this from an admin terminal)")
    }

    pub fn install(exe: &Path, data: &Path) -> anyhow::Result<()> {
        let m = manager(ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE)?;
        let info = ServiceInfo {
            name: NAME.into(),
            display_name: DISPLAY.into(),
            service_type: ServiceType::OWN_PROCESS,
            start_type: ServiceStartType::AutoStart,
            error_control: ServiceErrorControl::Normal,
            executable_path: exe.to_path_buf(),
            launch_arguments: vec![OsString::from("--service"), OsString::from("--data"), data.as_os_str().to_owned()],
            dependencies: vec![],
            // LocalSystem. To run as a user (for their Java or drives), change
            // "Log On" in services.msc.
            account_name: None,
            account_password: None,
        };
        let s = m
            .create_service(&info, ServiceAccess::START | ServiceAccess::CHANGE_CONFIG | ServiceAccess::QUERY_STATUS)
            .context("creating the service (is it installed already? hopperd service uninstall first)")?;
        let _ = s.set_description(ABOUT);
        // Start again after a crash.
        let _ = s.update_failure_actions(windows_service::service::ServiceFailureActions {
            reset_period: windows_service::service::ServiceFailureResetPeriod::After(Duration::from_secs(86_400)),
            reboot_msg: None,
            command: None,
            actions: Some(vec![windows_service::service::ServiceAction {
                action_type: windows_service::service::ServiceActionType::Restart,
                delay: Duration::from_secs(10),
            }]),
        });
        s.start::<&str>(&[]).context("starting the service")?;
        Ok(())
    }

    pub fn uninstall() -> anyhow::Result<()> {
        let m = manager(ServiceManagerAccess::CONNECT)?;
        let s = m
            .open_service(NAME, ServiceAccess::STOP | ServiceAccess::DELETE | ServiceAccess::QUERY_STATUS)
            .context("the service isn't installed")?;
        if s.query_status()?.current_state != ServiceState::Stopped {
            let _ = s.stop();
            for _ in 0..40 {
                if s.query_status()?.current_state == ServiceState::Stopped {
                    break;
                }
                std::thread::sleep(Duration::from_millis(250));
            }
        }
        s.delete().context("removing the service")?;
        Ok(())
    }

    pub fn status() -> anyhow::Result<String> {
        let m = manager(ServiceManagerAccess::CONNECT)?;
        let Ok(s) = m.open_service(NAME, ServiceAccess::QUERY_STATUS | ServiceAccess::QUERY_CONFIG) else {
            return Ok("Not installed.".into());
        };
        let st = s.query_status()?;
        let cfg = s.query_config()?;
        Ok(format!("{:?}; runs {}", st.current_state, cfg.executable_path.display()))
    }

    // ---- running as the service ------------------------------------------------

    windows_service::define_windows_service!(ffi_main, service_main);

    fn stop() -> &'static tokio::sync::Notify {
        static STOP: OnceLock<tokio::sync::Notify> = OnceLock::new();
        STOP.get_or_init(tokio::sync::Notify::new)
    }

    /// Hands the process to the service manager; returns when it stops.
    pub fn run() -> anyhow::Result<()> {
        windows_service::service_dispatcher::start(NAME, ffi_main).context("not started by the service manager")?;
        Ok(())
    }

    fn service_main(_args: Vec<OsString>) {
        let handle = service_control_handler::register(NAME, |c| match c {
            ServiceControl::Stop | ServiceControl::Shutdown => {
                stop().notify_one();
                ServiceControlHandlerResult::NoError
            }
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
            _ => ServiceControlHandlerResult::NotImplemented,
        });
        let Ok(handle) = handle else { return };
        let status = |state, accept| ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: state,
            controls_accepted: accept,
            exit_code: ServiceExitCode::Win32(0),
            checkpoint: 0,
            wait_hint: Duration::from_secs(10),
            process_id: None,
        };
        let _ = handle.set_service_status(status(ServiceState::Running, ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN));
        // The process's own arguments carry --data (the service's are empty).
        if let Err(e) = crate::run_main(Box::pin(async { stop().notified().await })) {
            log::error!("hopperd stopped: {e:#}");
        }
        let _ = handle.set_service_status(status(ServiceState::Stopped, ServiceControlAccept::empty()));
    }
}

#[cfg(unix)]
mod imp {
    use super::*;
    use std::process::Command;

    const UNIT: &str = "/etc/systemd/system/hopperd.service";

    pub fn sudo_home() -> Option<PathBuf> {
        let user = std::env::var("SUDO_USER").ok().filter(|u| !u.is_empty() && u != "root")?;
        let out = Command::new("getent").args(["passwd", &user]).output().ok()?;
        let line = String::from_utf8_lossy(&out.stdout).into_owned();
        line.trim().split(':').nth(5).map(PathBuf::from)
    }

    fn systemctl(args: &[&str]) -> anyhow::Result<()> {
        let st = Command::new("systemctl").args(args).status().context("running systemctl (is this a systemd system?)")?;
        if !st.success() {
            bail!("systemctl {} failed", args.join(" "));
        }
        Ok(())
    }

    pub fn unit(exe: &Path, data: &Path, user: &str) -> String {
        format!(
            "[Unit]\n\
             Description={ABOUT}\n\
             After=network-online.target\n\
             Wants=network-online.target\n\
             \n\
             [Service]\n\
             User={user}\n\
             ExecStart=\"{}\" --data \"{}\"\n\
             Restart=on-failure\n\
             RestartSec=5\n\
             # Minecraft servers keep running when hopperd restarts.\n\
             KillMode=process\n\
             \n\
             [Install]\n\
             WantedBy=multi-user.target\n",
            exe.display(),
            data.display()
        )
    }

    pub fn install(exe: &Path, data: &Path) -> anyhow::Result<()> {
        if !Path::new("/run/systemd/system").exists() {
            bail!("This system doesn't run systemd. Start hopperd from your init system instead.");
        }
        let user = std::env::var("SUDO_USER").ok().filter(|u| !u.is_empty()).unwrap_or_else(|| "root".into());
        std::fs::write(UNIT, unit(exe, data, &user)).with_context(|| format!("writing {UNIT} (run with sudo)"))?;
        if user != "root" {
            let _ = Command::new("chown").args(["-R", &format!("{user}:"), &data.to_string_lossy()]).status();
        }
        systemctl(&["daemon-reload"])?;
        systemctl(&["enable", "--now", NAME])
    }

    pub fn uninstall() -> anyhow::Result<()> {
        let _ = systemctl(&["disable", "--now", NAME]);
        std::fs::remove_file(UNIT).with_context(|| format!("removing {UNIT} (run with sudo)"))?;
        systemctl(&["daemon-reload"])
    }

    pub fn status() -> anyhow::Result<String> {
        if !Path::new(UNIT).exists() {
            return Ok("Not installed.".into());
        }
        let out = Command::new("systemctl").args(["is-active", NAME]).output()?;
        Ok(format!("{} ({UNIT})", String::from_utf8_lossy(&out.stdout).trim()))
    }
}

#[cfg(windows)]
pub use imp::run as run_as_service;

#[cfg(all(test, unix))]
mod tests {
    #[test]
    fn unit_file() {
        let u = super::imp::unit(std::path::Path::new("/opt/hopperd/hopperd"), std::path::Path::new("/home/mc/.local/share/hopperd"), "mc");
        assert!(u.contains("User=mc") && u.contains("KillMode=process") && u.contains("--data \"/home/mc/.local/share/hopperd\""));
    }
}
