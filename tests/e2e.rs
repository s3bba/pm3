use assert_cmd::Command;
use assert_cmd::cargo::cargo_bin_cmd;
use pm3::config;
use pm3::protocol::{ProcessInfo, ProcessStatus, Response};
use predicates::prelude::*;
use std::path::Path;
use std::time::Duration;
use tempfile::TempDir;

fn pm3(data_dir: &Path, work_dir: &Path) -> Command {
    let mut cmd: Command = cargo_bin_cmd!("pm3");
    cmd.env("PM3_DATA_DIR", data_dir);
    cmd.current_dir(work_dir);
    cmd.timeout(Duration::from_secs(30));
    cmd
}

fn kill_daemon(data_dir: &Path, work_dir: &Path) {
    let _ = pm3(data_dir, work_dir).arg("kill").output();
    std::thread::sleep(Duration::from_millis(300));
}

fn parse_json_response(output: &std::process::Output) -> Response {
    let stdout = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(stdout.trim()).expect("failed to parse JSON response")
}

fn get_process_list(data_dir: &Path, work_dir: &Path) -> Vec<ProcessInfo> {
    let output = pm3(data_dir, work_dir)
        .args(["--json", "list"])
        .output()
        .unwrap();
    match parse_json_response(&output) {
        Response::ProcessList { processes } => processes,
        other => panic!("expected ProcessList, got: {other:?}"),
    }
}

fn find_process_pid(processes: &[ProcessInfo], name: &str) -> u32 {
    processes
        .iter()
        .find(|p| p.name == name)
        .unwrap_or_else(|| panic!("process '{name}' not found"))
        .pid
        .unwrap_or_else(|| panic!("process '{name}' has no pid"))
}

