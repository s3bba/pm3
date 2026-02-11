use color_eyre::eyre::{WrapErr, bail};
use std::path::PathBuf;

pub fn install() -> color_eyre::Result<()> {
    let exe = std::env::current_exe().wrap_err("could not determine pm3 executable path")?;
    let exe_path = exe.to_string_lossy();
    let path = service_file_path()?;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .wrap_err_with(|| format!("could not create directory {}", parent.display()))?;
    }

    let content = generate_service_content(&exe_path);
    std::fs::write(&path, content)
        .wrap_err_with(|| format!("could not write service file {}", path.display()))?;

    eprintln!("Service installed: {}", path.display());
    post_install(&path);
    Ok(())
}

pub fn uninstall() -> color_eyre::Result<()> {
    let path = service_file_path()?;

    if !path.exists() {
        eprintln!("No service file found at {}", path.display());
        return Ok(());
    }

    pre_uninstall(&path);
    std::fs::remove_file(&path)
        .wrap_err_with(|| format!("could not remove service file {}", path.display()))?;

    eprintln!("Service removed: {}", path.display());
    Ok(())
}

#[cfg(target_os = "macos")]
fn service_file_path() -> color_eyre::Result<PathBuf> {
    let Some(home) = dirs::home_dir() else {
        bail!("could not determine home directory");
    };
    Ok(home.join("Library/LaunchAgents/com.pm3.daemon.plist"))
}

