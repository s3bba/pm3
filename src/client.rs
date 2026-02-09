use crate::paths::Paths;
use crate::pid;
use crate::protocol::{self, Request, Response};
use crate::sys;
use color_eyre::eyre::{Context, bail};
use std::io::{BufRead, BufReader, Write};
use std::time::Duration;

pub fn send_request(paths: &Paths, request: &Request) -> color_eyre::Result<Response> {
    ensure_daemon_running(paths)?;
    let mut stream = connect_with_retry(paths, 10, Duration::from_millis(200))?;

    let encoded = protocol::encode_request(request)?;
    stream.write_all(&encoded)?;
    stream.shutdown(std::net::Shutdown::Write)?;

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line)?;

    let response = protocol::decode_response(&line)?;
    Ok(response)
}

pub fn send_request_streaming<F>(
    paths: &Paths,
    request: &Request,
    mut on_response: F,
) -> color_eyre::Result<()>
where
    F: FnMut(&Response),
{
    ensure_daemon_running(paths)?;
    let mut stream = connect_with_retry(paths, 10, Duration::from_millis(200))?;

    let encoded = protocol::encode_request(request)?;
    stream.write_all(&encoded)?;
    stream.shutdown(std::net::Shutdown::Write)?;

    let reader = BufReader::new(stream);
    for line_result in reader.lines() {
        let line = line_result?;
        if line.is_empty() {
            continue;
        }
        let response = protocol::decode_response(&line)?;
        on_response(&response);
    }

    Ok(())
}

fn ensure_daemon_running(paths: &Paths) -> color_eyre::Result<()> {
    if pid::is_daemon_running_sync(paths)? {
        return Ok(());
    }

    spawn_daemon()?;

    // Wait for IPC endpoint to appear
    for _ in 0..50 {
        if sys::ipc_exists(paths) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    bail!("timed out waiting for daemon to start");
}

fn spawn_daemon() -> color_eyre::Result<()> {
    let exe = std::env::current_exe().context("failed to get current executable path")?;

    let mut cmd = std::process::Command::new(exe);
    cmd.arg("--daemon")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    sys::configure_daemon_cmd(&mut cmd);

    cmd.spawn().context("failed to spawn daemon")?;

    Ok(())
}

fn connect_with_retry(
    paths: &Paths,
    retries: u32,
    delay: Duration,
) -> color_eyre::Result<sys::SyncIpcStream> {
    for attempt in 0..retries {
        match sys::ipc_connect(paths) {
            Ok(stream) => return Ok(stream),
            Err(e) => {
                if attempt == retries - 1 {
                    bail!("failed to connect to daemon after {retries} attempts: {e}");
                }
                std::thread::sleep(delay);
            }
        }
    }

    unreachable!()
}