fn wait_until_online(data_dir: &Path, work_dir: &Path, name: &str, timeout_secs: u64) {
    let deadline = std::time::Instant::now() + Duration::from_secs(timeout_secs);
    loop {
        let processes = get_process_list(data_dir, work_dir);
        if let Some(p) = processes.iter().find(|p| p.name == name)
            && p.status == ProcessStatus::Online
        {
            return;
        }
        if std::time::Instant::now() >= deadline {
            panic!("timeout waiting for '{name}' to become online");
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

#[test]
fn test_e2e_stop_one_process_others_keep_running() {
    let dir = TempDir::new().unwrap();
    let work_dir = dir.path();
    let data_dir = dir.path().join("data");

    std::fs::write(
        work_dir.join("pm3.toml"),
        r#"
[web]
command = "sleep 999"

[worker]
command = "sleep 999"
"#,
    )
    .unwrap();

    // Start all processes
    pm3(&data_dir, work_dir).arg("start").assert().success();

    // Stop only web
    pm3(&data_dir, work_dir)
        .args(["stop", "web"])
        .assert()
        .success()
        .stdout(predicate::str::contains("stopped: web"));

    // Verify via list: web is stopped, worker is still online
    let processes = get_process_list(&data_dir, work_dir);

    let web = processes
        .iter()
        .find(|p| p.name == "web")
        .expect("web should appear in list");
    assert_eq!(web.status, ProcessStatus::Stopped, "web should be stopped");

    let worker = processes
        .iter()
        .find(|p| p.name == "worker")
        .expect("worker should appear in list");
    assert_eq!(
        worker.status,
        ProcessStatus::Online,
        "worker should be online"
    );

    kill_daemon(&data_dir, work_dir);
}

#[test]
fn test_e2e_stop_all_processes() {
    let dir = TempDir::new().unwrap();
    let work_dir = dir.path();
    let data_dir = dir.path().join("data");

    std::fs::write(
        work_dir.join("pm3.toml"),
        r#"
[web]
command = "sleep 999"

[worker]
command = "sleep 999"
"#,
    )
    .unwrap();

    // Start all processes
    pm3(&data_dir, work_dir).arg("start").assert().success();

    // Stop all (no name argument)
    pm3(&data_dir, work_dir)
        .arg("stop")
        .assert()
        .success()
        .stdout(predicate::str::contains("stopped:"));

    // Verify via list: all processes are stopped
    let processes = get_process_list(&data_dir, work_dir);
    assert!(!processes.is_empty(), "should have processes in list");
    for p in &processes {
        assert_eq!(
            p.status,
            ProcessStatus::Stopped,
            "process '{}' should be stopped",
            p.name
        );
    }

    kill_daemon(&data_dir, work_dir);
}

#[test]
fn test_e2e_start_stopped_processes() {
    let dir = TempDir::new().unwrap();
    let work_dir = dir.path();
    let data_dir = dir.path().join("data");

    std::fs::write(
        work_dir.join("pm3.toml"),
        r#"
[web]
command = "sleep 999"
"#,
    )
    .unwrap();

    pm3(&data_dir, work_dir).arg("start").assert().success();
    pm3(&data_dir, work_dir)
        .args(["stop", "web"])
        .assert()
        .success();

    let processes = get_process_list(&data_dir, work_dir);
    let web = processes
        .iter()
        .find(|p| p.name == "web")
        .expect("web should appear in list");
    assert_eq!(web.status, ProcessStatus::Stopped, "web should be stopped");

    pm3(&data_dir, work_dir)
        .args(["start", "web"])
        .assert()
        .success();

    wait_until_online(&data_dir, work_dir, "web", 10);

    let processes = get_process_list(&data_dir, work_dir);
    let web = processes
        .iter()
        .find(|p| p.name == "web")
        .expect("web should appear in list");
    assert_eq!(
        web.status,
        ProcessStatus::Online,
        "web should be online after start"
    );

    kill_daemon(&data_dir, work_dir);
}

#[test]
fn test_e2e_stop_nonexistent_prints_error() {
    let dir = TempDir::new().unwrap();
    let work_dir = dir.path();
    let data_dir = dir.path().join("data");

    std::fs::write(
        work_dir.join("pm3.toml"),
        r#"
[web]
command = "sleep 999"
"#,
    )
    .unwrap();

    // Start a process so the daemon has a process table
    pm3(&data_dir, work_dir).arg("start").assert().success();

    // Try to stop a nonexistent process (use --json to get structured error)
    let output = pm3(&data_dir, work_dir)
        .args(["--json", "stop", "nonexistent"])
        .output()
        .unwrap();
    let response = parse_json_response(&output);
    match response {
        Response::Error { message } => {
            assert!(
                message.contains("not found"),
                "error should contain 'not found', got: {message}"
            );
        }
        other => panic!("expected Error response, got: {other:?}"),
    }

    kill_daemon(&data_dir, work_dir);
}

#[test]
fn test_e2e_restart_one_process_gets_new_pid() {
    let dir = TempDir::new().unwrap();
    let work_dir = dir.path();
    let data_dir = dir.path().join("data");

    std::fs::write(
        work_dir.join("pm3.toml"),
        r#"
[web]
command = "sleep 999"

[worker]
command = "sleep 999"
"#,
    )
    .unwrap();

    // Start all processes
    pm3(&data_dir, work_dir).arg("start").assert().success();

    // Record PIDs
    let processes = get_process_list(&data_dir, work_dir);
    let web_pid_before = find_process_pid(&processes, "web");
    let worker_pid_before = find_process_pid(&processes, "worker");

    // Restart only web
    pm3(&data_dir, work_dir)
        .args(["restart", "web"])
        .assert()
        .success()
        .stdout(predicate::str::contains("restarted: web"));

    // Verify: web has new PID, worker unchanged, both online
    let processes = get_process_list(&data_dir, work_dir);
    let web_pid_after = find_process_pid(&processes, "web");
    let worker_pid_after = find_process_pid(&processes, "worker");

    assert_ne!(
        web_pid_before, web_pid_after,
        "web PID should change after restart"
    );
    assert_eq!(
        worker_pid_before, worker_pid_after,
        "worker PID should not change"
    );

    let web = processes.iter().find(|p| p.name == "web").unwrap();
    assert_eq!(web.status, ProcessStatus::Online, "web should be online");
    let worker = processes.iter().find(|p| p.name == "worker").unwrap();
    assert_eq!(
        worker.status,
        ProcessStatus::Online,
        "worker should be online"
    );

    kill_daemon(&data_dir, work_dir);
}

#[test]
fn test_e2e_restart_all_processes_get_new_pids() {
    let dir = TempDir::new().unwrap();
    let work_dir = dir.path();
    let data_dir = dir.path().join("data");

    std::fs::write(
        work_dir.join("pm3.toml"),
        r#"
[web]
command = "sleep 999"

[worker]
command = "sleep 999"
"#,
    )
    .unwrap();

    // Start all processes
    pm3(&data_dir, work_dir).arg("start").assert().success();

    // Record PIDs
    let processes = get_process_list(&data_dir, work_dir);
    let web_pid_before = find_process_pid(&processes, "web");
    let worker_pid_before = find_process_pid(&processes, "worker");

    // Restart all (no args)
    pm3(&data_dir, work_dir)
        .arg("restart")
        .assert()
        .success()
        .stdout(predicate::str::contains("restarted:"));

    // Verify: both have new PIDs, both online
    let processes = get_process_list(&data_dir, work_dir);
    let web_pid_after = find_process_pid(&processes, "web");
    let worker_pid_after = find_process_pid(&processes, "worker");

    assert_ne!(
        web_pid_before, web_pid_after,
        "web PID should change after restart"
    );
    assert_ne!(
        worker_pid_before, worker_pid_after,
        "worker PID should change after restart"
    );

    let web = processes.iter().find(|p| p.name == "web").unwrap();
    assert_eq!(web.status, ProcessStatus::Online, "web should be online");
    let worker = processes.iter().find(|p| p.name == "worker").unwrap();
    assert_eq!(
        worker.status,
        ProcessStatus::Online,
        "worker should be online"
    );

    kill_daemon(&data_dir, work_dir);
}

// ── Step 13: Kill command ───────────────────────────────────────────

#[test]
fn test_e2e_kill_stops_processes_and_cleans_up() {
    let dir = TempDir::new().unwrap();
    let work_dir = dir.path();
    let data_dir = dir.path().join("data");

    std::fs::write(
        work_dir.join("pm3.toml"),
        r#"
[web]
command = "sleep 999"

[worker]
command = "sleep 999"
"#,
    )
    .unwrap();

    // Start both processes
    pm3(&data_dir, work_dir).arg("start").assert().success();

    // Verify socket and PID file exist
    assert!(
        data_dir.join("pm3.sock").exists(),
        "pm3.sock should exist after start"
    );
    assert!(
        data_dir.join("pm3.pid").exists(),
        "pm3.pid should exist after start"
    );

    // Kill the daemon
    pm3(&data_dir, work_dir)
        .arg("kill")
        .assert()
        .success()
        .stdout(predicate::str::contains("daemon shutting down"));

    // Wait for async cleanup
    std::thread::sleep(Duration::from_millis(500));

    // Socket and PID file should be cleaned up
    assert!(
        !data_dir.join("pm3.sock").exists(),
        "pm3.sock should be removed after kill"
    );
    assert!(
        !data_dir.join("pm3.pid").exists(),
        "pm3.pid should be removed after kill"
    );
}

#[test]
fn test_e2e_kill_then_list_auto_starts_fresh_daemon() {
    let dir = TempDir::new().unwrap();
    let work_dir = dir.path();
    let data_dir = dir.path().join("data");

    std::fs::write(
        work_dir.join("pm3.toml"),
        r#"
[web]
command = "sleep 999"
"#,
    )
    .unwrap();

    // Start a process and verify it appears
    pm3(&data_dir, work_dir).arg("start").assert().success();
    let processes = get_process_list(&data_dir, work_dir);
    assert!(
        processes.iter().any(|p| p.name == "web"),
        "web should appear in list before kill"
    );

    // Kill the daemon
    pm3(&data_dir, work_dir).arg("kill").assert().success();
    std::thread::sleep(Duration::from_millis(500));

    // List should auto-start a fresh daemon with no processes
    let processes = get_process_list(&data_dir, work_dir);
    assert!(
        processes.is_empty(),
        "process list should be empty after kill"
    );

    // Fresh daemon should have recreated socket and PID file
    assert!(
        data_dir.join("pm3.sock").exists(),
        "pm3.sock should be re-created by fresh daemon"
    );
    assert!(
        data_dir.join("pm3.pid").exists(),
        "pm3.pid should be re-created by fresh daemon"
    );

    kill_daemon(&data_dir, work_dir);
}

// ── Step 8: Start command ───────────────────────────────────────────

#[test]
fn test_e2e_start_one_process_running() {
    let dir = TempDir::new().unwrap();
    let work_dir = dir.path();
    let data_dir = dir.path().join("data");

    std::fs::write(
        work_dir.join("pm3.toml"),
        r#"
[web]
command = "sleep 999"
"#,
    )
    .unwrap();

    // Start should report the process
    pm3(&data_dir, work_dir)
        .arg("start")
        .assert()
        .success()
        .stdout(predicate::str::contains("started: web"));

    // List should show web online
    let processes = get_process_list(&data_dir, work_dir);
    let web = processes
        .iter()
        .find(|p| p.name == "web")
        .expect("web should appear in list");
    assert_eq!(web.status, ProcessStatus::Online, "web should be online");

    kill_daemon(&data_dir, work_dir);
}

#[test]
fn test_e2e_start_two_processes_running() {
    let dir = TempDir::new().unwrap();
    let work_dir = dir.path();
    let data_dir = dir.path().join("data");

    std::fs::write(
        work_dir.join("pm3.toml"),
        r#"
[web]
command = "sleep 999"

[worker]
command = "sleep 999"
"#,
    )
    .unwrap();

    pm3(&data_dir, work_dir).arg("start").assert().success();

    // List should show both processes online
    let processes = get_process_list(&data_dir, work_dir);

    let web = processes
        .iter()
        .find(|p| p.name == "web")
        .expect("web should appear in list");
    assert_eq!(web.status, ProcessStatus::Online, "web should be online");

    let worker = processes
        .iter()
        .find(|p| p.name == "worker")
        .expect("worker should appear in list");
    assert_eq!(
        worker.status,
        ProcessStatus::Online,
        "worker should be online"
    );

    kill_daemon(&data_dir, work_dir);
}

#[test]
fn test_e2e_start_named_process_only() {
    let dir = TempDir::new().unwrap();
    let work_dir = dir.path();
    let data_dir = dir.path().join("data");

    std::fs::write(
        work_dir.join("pm3.toml"),
        r#"
[web]
command = "sleep 999"

[worker]
command = "sleep 999"
"#,
    )
    .unwrap();

    // Start only web
    pm3(&data_dir, work_dir)
        .args(["start", "web"])
        .assert()
        .success()
        .stdout(predicate::str::contains("started: web"));

    // List should show web but not worker
    let processes = get_process_list(&data_dir, work_dir);
    assert!(
        processes.iter().any(|p| p.name == "web"),
        "web should appear in list"
    );
    assert!(
        !processes.iter().any(|p| p.name == "worker"),
        "worker should NOT appear in list"
    );

    kill_daemon(&data_dir, work_dir);
}

#[test]
fn test_e2e_start_no_config_file_errors() {
    let dir = TempDir::new().unwrap();
    let work_dir = dir.path();
    let data_dir = dir.path().join("data");

    // No pm3.toml exists — start should fail client-side
    pm3(&data_dir, work_dir).arg("start").assert().failure();
}

#[test]
fn test_e2e_start_nonexistent_name_errors() {
    let dir = TempDir::new().unwrap();
    let work_dir = dir.path();
    let data_dir = dir.path().join("data");

    std::fs::write(
        work_dir.join("pm3.toml"),
        r#"
[web]
command = "sleep 999"
"#,
    )
    .unwrap();

    // Start a nonexistent process name (use --json to get structured error)
    let output = pm3(&data_dir, work_dir)
        .args(["--json", "start", "nonexistent"])
        .output()
        .unwrap();
    let response = parse_json_response(&output);
    match response {
        Response::Error { message } => {
            assert!(
                message.contains("not found"),
                "error should contain 'not found', got: {message}"
            );
        }
        other => panic!("expected Error response, got: {other:?}"),
    }

    kill_daemon(&data_dir, work_dir);
}

// ── Step 9: List command ────────────────────────────────────────────

#[test]
fn test_e2e_list_shows_process_details() {
    let dir = TempDir::new().unwrap();
    let work_dir = dir.path();
    let data_dir = dir.path().join("data");

    std::fs::write(
        work_dir.join("pm3.toml"),
        r#"
[web]
command = "sleep 999"

[worker]
command = "sleep 999"
"#,
    )
    .unwrap();

    pm3(&data_dir, work_dir).arg("start").assert().success();

    let processes = get_process_list(&data_dir, work_dir);

    // Both processes should be listed with PIDs and online status
    for name in &["web", "worker"] {
        let p = processes
            .iter()
            .find(|p| p.name == *name)
            .unwrap_or_else(|| panic!("{name} should appear in list"));
        assert_eq!(p.status, ProcessStatus::Online, "{name} should be online");
        assert!(p.pid.is_some(), "{name} should have a PID");
    }

    kill_daemon(&data_dir, work_dir);
}

#[test]
fn test_e2e_list_no_processes_shows_message() {
    let dir = TempDir::new().unwrap();
    let work_dir = dir.path();
    let data_dir = dir.path().join("data");

    // No pm3.toml needed — list auto-starts the daemon
    let processes = get_process_list(&data_dir, work_dir);
    assert!(
        processes.is_empty(),
        "process list should be empty when no processes running"
    );

    kill_daemon(&data_dir, work_dir);
}

// ── Full lifecycle ──────────────────────────────────────────────────

#[test]
fn test_e2e_full_lifecycle() {
    let dir = TempDir::new().unwrap();
    let work_dir = dir.path();
    let data_dir = dir.path().join("data");

    std::fs::write(
        work_dir.join("pm3.toml"),
        r#"
[web]
command = "sleep 999"

[worker]
command = "sleep 999"
"#,
    )
    .unwrap();

    // 1. Start → 2 processes
    pm3(&data_dir, work_dir)
        .arg("start")
        .assert()
        .success()
        .stdout(predicate::str::contains("started:"));

    // 2. List → both online, record PIDs
    let processes = get_process_list(&data_dir, work_dir);
    let web_pid_before = find_process_pid(&processes, "web");
    let worker_pid_before = find_process_pid(&processes, "worker");
    for p in &processes {
        assert_eq!(
            p.status,
            ProcessStatus::Online,
            "{} should be online",
            p.name
        );
    }

    // 3. Restart web → new PID, worker unchanged
    pm3(&data_dir, work_dir)
        .args(["restart", "web"])
        .assert()
        .success()
        .stdout(predicate::str::contains("restarted: web"));

    let processes = get_process_list(&data_dir, work_dir);
    let web_pid_after = find_process_pid(&processes, "web");
    let worker_pid_after = find_process_pid(&processes, "worker");
    assert_ne!(
        web_pid_before, web_pid_after,
        "web PID should change after restart"
    );
    assert_eq!(
        worker_pid_before, worker_pid_after,
        "worker PID should not change after web restart"
    );

    // 4. Stop worker → worker stopped, web still online
    pm3(&data_dir, work_dir)
        .args(["stop", "worker"])
        .assert()
        .success()
        .stdout(predicate::str::contains("stopped: worker"));

    let processes = get_process_list(&data_dir, work_dir);
    let web = processes.iter().find(|p| p.name == "web").unwrap();
    assert_eq!(
        web.status,
        ProcessStatus::Online,
        "web should still be online after stopping worker"
    );
    let worker = processes.iter().find(|p| p.name == "worker").unwrap();
    assert_eq!(
        worker.status,
        ProcessStatus::Stopped,
        "worker should be stopped"
    );

    // 5. Kill → daemon shuts down, files cleaned up
    pm3(&data_dir, work_dir)
        .arg("kill")
        .assert()
        .success()
        .stdout(predicate::str::contains("daemon shutting down"));
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        !data_dir.join("pm3.sock").exists(),
        "socket should be removed after kill"
    );
    assert!(
        !data_dir.join("pm3.pid").exists(),
        "PID file should be removed after kill"
    );

    // 6. List → auto-starts fresh daemon, no processes
    let processes = get_process_list(&data_dir, work_dir);
    assert!(
        processes.is_empty(),
        "process list should be empty after kill"
    );

    kill_daemon(&data_dir, work_dir);
}

// ── Item 14: Log command ────────────────────────────────────────────

#[test]
fn test_e2e_log_shows_output() {
    let dir = TempDir::new().unwrap();
    let work_dir = dir.path();
    let data_dir = dir.path().join("data");

    std::fs::write(
        work_dir.join("pm3.toml"),
        r#"
[echoer]
command = "sh -c 'echo hello_from_echoer'"
"#,
    )
    .unwrap();

    pm3(&data_dir, work_dir).arg("start").assert().success();
    std::thread::sleep(Duration::from_millis(500));

    pm3(&data_dir, work_dir)
        .args(["log", "echoer"])
        .assert()
        .success()
        .stdout(predicate::str::contains("hello_from_echoer"));

    kill_daemon(&data_dir, work_dir);
}

#[test]
fn test_e2e_log_lines_param() {
    let dir = TempDir::new().unwrap();
    let work_dir = dir.path();
    let data_dir = dir.path().join("data");

    std::fs::write(
        work_dir.join("pm3.toml"),
        r#"
[counter]
command = "sh -c 'for i in 1 2 3 4 5 6 7 8 9 10; do echo line$i; done'"
"#,
    )
    .unwrap();

    pm3(&data_dir, work_dir).arg("start").assert().success();
    std::thread::sleep(Duration::from_millis(500));

    let output = pm3(&data_dir, work_dir)
        .args(["log", "counter", "--lines", "5"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();

    assert_eq!(
        lines.len(),
        5,
        "should show exactly 5 lines, got: {lines:?}"
    );

    kill_daemon(&data_dir, work_dir);
}

#[test]
fn test_e2e_log_no_name_shows_interleaved() {
    let dir = TempDir::new().unwrap();
    let work_dir = dir.path();
    let data_dir = dir.path().join("data");

    std::fs::write(
        work_dir.join("pm3.toml"),
        r#"
[alpha]
command = "sh -c 'echo alpha_line'"

[beta]
command = "sh -c 'echo beta_line'"
"#,
    )
    .unwrap();

    pm3(&data_dir, work_dir).arg("start").assert().success();
    std::thread::sleep(Duration::from_millis(500));

    let output = pm3(&data_dir, work_dir).arg("log").output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        stdout.contains("alpha_line"),
        "should contain alpha_line, got: {stdout}"
    );
    assert!(
        stdout.contains("beta_line"),
        "should contain beta_line, got: {stdout}"
    );

    kill_daemon(&data_dir, work_dir);
}

// ── Item 15: Flush command ──────────────────────────────────────────

#[test]
fn test_e2e_flush_named_process() {
    let dir = TempDir::new().unwrap();
    let work_dir = dir.path();
    let data_dir = dir.path().join("data");

    std::fs::write(
        work_dir.join("pm3.toml"),
        r#"
[web]
command = "sh -c 'echo web_flush_output'"
"#,
    )
    .unwrap();

    pm3(&data_dir, work_dir).arg("start").assert().success();
    std::thread::sleep(Duration::from_millis(500));

    // Verify log file has content
    let stdout_log = data_dir.join("logs").join("web-out.log");
    assert!(stdout_log.exists(), "stdout log should exist");
    assert!(
        !std::fs::read_to_string(&stdout_log).unwrap().is_empty(),
        "stdout log should have content before flush"
    );

    // Flush by name
    pm3(&data_dir, work_dir)
        .args(["flush", "web"])
        .assert()
        .success()
        .stdout(predicate::str::contains("flushed"));

    // Verify log file is empty
    assert!(stdout_log.exists(), "stdout log should still exist");
    assert!(
        std::fs::read_to_string(&stdout_log).unwrap().is_empty(),
        "stdout log should be empty after flush"
    );

    kill_daemon(&data_dir, work_dir);
}

#[test]
fn test_e2e_flush_all() {
    let dir = TempDir::new().unwrap();
    let work_dir = dir.path();
    let data_dir = dir.path().join("data");

    std::fs::write(
        work_dir.join("pm3.toml"),
        r#"
[web]
command = "sh -c 'echo web_output'"

[worker]
command = "sh -c 'echo worker_output'"
"#,
    )
    .unwrap();

    pm3(&data_dir, work_dir).arg("start").assert().success();
    std::thread::sleep(Duration::from_millis(500));

    // Verify log files have content
    for name in &["web", "worker"] {
        let stdout_log = data_dir.join("logs").join(format!("{name}-out.log"));
        assert!(stdout_log.exists(), "{name} stdout log should exist");
        assert!(
            !std::fs::read_to_string(&stdout_log).unwrap().is_empty(),
            "{name} stdout log should have content before flush"
        );
    }

    // Flush all (no name argument)
    pm3(&data_dir, work_dir)
        .arg("flush")
        .assert()
        .success()
        .stdout(predicate::str::contains("flushed"));

    // Verify all log files are empty
    for name in &["web", "worker"] {
        let stdout_log = data_dir.join("logs").join(format!("{name}-out.log"));
        assert!(
            stdout_log.exists(),
            "{name} stdout log should still exist after flush"
        );
        assert!(
            std::fs::read_to_string(&stdout_log).unwrap().is_empty(),
            "{name} stdout log should be empty after flush"
        );
    }

    kill_daemon(&data_dir, work_dir);
}

// ── Step 31: Reload command ──────────────────────────────────────────

#[test]
fn test_e2e_reload_without_health_check_falls_back() {
    let dir = TempDir::new().unwrap();
    let work_dir = dir.path();
    let data_dir = dir.path().join("data");

    std::fs::write(
        work_dir.join("pm3.toml"),
        r#"
[web]
command = "sleep 999"
"#,
    )
    .unwrap();

    pm3(&data_dir, work_dir).arg("start").assert().success();

    let processes = get_process_list(&data_dir, work_dir);
    let pid_before = find_process_pid(&processes, "web");

    // Reload should fall back to restart for processes without health check
    pm3(&data_dir, work_dir)
        .args(["reload", "web"])
        .assert()
        .success()
        .stdout(predicate::str::contains("reloaded:"));

    let processes = get_process_list(&data_dir, work_dir);
    let pid_after = find_process_pid(&processes, "web");
    assert_ne!(pid_before, pid_after, "PID should change after reload");

    let web = processes.iter().find(|p| p.name == "web").unwrap();
    assert_eq!(
        web.status,
        ProcessStatus::Online,
        "web should be online after reload"
    );

    kill_daemon(&data_dir, work_dir);
}

#[test]
fn test_e2e_reload_nonexistent_errors() {
    let dir = TempDir::new().unwrap();
    let work_dir = dir.path();
    let data_dir = dir.path().join("data");

    std::fs::write(
        work_dir.join("pm3.toml"),
        r#"
[web]
command = "sleep 999"
"#,
    )
    .unwrap();

    pm3(&data_dir, work_dir).arg("start").assert().success();

    let output = pm3(&data_dir, work_dir)
        .args(["--json", "reload", "nonexistent"])
        .output()
        .unwrap();
    let response = parse_json_response(&output);
    match response {
        Response::Error { message } => {
            assert!(
                message.contains("not found"),
                "error should contain 'not found', got: {message}"
            );
        }
        other => panic!("expected Error response, got: {other:?}"),
    }

    kill_daemon(&data_dir, work_dir);
}

#[test]
fn test_e2e_reload_pid_changes() {
    let dir = TempDir::new().unwrap();
    let work_dir = dir.path();
    let data_dir = dir.path().join("data");

    std::fs::write(
        work_dir.join("pm3.toml"),
        r#"
[web]
command = "sleep 999"

[worker]
command = "sleep 999"
"#,
    )
    .unwrap();

    pm3(&data_dir, work_dir).arg("start").assert().success();

    let processes = get_process_list(&data_dir, work_dir);
    let web_pid_before = find_process_pid(&processes, "web");
    let worker_pid_before = find_process_pid(&processes, "worker");

    // Reload only web
    pm3(&data_dir, work_dir)
        .args(["reload", "web"])
        .assert()
        .success()
        .stdout(predicate::str::contains("reloaded:"));

    let processes = get_process_list(&data_dir, work_dir);
    let web_pid_after = find_process_pid(&processes, "web");
    let worker_pid_after = find_process_pid(&processes, "worker");

    assert_ne!(
        web_pid_before, web_pid_after,
        "web PID should change after reload"
    );
    assert_eq!(
        worker_pid_before, worker_pid_after,
        "worker PID should not change"
    );

    let web = processes.iter().find(|p| p.name == "web").unwrap();
    assert_eq!(web.status, ProcessStatus::Online, "web should be online");
    let worker = processes.iter().find(|p| p.name == "worker").unwrap();
    assert_eq!(
        worker.status,
        ProcessStatus::Online,
        "worker should be online"
    );

    kill_daemon(&data_dir, work_dir);
}

#[test]
fn test_e2e_reload_with_health_check() {
    let dir = TempDir::new().unwrap();
    let work_dir = dir.path();
    let data_dir = dir.path().join("data");

    // Use a node server that listens on an ephemeral port and writes it to a file,
    // but the health check uses a fixed port. We launch a background socat/nc to
    // handle the health check port separately. Instead, use a simpler approach:
    // the process itself opens a TCP listener, and we use a unique port per test run
    // to avoid conflicts. The key insight: during reload, the new process starts
    // while old is running, so we need a process whose health check doesn't conflict.
    //
    // Solution: use a process that starts a background TCP listener that isn't
    // the main command. We write a shell script that starts nc in background.

    // Create a script that listens on a port using node
    std::fs::write(
        work_dir.join("server.js"),
        r#"
const http = require('http');
const server = http.createServer((req, res) => {
  res.writeHead(200);
  res.end('ok');
});
// Use exclusive:false to allow multiple binds (REUSEADDR)
server.listen({port: 18935, host: '127.0.0.1', exclusive: false}, () => {
  console.log('listening');
});
server.on('error', (e) => {
  // If port is taken, retry after a delay (for reload overlap)
  if (e.code === 'EADDRINUSE') {
    setTimeout(() => {
      server.listen({port: 18935, host: '127.0.0.1', exclusive: false}, () => {
        console.log('listening on retry');
      });
    }, 1000);
  }
});
"#,
    )
    .unwrap();

    std::fs::write(
        work_dir.join("pm3.toml"),
        r#"
[server]
command = "node server.js"
health_check = "tcp://127.0.0.1:18935"
"#,
    )
    .unwrap();

    pm3(&data_dir, work_dir).arg("start").assert().success();

    // Wait for health check to pass
    wait_until_online(&data_dir, work_dir, "server", 15);

    let processes = get_process_list(&data_dir, work_dir);
    let pid_before = find_process_pid(&processes, "server");

    // Reload with health check
    pm3(&data_dir, work_dir)
        .args(["reload", "server"])
        .timeout(Duration::from_secs(60))
        .assert()
        .success()
        .stdout(predicate::str::contains("reloaded:"));

    // Wait for the reloaded process to come online
    wait_until_online(&data_dir, work_dir, "server", 15);

    let processes = get_process_list(&data_dir, work_dir);
    let pid_after = find_process_pid(&processes, "server");
    assert_ne!(pid_before, pid_after, "PID should change after reload");

    let server = processes.iter().find(|p| p.name == "server").unwrap();
    assert_eq!(
        server.status,
        ProcessStatus::Online,
        "server should be online after reload"
    );

    kill_daemon(&data_dir, work_dir);
}

// ── Step 25: Info command ───────────────────────────────────────────

#[test]
fn test_e2e_info_prints_detail() {
    let dir = TempDir::new().unwrap();
    let work_dir = dir.path();
    let data_dir = dir.path().join("data");

    std::fs::write(
        work_dir.join("pm3.toml"),
        r#"
[web]
command = "sleep 999"
cwd = "/tmp"
group = "backend"
"#,
    )
    .unwrap();

    pm3(&data_dir, work_dir).arg("start").assert().success();

    // Use --json to get structured response
    let output = pm3(&data_dir, work_dir)
        .args(["--json", "info", "web"])
        .output()
        .unwrap();
    let response = parse_json_response(&output);
    match response {
        Response::ProcessDetail { info } => {
            assert_eq!(info.name, "web");
            assert_eq!(info.status, ProcessStatus::Online);
            assert!(info.pid.is_some(), "should have a PID");
            assert_eq!(info.command, "sleep 999");
            assert!(info.stdout_log.is_some(), "should have stdout log path");
            assert!(info.stderr_log.is_some(), "should have stderr log path");
        }
        other => panic!("expected ProcessDetail, got: {other:?}"),
    }

    // Also verify human-readable output contains key fields
    pm3(&data_dir, work_dir)
        .args(["info", "web"])
        .assert()
        .success()
        .stdout(predicate::str::contains("web"))
        .stdout(predicate::str::contains("sleep 999"))
        .stdout(predicate::str::contains("pid:"))
        .stdout(predicate::str::contains("stdout_log:"))
        .stdout(predicate::str::contains("stderr_log:"));

    kill_daemon(&data_dir, work_dir);
}

#[test]
fn test_e2e_info_nonexistent_errors() {
    let dir = TempDir::new().unwrap();
    let work_dir = dir.path();
    let data_dir = dir.path().join("data");

    std::fs::write(
        work_dir.join("pm3.toml"),
        r#"
[web]
command = "sleep 999"
"#,
    )
    .unwrap();

    pm3(&data_dir, work_dir).arg("start").assert().success();

    // Use --json to get structured error
    let output = pm3(&data_dir, work_dir)
        .args(["--json", "info", "nonexistent"])
        .output()
        .unwrap();
    let response = parse_json_response(&output);
    match response {
        Response::Error { message } => {
            assert!(
                message.contains("not found"),
                "error should contain 'not found', got: {message}"
            );
        }
        other => panic!("expected Error response, got: {other:?}"),
    }

    kill_daemon(&data_dir, work_dir);
}

// ---------------------------------------------------------------------------
// Process dependency E2E tests
// ---------------------------------------------------------------------------

#[test]
fn test_e2e_dependency_start_order() {
    let dir = TempDir::new().unwrap();
    let work_dir = dir.path();
    let data_dir = dir.path().join("data");

    std::fs::write(
        work_dir.join("pm3.toml"),
        r#"
[db]
command = "sleep 999"

[web]
command = "sleep 999"
depends_on = ["db"]
"#,
    )
    .unwrap();

    // Start all processes
    pm3(&data_dir, work_dir)
        .arg("start")
        .assert()
        .success()
        .stdout(predicate::str::contains("started:"));

    std::thread::sleep(Duration::from_millis(500));

    // Both should be online
    let processes = get_process_list(&data_dir, work_dir);
    assert_eq!(processes.len(), 2, "should have 2 processes");

    for p in &processes {
        assert_eq!(
            p.status,
            ProcessStatus::Online,
            "process '{}' should be online",
            p.name
        );
    }

    kill_daemon(&data_dir, work_dir);
}

// ---------------------------------------------------------------------------
// Process groups (step 28)
// ---------------------------------------------------------------------------

#[test]
fn test_e2e_list_shows_group_column() {
    let dir = TempDir::new().unwrap();
    let work_dir = dir.path();
    let data_dir = dir.path().join("data");

    std::fs::write(
        work_dir.join("pm3.toml"),
        r#"
[api]
command = "sleep 999"
group = "backend"

[worker]
command = "sleep 999"
group = "backend"

[frontend]
command = "sleep 999"
"#,
    )
    .unwrap();

    pm3(&data_dir, work_dir).arg("start").assert().success();

    // Human-readable list should contain "group" header and group values
    let output = pm3(&data_dir, work_dir).arg("list").output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        stdout.contains("group"),
        "list output should contain 'group' header, got: {stdout}"
    );
    assert!(
        stdout.contains("backend"),
        "list output should contain 'backend' group, got: {stdout}"
    );

    // JSON list should have group field
    let processes = get_process_list(&data_dir, work_dir);
    let api = processes.iter().find(|p| p.name == "api").unwrap();
    assert_eq!(
        api.group.as_deref(),
        Some("backend"),
        "api should have group 'backend'"
    );

    let frontend = processes.iter().find(|p| p.name == "frontend").unwrap();
    assert_eq!(frontend.group, None, "frontend should have no group");

    kill_daemon(&data_dir, work_dir);
}

// ---------------------------------------------------------------------------
// Signal command (step 29)
// ---------------------------------------------------------------------------

#[test]
fn test_e2e_signal_success() {
    let dir = TempDir::new().unwrap();
    let work_dir = dir.path();
    let data_dir = dir.path().join("data");

    std::fs::write(
        work_dir.join("pm3.toml"),
        r#"
[web]
command = "sleep 999"
"#,
    )
    .unwrap();

    pm3(&data_dir, work_dir).arg("start").assert().success();

    pm3(&data_dir, work_dir)
        .args(["signal", "web", "SIGUSR1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("sent"));

    kill_daemon(&data_dir, work_dir);
}

#[test]
fn test_e2e_signal_nonexistent_errors() {
    let dir = TempDir::new().unwrap();
    let work_dir = dir.path();
    let data_dir = dir.path().join("data");

    std::fs::write(
        work_dir.join("pm3.toml"),
        r#"
[web]
command = "sleep 999"
"#,
    )
    .unwrap();

    pm3(&data_dir, work_dir).arg("start").assert().success();

    let output = pm3(&data_dir, work_dir)
        .args(["--json", "signal", "nonexistent", "SIGHUP"])
        .output()
        .unwrap();
    let response = parse_json_response(&output);
    match response {
        Response::Error { message } => {
            assert!(
                message.contains("not found"),
                "error should contain 'not found', got: {message}"
            );
        }
        other => panic!("expected Error response, got: {other:?}"),
    }

    kill_daemon(&data_dir, work_dir);
}

// ---------------------------------------------------------------------------
// Save & resurrect E2E tests
// ---------------------------------------------------------------------------

#[test]
fn test_e2e_save_creates_snapshot_file() {
    let dir = TempDir::new().unwrap();
    let work_dir = dir.path();
    let data_dir = dir.path().join("data");

    std::fs::write(
        work_dir.join("pm3.toml"),
        r#"
[web]
command = "sleep 999"

[worker]
command = "sleep 888"
"#,
    )
    .unwrap();

    pm3(&data_dir, work_dir).arg("start").assert().success();
    std::thread::sleep(Duration::from_millis(500));

    // Save
    pm3(&data_dir, work_dir)
        .arg("save")
        .assert()
        .success()
        .stdout(predicate::str::contains("saved"));

    // Verify dump.json exists and contains both processes
    let dump_path = data_dir.join("dump.json");
    assert!(dump_path.exists(), "dump.json should exist after save");

    let data = std::fs::read_to_string(&dump_path).unwrap();
    let entries: serde_json::Value = serde_json::from_str(&data).unwrap();
    let arr = entries.as_array().unwrap();
    assert_eq!(arr.len(), 2, "dump should contain 2 processes");

    let names: Vec<&str> = arr.iter().map(|e| e["name"].as_str().unwrap()).collect();
    assert!(names.contains(&"web"), "dump should contain 'web'");
    assert!(names.contains(&"worker"), "dump should contain 'worker'");

    kill_daemon(&data_dir, work_dir);
}

#[test]
fn test_e2e_kill_then_resurrect_restores_processes() {
    let dir = TempDir::new().unwrap();
    let work_dir = dir.path();
    let data_dir = dir.path().join("data");

    std::fs::write(
        work_dir.join("pm3.toml"),
        r#"
[alpha]
command = "sleep 999"

[beta]
command = "sleep 888"
"#,
    )
    .unwrap();

    // Start and save
    pm3(&data_dir, work_dir).arg("start").assert().success();
    std::thread::sleep(Duration::from_millis(500));

    pm3(&data_dir, work_dir).arg("save").assert().success();

    // Kill daemon
    kill_daemon(&data_dir, work_dir);

    // Resurrect — this auto-starts the daemon and restores processes
    pm3(&data_dir, work_dir)
        .arg("resurrect")
        .assert()
        .success()
        .stdout(predicate::str::contains("resurrected"));

    std::thread::sleep(Duration::from_millis(500));

    // Verify both processes are running
    let processes = get_process_list(&data_dir, work_dir);
    assert_eq!(
        processes.len(),
        2,
        "should have 2 processes after resurrect"
    );
    for p in &processes {
        assert_eq!(
            p.status,
            ProcessStatus::Online,
            "'{}' should be online after resurrect",
            p.name
        );
        assert!(p.pid.is_some(), "'{}' should have a PID", p.name);
    }

    kill_daemon(&data_dir, work_dir);
}

#[test]
fn test_e2e_resurrect_stores_absolute_paths() {
    let dir = TempDir::new().unwrap();
    let work_dir = dir.path();
    let data_dir = dir.path().join("data");
    let cwd_dir = dir.path().join("myapp");
    std::fs::create_dir_all(&cwd_dir).unwrap();

    std::fs::write(
        work_dir.join("pm3.toml"),
        format!(
            r#"
[app]
command = "sleep 999"
cwd = "{}"
"#,
            cwd_dir.display()
        ),
    )
    .unwrap();

    // Start and save
    pm3(&data_dir, work_dir).arg("start").assert().success();
    std::thread::sleep(Duration::from_millis(500));
    pm3(&data_dir, work_dir).arg("save").assert().success();

    // Verify the dump file contains the absolute cwd path
    let dump_path = data_dir.join("dump.json");
    let data = std::fs::read_to_string(&dump_path).unwrap();
    assert!(
        data.contains(&cwd_dir.to_string_lossy().to_string()),
        "dump should contain absolute cwd path"
    );

    // Kill daemon
    kill_daemon(&data_dir, work_dir);

    // Resurrect from a DIFFERENT working directory
    let other_dir = dir.path().join("other");
    std::fs::create_dir_all(&other_dir).unwrap();

    // We need a pm3.toml in the new dir for the CLI, but resurrect reads from dump
    std::fs::write(
        other_dir.join("pm3.toml"),
        "[placeholder]\ncommand = \"true\"\n",
    )
    .unwrap();

    pm3(&data_dir, &other_dir)
        .arg("resurrect")
        .assert()
        .success()
        .stdout(predicate::str::contains("resurrected"));

    std::thread::sleep(Duration::from_millis(500));

    // Verify process is running with original config
    let processes = get_process_list(&data_dir, &other_dir);
    let app = processes.iter().find(|p| p.name == "app");
    assert!(app.is_some(), "app should be restored");
    assert_eq!(app.unwrap().status, ProcessStatus::Online);

    kill_daemon(&data_dir, &other_dir);
}

// ---------------------------------------------------------------------------
// Immediate exit detection
// ---------------------------------------------------------------------------

#[test]
fn test_e2e_start_immediate_exit_reports_error() {
    let dir = TempDir::new().unwrap();
    let work_dir = dir.path();
    let data_dir = dir.path().join("data");

    std::fs::write(
        work_dir.join("pm3.toml"),
        r#"
[failing]
command = "sh -c 'exit 1'"
"#,
    )
    .unwrap();

    let output = pm3(&data_dir, work_dir)
        .args(["--json", "start"])
        .output()
        .unwrap();
    let response = parse_json_response(&output);
    match response {
        Response::Error { message } => {
            assert!(
                message.contains("exited immediately"),
                "error should contain 'exited immediately', got: {message}"
            );
        }
        other => panic!("expected Error response for immediate exit, got: {other:?}"),
    }

    kill_daemon(&data_dir, work_dir);
}

// ---------------------------------------------------------------------------
// Init command E2E tests
// ---------------------------------------------------------------------------

fn pm3_init(work_dir: &Path) -> Command {
    let mut cmd: Command = cargo_bin_cmd!("pm3");
    cmd.current_dir(work_dir);
    cmd.timeout(Duration::from_secs(10));
    cmd
}

fn pm3_standalone() -> Command {
    let mut cmd: Command = cargo_bin_cmd!("pm3");
    cmd.timeout(Duration::from_secs(10));
    cmd
}

#[test]
fn test_e2e_init_generates_valid_toml() {
    let dir = TempDir::new().unwrap();
    let work_dir = dir.path();

    // Pipe answers: name, command, cwd (empty), env (empty), restart (default), health_check (empty), group (empty), add another (n)
    let stdin = "web\nnode server.js\n\n\non_failure\n\n\nn\n";

    pm3_init(work_dir)
        .arg("init")
        .write_stdin(stdin)
        .assert()
        .success()
        .stdout(predicate::str::contains("Created"));

    let config_path = work_dir.join("pm3.toml");
    assert!(config_path.exists(), "pm3.toml should be created");

    let content = std::fs::read_to_string(&config_path).unwrap();
    let configs = config::parse_config(&content).expect("generated TOML should be valid");
    assert!(configs.contains_key("web"), "should contain 'web' process");
    assert_eq!(configs["web"].command, "node server.js");
}

#[test]
fn test_e2e_init_warns_on_existing_toml() {
    let dir = TempDir::new().unwrap();
    let work_dir = dir.path();

    let config_path = work_dir.join("pm3.toml");
    let original = "[existing]\ncommand = \"sleep 1\"\n";
    std::fs::write(&config_path, original).unwrap();

    // Answer "n" to overwrite prompt
    pm3_init(work_dir)
        .arg("init")
        .write_stdin("n\n")
        .assert()
        .failure();

    // File should be unchanged
    let content = std::fs::read_to_string(&config_path).unwrap();
    assert_eq!(
        content, original,
        "file should be unchanged after declining overwrite"
    );
}

#[test]
fn test_e2e_init_multiple_processes() {
    let dir = TempDir::new().unwrap();
    let work_dir = dir.path();

    // First process: web, then "y" to add another, second process: worker, then "n"
    let stdin = [
        "web",            // name
        "node server.js", // command
        "",               // cwd (skip)
        "",               // env (skip)
        "on_failure",     // restart
        "",               // health_check (skip)
        "",               // group (skip)
        // no deps prompt (no existing processes)
        "y",                // add another
        "worker",           // name
        "python worker.py", // command
        "",                 // cwd (skip)
        "",                 // env (skip)
        "always",           // restart
        "",                 // health_check (skip)
        "",                 // group (skip)
        "web",              // dependencies
        "n",                // add another
    ]
    .join("\n")
        + "\n";

    pm3_init(work_dir)
        .arg("init")
        .write_stdin(stdin)
        .assert()
        .success()
        .stdout(predicate::str::contains("Created"));

    let config_path = work_dir.join("pm3.toml");
    let content = std::fs::read_to_string(&config_path).unwrap();
    let configs = config::parse_config(&content).expect("generated TOML should be valid");
    assert_eq!(configs.len(), 2);
    assert!(configs.contains_key("web"));
    assert!(configs.contains_key("worker"));
    assert_eq!(configs["worker"].depends_on, Some(vec!["web".to_string()]));
}

// ---------------------------------------------------------------------------
// Startup / Unstartup E2E tests
// ---------------------------------------------------------------------------

#[cfg(target_os = "macos")]
fn expected_service_path() -> std::path::PathBuf {
    dirs::home_dir()
        .unwrap()
        .join("Library/LaunchAgents/com.pm3.daemon.plist")
}

#[cfg(target_os = "linux")]
fn expected_service_path() -> std::path::PathBuf {
    dirs::home_dir()
        .unwrap()
        .join(".config/systemd/user/pm3.service")
}

#[test]
fn test_e2e_startup_creates_service_file() {
    let path = expected_service_path();

    // Clean up from any prior runs
    let _ = std::fs::remove_file(&path);

    pm3_standalone()
        .arg("startup")
        .assert()
        .success()
        .stderr(predicate::str::contains("Service installed"));

    assert!(
        path.exists(),
        "service file should exist at {}",
        path.display()
    );

    let content = std::fs::read_to_string(&path).unwrap();
    assert!(
        content.contains("--daemon"),
        "service file should contain --daemon arg"
    );

    #[cfg(target_os = "macos")]
    {
        assert!(
            content.contains("com.pm3.daemon"),
            "plist should contain label"
        );
        assert!(
            content.contains("RunAtLoad"),
            "plist should contain RunAtLoad"
        );
    }
    #[cfg(target_os = "linux")]
    {
        assert!(
            content.contains("ExecStart="),
            "unit should contain ExecStart"
        );
        assert!(
            content.contains("[Service]"),
            "unit should contain [Service] section"
        );
    }

    // Clean up
    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_e2e_unstartup_removes_service_file() {
    let path = expected_service_path();

    // Ensure the file exists first
    let _ = std::fs::remove_file(&path);
    pm3_standalone().arg("startup").assert().success();
    assert!(path.exists(), "service file should exist before unstartup");

    pm3_standalone()
        .arg("unstartup")
        .assert()
        .success()
        .stderr(predicate::str::contains("Service removed"));

    assert!(
        !path.exists(),
        "service file should be removed after unstartup"
    );
}

#[test]
fn test_e2e_unstartup_no_file_prints_message() {
    let path = expected_service_path();

    // Make sure no service file exists
    let _ = std::fs::remove_file(&path);

    pm3_standalone()
        .arg("unstartup")
        .assert()
        .success()
        .stderr(predicate::str::contains("No service file found"));
}

#[test]
fn test_e2e_startup_idempotent() {
    let path = expected_service_path();
    let _ = std::fs::remove_file(&path);

    // Run startup twice — second should overwrite without error
    pm3_standalone().arg("startup").assert().success();
    pm3_standalone().arg("startup").assert().success();

    assert!(
        path.exists(),
        "service file should exist after double startup"
    );

    // Clean up
    let _ = std::fs::remove_file(&path);
}