#[cfg(target_os = "linux")]
fn service_file_path() -> color_eyre::Result<PathBuf> {
    let Some(home) = dirs::home_dir() else {
        bail!("could not determine home directory");
    };
    Ok(home.join(".config/systemd/user/pm3.service"))
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn service_file_path() -> color_eyre::Result<PathBuf> {
    bail!("startup/unstartup is not supported on this platform");
}

#[cfg(target_os = "macos")]
fn generate_service_content(exe_path: &str) -> String {
    generate_launchd_plist(exe_path)
}

#[cfg(target_os = "linux")]
fn generate_service_content(exe_path: &str) -> String {
    generate_systemd_unit(exe_path)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn generate_service_content(_exe_path: &str) -> String {
    String::new()
}

#[cfg(target_os = "macos")]
fn post_install(path: &std::path::Path) {
    let status = std::process::Command::new("launchctl")
        .args(["load", &path.to_string_lossy()])
        .status();
    match status {
        Ok(s) if s.success() => eprintln!("Service loaded via launchctl"),
        Ok(s) => eprintln!("warning: launchctl load exited with {s}"),
        Err(e) => eprintln!("warning: could not run launchctl load: {e}"),
    }
}

#[cfg(target_os = "macos")]
fn pre_uninstall(path: &std::path::Path) {
    let status = std::process::Command::new("launchctl")
        .args(["unload", &path.to_string_lossy()])
        .status();
    match status {
        Ok(s) if s.success() => eprintln!("Service unloaded via launchctl"),
        Ok(s) => eprintln!("warning: launchctl unload exited with {s}"),
        Err(e) => eprintln!("warning: could not run launchctl unload: {e}"),
    }
}

#[cfg(target_os = "linux")]
fn post_install(_path: &std::path::Path) {
    let reload = std::process::Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .status();
    match reload {
        Ok(s) if s.success() => {}
        Ok(s) => eprintln!("warning: systemctl daemon-reload exited with {s}"),
        Err(e) => eprintln!("warning: could not run systemctl daemon-reload: {e}"),
    }

    let enable = std::process::Command::new("systemctl")
        .args(["--user", "enable", "pm3"])
        .status();
    match enable {
        Ok(s) if s.success() => eprintln!("Service enabled via systemctl"),
        Ok(s) => eprintln!("warning: systemctl enable exited with {s}"),
        Err(e) => eprintln!("warning: could not run systemctl enable: {e}"),
    }
}

#[cfg(target_os = "linux")]
fn pre_uninstall(_path: &std::path::Path) {
    let disable = std::process::Command::new("systemctl")
        .args(["--user", "disable", "pm3"])
        .status();
    match disable {
        Ok(s) if s.success() => eprintln!("Service disabled via systemctl"),
        Ok(s) => eprintln!("warning: systemctl disable exited with {s}"),
        Err(e) => eprintln!("warning: could not run systemctl disable: {e}"),
    }

    let reload = std::process::Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .status();
    match reload {
        Ok(s) if s.success() => {}
        Ok(s) => eprintln!("warning: systemctl daemon-reload exited with {s}"),
        Err(e) => eprintln!("warning: could not run systemctl daemon-reload: {e}"),
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn post_install(_path: &std::path::Path) {}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn pre_uninstall(_path: &std::path::Path) {}

pub fn generate_launchd_plist(exe_path: &str) -> String {
    let log_dir = dirs::data_dir()
        .map(|d| d.join("pm3").join("logs"))
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    let stdout_log = log_dir.join("daemon-out.log");
    let stderr_log = log_dir.join("daemon-err.log");

    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.pm3.daemon</string>
    <key>ProgramArguments</key>
    <array>
        <string>{exe_path}</string>
        <string>--daemon</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>{stdout}</string>
    <key>StandardErrorPath</key>
    <string>{stderr}</string>
</dict>
</plist>
"#,
        stdout = stdout_log.display(),
        stderr = stderr_log.display(),
    )
}

pub fn generate_systemd_unit(exe_path: &str) -> String {
    format!(
        r#"[Unit]
Description=pm3 process manager daemon
After=network.target

[Service]
Type=simple
ExecStart={exe_path} --daemon
Restart=on-failure

[Install]
WantedBy=default.target
"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_plist_contains_label() {
        let plist = generate_launchd_plist("/usr/local/bin/pm3");
        assert!(plist.contains("com.pm3.daemon"));
    }

    #[test]
    fn test_plist_contains_exe_path() {
        let plist = generate_launchd_plist("/usr/local/bin/pm3");
        assert!(plist.contains("/usr/local/bin/pm3"));
    }

    #[test]
    fn test_plist_contains_daemon_arg() {
        let plist = generate_launchd_plist("/usr/local/bin/pm3");
        assert!(plist.contains("--daemon"));
    }

    #[test]
    fn test_plist_contains_run_at_load() {
        let plist = generate_launchd_plist("/usr/local/bin/pm3");
        assert!(plist.contains("<key>RunAtLoad</key>"));
        assert!(plist.contains("<true/>"));
    }

    #[test]
    fn test_plist_contains_keep_alive() {
        let plist = generate_launchd_plist("/usr/local/bin/pm3");
        assert!(plist.contains("<key>KeepAlive</key>"));
    }

    #[test]
    fn test_plist_contains_log_paths() {
        let plist = generate_launchd_plist("/usr/local/bin/pm3");
        assert!(plist.contains("<key>StandardOutPath</key>"));
        assert!(plist.contains("<key>StandardErrorPath</key>"));
        assert!(plist.contains("daemon-out.log"));
        assert!(plist.contains("daemon-err.log"));
    }

    #[test]
    fn test_plist_valid_xml_structure() {
        let plist = generate_launchd_plist("/usr/local/bin/pm3");
        assert!(plist.starts_with("<?xml version="));
        assert!(plist.contains("<plist version=\"1.0\">"));
        assert!(plist.contains("</plist>"));
        assert!(plist.contains("<dict>"));
        assert!(plist.contains("</dict>"));
    }

    #[test]
    fn test_systemd_contains_exec_start() {
        let unit = generate_systemd_unit("/usr/local/bin/pm3");
        assert!(unit.contains("ExecStart=/usr/local/bin/pm3 --daemon"));
    }

    #[test]
    fn test_systemd_contains_description() {
        let unit = generate_systemd_unit("/usr/local/bin/pm3");
        assert!(unit.contains("Description="));
    }

    #[test]
    fn test_systemd_contains_restart_policy() {
        let unit = generate_systemd_unit("/usr/local/bin/pm3");
        assert!(unit.contains("Restart=on-failure"));
    }

    #[test]
    fn test_systemd_contains_all_sections() {
        let unit = generate_systemd_unit("/usr/local/bin/pm3");
        assert!(unit.contains("[Unit]"));
        assert!(unit.contains("[Service]"));
        assert!(unit.contains("[Install]"));
    }

    #[test]
    fn test_systemd_contains_wanted_by() {
        let unit = generate_systemd_unit("/usr/local/bin/pm3");
        assert!(unit.contains("WantedBy=default.target"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn test_service_file_path_macos() {
        let path = service_file_path().unwrap();
        assert!(
            path.to_string_lossy()
                .contains("Library/LaunchAgents/com.pm3.daemon.plist"),
            "expected macOS plist path, got: {}",
            path.display()
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_service_file_path_linux() {
        let path = service_file_path().unwrap();
        assert!(
            path.to_string_lossy()
                .contains(".config/systemd/user/pm3.service"),
            "expected Linux systemd path, got: {}",
            path.display()
        );
    }
}
