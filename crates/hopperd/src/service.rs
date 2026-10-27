//! Running hopperd in the background, started with the machine:
//! `hopperd service install` makes it a Windows service or a systemd unit;
//! `uninstall`, `update` and `status` do what they say. Installing needs an
//! admin (Windows) or root (Linux) shell; on Windows the first-run dialog
//! and the tray ask for it with one UAC prompt (see `launcher`).
//!
//! Minecraft servers keep running when the service stops or restarts, as
//! they do when hopperd is closed.

use anyhow::{Context as _, bail};
use std::path::{Path, PathBuf};

const DISPLAY: &str = "Hopper Server";
const ABOUT: &str = "Hopper Server: Minecraft server panel (web UI and API).";
const USAGE: &str = "Use: hopperd service install|uninstall|update|start|stop|restart|status [--data <dir>] [--copy]";

/// The service's name: `hopperd`. `HOPPERD_SERVICE` changes it (and the
/// Start-with-Windows entry), so a test copy can run next to the real one.
pub fn name() -> String {
    std::env::var("HOPPERD_SERVICE").ok().filter(|n| !n.trim().is_empty()).unwrap_or_else(|| "hopperd".into())
}

/// The display name, with the test name when there is one.
pub fn display() -> String {
    match name().as_str() {
        "hopperd" => DISPLAY.into(),
        n => format!("{DISPLAY} ({n})"),
    }
}

/// Where `install --copy` puts the exe: `C:\Program Files\Hopper Server`
/// (`HOPPERD_INSTALL_DIR` for tests).
pub fn install_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("HOPPERD_INSTALL_DIR") {
        return PathBuf::from(d);
    }
    let pf = std::env::var_os("ProgramFiles").map_or_else(|| PathBuf::from(r"C:\Program Files"), PathBuf::from);
    pf.join(DISPLAY)
}

/// `hopperd service <what> [--data <dir>] [--copy] [--remove-files] [--report <file>]`.
/// `--report` writes what went wrong to a file, for the elevated copy the
/// launcher starts (it has no console to show it on).
pub fn command(args: &[String]) -> anyhow::Result<()> {
    // The launcher's elevated copy doesn't inherit its environment: a test
    // name and install folder come as options.
    for (opt, var) in [("--name", "HOPPERD_SERVICE"), ("--install-dir", "HOPPERD_INSTALL_DIR")] {
        if let Some(v) = args.iter().position(|a| a == opt).and_then(|i| args.get(i + 1)) {
            // SAFETY: set before anything else runs (no other threads yet).
            unsafe { std::env::set_var(var, v) };
        }
    }
    let report = args.iter().position(|a| a == "--report").and_then(|i| args.get(i + 1)).map(PathBuf::from);
    let r = run_command(args);
    if let (Err(e), Some(p)) = (&r, &report) {
        let _ = std::fs::write(p, format!("{e:#}"));
    }
    r
}

fn run_command(args: &[String]) -> anyhow::Result<()> {
    let what = args.first().map(String::as_str).unwrap_or("");
    let mut data: Option<PathBuf> = None;
    let (mut copy, mut remove_files) = (false, false);
    let mut it = args.iter().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--data" => data = Some(it.next().context("--data needs a folder")?.into()),
            "--copy" => copy = true,
            "--remove-files" => remove_files = true,
            "--report" | "--name" | "--install-dir" => {
                it.next();
            }
            other => bail!("Unknown option {other}. {USAGE}"),
        }
    }
    match what {
        "install" => {
            let mut exe = std::env::current_exe()?;
            if copy {
                exe = copy_to_install_dir(&exe)?;
            }
            let data = crate::absolute(&data.unwrap_or_else(default_data));
            std::fs::create_dir_all(&data).with_context(|| format!("creating {}", data.display()))?;
            imp::install(&exe, &data)?;
            println!("Installed and started the {} service.", display());
            println!("{}", where_to_open(&data));
            Ok(())
        }
        "uninstall" => {
            let exe = imp::uninstall()?;
            if remove_files && let Some(exe) = exe.filter(|e| e.parent() == Some(install_dir().as_path())) {
                let _ = std::fs::remove_file(&exe);
                let _ = std::fs::remove_dir(install_dir());
            }
            println!("Removed the {} service. Its data folder is still there.", display());
            Ok(())
        }
        #[cfg(windows)]
        "start" => imp::start(),
        #[cfg(windows)]
        "stop" => imp::stop_service(),
        #[cfg(windows)]
        "restart" => imp::restart(),
        #[cfg(windows)]
        "update" => {
            imp::update(&std::env::current_exe()?)?;
            println!("Updated the {} service to {}.", display(), env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        "status" => {
            println!("{}", imp::status()?);
            Ok(())
        }
        _ => bail!("{USAGE}"),
    }
}

/// Copies the running exe to [`install_dir`] (if it isn't there already).
fn copy_to_install_dir(exe: &Path) -> anyhow::Result<PathBuf> {
    let dir = install_dir();
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let to = dir.join(exe.file_name().unwrap_or("hopperd.exe".as_ref()));
    if crate::absolute(exe) != crate::absolute(&to) {
        copy_retrying(exe, &to)?;
    }
    Ok(to)
}

/// Copies over an exe that may still be closing.
pub(crate) fn copy_retrying(from: &Path, to: &Path) -> anyhow::Result<()> {
    let mut last = None;
    for _ in 0..40 {
        match std::fs::copy(from, to) {
            Ok(_) => return Ok(()),
            Err(e) => last = Some(e),
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    Err(last.unwrap()).with_context(|| format!("copying {} to {}", from.display(), to.display()))
}

/// The settings in `dir` without creating them.
fn read_config(dir: &Path) -> Option<crate::config::Config> {
    toml::from_str(&std::fs::read_to_string(dir.join(crate::config::FILE)).ok()?).ok()
}

/// The panel's real data folder and settings when it starts from `boot`:
/// with a panel folder, `boot` only points to it (see `home`). Reads only.
pub fn real_data(boot: &Path) -> (PathBuf, crate::config::Config) {
    let cfg = read_config(boot).unwrap_or_default();
    if let Some(home) = &cfg.home {
        let data = home.join(crate::home::PANEL_DIR);
        if let Some(inner) = read_config(&data) {
            return (data, inner);
        }
    }
    (boot.to_path_buf(), cfg)
}

/// Whether the panel with its data in `data` has an owner yet.
pub fn has_owner(data: &Path) -> bool {
    crate::auth::Auth::load(data).is_ok_and(|a| !a.needs_setup())
}

/// "Open the panel at …", with how to set it up when nobody has yet.
fn where_to_open(boot: &Path) -> String {
    let (data, cfg) = real_data(boot);
    let url = format!("http://localhost:{}", cfg.port);
    if has_owner(&data) {
        format!("Open the panel at {url}")
    } else {
        format!(
            "Open {url} in a browser on this PC to set the owner password. From another device you need the setup code in {}.",
            data.join(crate::SETUP_FILE).display()
        )
    }
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
            name: name().into(),
            display_name: display().into(),
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

    fn stop_and_wait(s: &windows_service::service::Service) -> anyhow::Result<()> {
        if s.query_status()?.current_state != ServiceState::Stopped {
            let _ = s.stop();
            for _ in 0..80 {
                if s.query_status()?.current_state == ServiceState::Stopped {
                    return Ok(());
                }
                std::thread::sleep(Duration::from_millis(250));
            }
            bail!("the service didn't stop within 20 seconds");
        }
        Ok(())
    }

    /// Removes the service; returns the exe it ran.
    pub fn uninstall() -> anyhow::Result<Option<PathBuf>> {
        let m = manager(ServiceManagerAccess::CONNECT)?;
        let s = m
            .open_service(name(), ServiceAccess::STOP | ServiceAccess::DELETE | ServiceAccess::QUERY_STATUS | ServiceAccess::QUERY_CONFIG)
            .context("the service isn't installed")?;
        let exe = s.query_config().ok().map(|c| split_command(&c.executable_path.to_string_lossy()).0);
        stop_and_wait(&s)?;
        s.delete().context("removing the service")?;
        Ok(exe)
    }

    pub fn start() -> anyhow::Result<()> {
        let m = manager(ServiceManagerAccess::CONNECT)?;
        let s = m.open_service(name(), ServiceAccess::START).context("the service isn't installed")?;
        s.start::<&str>(&[]).context("starting the service")
    }

    /// Stops the panel (servers keep running).
    pub fn stop_service() -> anyhow::Result<()> {
        let m = manager(ServiceManagerAccess::CONNECT)?;
        let s = m.open_service(name(), ServiceAccess::STOP | ServiceAccess::QUERY_STATUS).context("the service isn't installed")?;
        stop_and_wait(&s)
    }

    pub fn restart() -> anyhow::Result<()> {
        let m = manager(ServiceManagerAccess::CONNECT)?;
        let s = m
            .open_service(name(), ServiceAccess::STOP | ServiceAccess::START | ServiceAccess::QUERY_STATUS)
            .context("the service isn't installed")?;
        stop_and_wait(&s)?;
        s.start::<&str>(&[]).context("starting the service")
    }

    /// Replaces the service's exe with `new` and starts it again. Servers
    /// keep running meanwhile; the panel finds them again.
    pub fn update(new: &Path) -> anyhow::Result<()> {
        let m = manager(ServiceManagerAccess::CONNECT)?;
        let s = m
            .open_service(name(), ServiceAccess::STOP | ServiceAccess::START | ServiceAccess::QUERY_STATUS | ServiceAccess::QUERY_CONFIG)
            .context("the service isn't installed")?;
        let exe = split_command(&s.query_config()?.executable_path.to_string_lossy()).0;
        if crate::absolute(&exe) == crate::absolute(new) {
            bail!("That's the copy the service runs already.");
        }
        stop_and_wait(&s)?;
        let copied = super::copy_retrying(new, &exe);
        s.start::<&str>(&[]).context("starting the service")?;
        copied
    }

    /// The installed service, as anyone can see it (no admin needed).
    pub struct Installed {
        pub running: bool,
        /// Starting, stopping or pausing right now.
        pub pending: bool,
        /// The exe the service runs.
        pub exe: PathBuf,
        pub data: Option<PathBuf>,
    }

    pub fn installed() -> Option<Installed> {
        let m = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT).ok()?;
        let s = m.open_service(name(), ServiceAccess::QUERY_STATUS | ServiceAccess::QUERY_CONFIG).ok()?;
        let state = s.query_status().ok()?.current_state;
        let running = state == ServiceState::Running;
        let pending = matches!(state, ServiceState::StartPending | ServiceState::StopPending | ServiceState::ContinuePending | ServiceState::PausePending);
        let (exe, data) = split_command(&s.query_config().ok()?.executable_path.to_string_lossy());
        Some(Installed { running, pending, exe, data })
    }

    /// The exe and `--data` folder from a service's command line.
    pub(super) fn split_command(cmd: &str) -> (PathBuf, Option<PathBuf>) {
        let cmd = cmd.trim();
        let (exe, rest) = match cmd.strip_prefix('"') {
            Some(r) => r.split_once('"').unwrap_or((r, "")),
            None => match cmd.find(" --") {
                Some(i) => (&cmd[..i], &cmd[i..]),
                None => (cmd, ""),
            },
        };
        let data = rest.split_once("--data").map(|(_, d)| {
            let d = d.trim();
            match d.strip_prefix('"') {
                Some(q) => q.split_once('"').map_or(q, |(a, _)| a).to_string(),
                None => d.split(" --").next().unwrap_or(d).trim().to_string(),
            }
        });
        (PathBuf::from(exe.trim()), data.filter(|d| !d.is_empty()).map(PathBuf::from))
    }

    pub fn status() -> anyhow::Result<String> {
        let m = manager(ServiceManagerAccess::CONNECT)?;
        let Ok(s) = m.open_service(name(), ServiceAccess::QUERY_STATUS | ServiceAccess::QUERY_CONFIG) else {
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
        windows_service::service_dispatcher::start(name(), ffi_main).context("not started by the service manager")?;
        Ok(())
    }

    fn service_main(_args: Vec<OsString>) {
        let handle = service_control_handler::register(name(), |c| match c {
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
        systemctl(&["enable", "--now", "hopperd"])
    }

    pub fn uninstall() -> anyhow::Result<Option<PathBuf>> {
        let _ = systemctl(&["disable", "--now", "hopperd"]);
        std::fs::remove_file(UNIT).with_context(|| format!("removing {UNIT} (run with sudo)"))?;
        systemctl(&["daemon-reload"])?;
        Ok(None)
    }

    pub fn status() -> anyhow::Result<String> {
        if !Path::new(UNIT).exists() {
            return Ok("Not installed.".into());
        }
        let out = Command::new("systemctl").args(["is-active", "hopperd"]).output()?;
        Ok(format!("{} ({UNIT})", String::from_utf8_lossy(&out.stdout).trim()))
    }
}

#[cfg(windows)]
pub use imp::{installed, run as run_as_service};

#[cfg(all(test, windows))]
mod win_tests {
    use std::path::PathBuf;

    #[test]
    fn service_command_lines() {
        let (exe, data) = super::imp::split_command(r#""C:\Program Files\Hopper Server\hopperd.exe" --service --data "C:\Users\me\AppData\Roaming\hopperd\data""#);
        assert_eq!(exe, PathBuf::from(r"C:\Program Files\Hopper Server\hopperd.exe"));
        assert_eq!(data, Some(PathBuf::from(r"C:\Users\me\AppData\Roaming\hopperd\data")));
        let (exe, data) = super::imp::split_command(r"E:\hopperd.exe --service --data E:\data");
        assert_eq!((exe, data), (PathBuf::from(r"E:\hopperd.exe"), Some(PathBuf::from(r"E:\data"))));
    }
}

#[cfg(test)]
mod data_tests {
    #[test]
    fn port_from_the_panel_folder() {
        let base = std::env::temp_dir().join(format!("hopperd-svc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let (boot, home) = (base.join("boot"), base.join("Minecraft"));
        let inner = home.join(crate::home::PANEL_DIR);
        std::fs::create_dir_all(&boot).unwrap();
        // Only a pointer in the start-up folder; the port lives in the panel folder.
        std::fs::write(boot.join(crate::config::FILE), format!("home = {:?}\n", home.to_string_lossy())).unwrap();
        crate::config::Config { port: 8796, home: Some(home.clone()), ..Default::default() }.save(&inner).unwrap();
        let (data, cfg) = super::real_data(&boot);
        assert_eq!((data, cfg.port), (inner, 8796));
        assert!(super::where_to_open(&boot).contains("http://localhost:8796"));
        let _ = std::fs::remove_dir_all(&base);
    }
}

#[cfg(all(test, unix))]
mod tests {
    #[test]
    fn unit_file() {
        let u = super::imp::unit(std::path::Path::new("/opt/hopperd/hopperd"), std::path::Path::new("/home/mc/.local/share/hopperd"), "mc");
        assert!(u.contains("User=mc") && u.contains("KillMode=process") && u.contains("--data \"/home/mc/.local/share/hopperd\""));
    }
}
