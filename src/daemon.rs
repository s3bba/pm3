use crate::config::ProcessConfig;
use crate::health;
use crate::log;
use crate::paths::Paths;
use crate::pid;
use crate::process::{self, ProcessTable};
use crate::protocol::{self, Request, Response};
use color_eyre::eyre::bail;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::fs;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::sync::RwLock;
use tokio::sync::watch;

type ChildToMonitor = (
    String,
    tokio::process::Child,
    Option<u32>,
    watch::Receiver<bool>,
    Option<String>,
    Option<watch::Receiver<bool>>,
);

pub async fn run(paths: Paths) -> color_eyre::Result<()> {
    fs::create_dir_all(paths.data_dir()).await?;

    if pid::is_daemon_running(&paths).await? {
        bail!("daemon is already running");
    }

    pid::write_pid_file(&paths).await?;

    // Remove stale socket file if it exists
    let socket_path = paths.socket_file();
    if socket_path.exists() {
        fs::remove_file(&socket_path).await?;
    }

    let listener = UnixListener::bind(&socket_path)?;

    let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
    let processes: Arc<RwLock<ProcessTable>> = Arc::new(RwLock::new(HashMap::new()));

    let result = run_accept_loop(
        &paths,
        &listener,
        &shutdown_tx,
        &mut shutdown_rx,
        &processes,
    )
    .await;

    // Gracefully stop all managed processes before cleanup
    {
        let mut table = processes.write().await;
        for (_, managed) in table.iter_mut() {
            let _ = managed.graceful_stop().await;
        }
    }

    // Cleanup
    let _ = fs::remove_file(paths.socket_file()).await;
    pid::remove_pid_file(&paths).await;

    result
}

async fn run_accept_loop(
    paths: &Paths,
    listener: &UnixListener,
    shutdown_tx: &watch::Sender<bool>,
    shutdown_rx: &mut watch::Receiver<bool>,
    processes: &Arc<RwLock<ProcessTable>>,
) -> color_eyre::Result<()> {
    loop {
        tokio::select! {
            accept_result = listener.accept() => {
                let (stream, _addr) = accept_result?;
                let tx = shutdown_tx.clone();
                let paths = paths.clone();
                let procs = Arc::clone(processes);
                tokio::spawn(async move {
                    if let Err(e) = handle_connection(stream, &tx, &procs, &paths).await {
                        eprintln!("connection error: {e}");
                    }
                });
            }
            _ = shutdown_rx.changed() => {
                if *shutdown_rx.borrow() {
                    break;
                }
            }
            _ = signal_shutdown() => {
                break;
            }
        }
    }

    Ok(())
}

async fn signal_shutdown() {
    let mut sigterm =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).unwrap();
    let mut sigint =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt()).unwrap();

    tokio::select! {
        _ = sigterm.recv() => {}
        _ = sigint.recv() => {}
    }
}

async fn handle_connection(
    stream: tokio::net::UnixStream,
    shutdown_tx: &watch::Sender<bool>,
    processes: &Arc<RwLock<ProcessTable>>,
    paths: &Paths,
) -> color_eyre::Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut buf_reader = BufReader::new(reader);
    let mut line = String::new();
    buf_reader.read_line(&mut line).await?;

    if line.is_empty() {
        return Ok(());
    }

    let request = protocol::decode_request(&line)?;

    // Log requests need streaming access to the writer
    if let Request::Log {
        ref name,
        lines,
        follow,
    } = request
    {
        handle_log(name.clone(), lines, follow, processes, paths, &mut writer).await?;
        writer.shutdown().await?;
        return Ok(());
    }

    let response = dispatch(request, shutdown_tx, processes, paths).await;
    let encoded = protocol::encode_response(&response)?;
    writer.write_all(&encoded).await?;
    writer.shutdown().await?;

    Ok(())
}

async fn dispatch(
    request: Request,
    shutdown_tx: &watch::Sender<bool>,
    processes: &Arc<RwLock<ProcessTable>>,
    paths: &Paths,
) -> Response {
    match request {
        Request::Start {
            configs,
            names,
            env,
        } => handle_start(configs, names, env, processes, paths).await,
        Request::List => {
            let table = processes.read().await;
            let infos: Vec<_> = table.values().map(|m| m.to_process_info()).collect();
            Response::ProcessList { processes: infos }
        }
        Request::Stop { names } => handle_stop(names, processes).await,
        Request::Restart { names } => handle_restart(names, processes, paths).await,
        Request::Kill => {
            let _ = shutdown_tx.send(true);
            Response::Success {
                message: Some("daemon shutting down".to_string()),
            }
        }
        Request::Info { name } => handle_info(name, processes, paths).await,
        Request::Flush { names } => handle_flush(names, processes, paths).await,
        Request::Log { .. } => {
            // Handled in handle_connection directly
            Response::Error {
                message: "unexpected dispatch for log".to_string(),
            }
        }
        _ => Response::Error {
            message: "not implemented".to_string(),
        },
    }
}

async fn handle_start(
    configs: HashMap<String, ProcessConfig>,
    names: Option<Vec<String>>,
    env: Option<String>,
    processes: &Arc<RwLock<ProcessTable>>,
    paths: &Paths,
) -> Response {
    let mut to_start: Vec<(String, ProcessConfig)> = match names {
        Some(ref requested) => {
            let mut selected = Vec::new();
            for name in requested {
                match configs.get(name) {
                    Some(config) => selected.push((name.clone(), config.clone())),
                    None => {
                        return Response::Error {
                            message: format!("process '{}' not found in configs", name),
                        };
                    }
                }
            }
            selected
        }
        None => configs.into_iter().collect(),
    };

    if let Some(ref env_name) = env {
        let any_has_env = to_start
            .iter()
            .any(|(_, config)| config.environments.contains_key(env_name));
        if !any_has_env {
            return Response::Error {
                message: format!("unknown environment: '{}'", env_name),
            };
        }
        for (_, config) in &mut to_start {
            if let Some(env_vars) = config.environments.get(env_name) {
                let base = config.env.get_or_insert_with(HashMap::new);
                for (k, v) in env_vars {
                    base.insert(k.clone(), v.clone());
                }
            }
        }
    }

    let mut started = Vec::new();
    let mut children_to_monitor: Vec<ChildToMonitor> = Vec::new();

    {
        let mut table = processes.write().await;

        for (name, config) in to_start {
            if table.contains_key(&name) {
                continue;
            }

            let health_check = config.health_check.clone();
            match process::spawn_process(name.clone(), config, paths).await {
                Ok((managed, child)) => {
                    let pid = managed.pid;
                    let shutdown_rx = managed
                        .monitor_shutdown
                        .as_ref()
                        .map(|tx| tx.subscribe())
                        .unwrap();
                    let health_shutdown_rx = health_check.as_ref().map(|_| {
                        managed
                            .monitor_shutdown
                            .as_ref()
                            .map(|tx| tx.subscribe())
                            .unwrap()
                    });
                    table.insert(name.clone(), managed);
                    children_to_monitor.push((
                        name.clone(),
                        child,
                        pid,
                        shutdown_rx,
                        health_check,
                        health_shutdown_rx,
                    ));
                    started.push(name);
                }
                Err(e) => {
                    return Response::Error {
                        message: format!("failed to start '{}': {}", name, e),
                    };
                }
            }
        }
    }

    // Spawn monitors and health checkers outside the lock
    for (name, child, pid, shutdown_rx, health_check, health_shutdown_rx) in children_to_monitor {
        process::spawn_monitor(
            name.clone(),
            child,
            pid,
            Arc::clone(processes),
            paths.clone(),
            shutdown_rx,
        );
        if let (Some(hc), Some(hc_rx)) = (health_check, health_shutdown_rx) {
            health::spawn_health_checker(name, hc, Arc::clone(processes), hc_rx);
        }
    }

    if started.is_empty() {
        Response::Success {
            message: Some("everything is already running".to_string()),
        }
    } else {
        Response::Success {
            message: Some(format!("started: {}", started.join(", "))),
        }
    }
}

async fn handle_stop(
    names: Option<Vec<String>>,
    processes: &Arc<RwLock<ProcessTable>>,
) -> Response {
    let mut table = processes.write().await;

    let targets: Vec<String> = match names {
        Some(ref requested) => {
            for name in requested {
                if !table.contains_key(name) {
                    return Response::Error {
                        message: format!("process not found: {name}"),
                    };
                }
            }
            requested.clone()
        }
        None => table.keys().cloned().collect(),
    };

    let mut stopped = Vec::new();
    for name in &targets {
        let managed = table.get_mut(name).unwrap();
        if managed.status == protocol::ProcessStatus::Stopped {
            continue;
        }
        if let Err(e) = managed.graceful_stop().await {
            return Response::Error {
                message: format!("failed to stop '{}': {}", name, e),
            };
        }
        stopped.push(name.clone());
    }

    Response::Success {
        message: Some(format!("stopped: {}", stopped.join(", "))),
    }
}

async fn handle_restart(
    names: Option<Vec<String>>,
    processes: &Arc<RwLock<ProcessTable>>,
    paths: &Paths,
) -> Response {
    let mut restarted = Vec::new();
    let mut children_to_monitor: Vec<ChildToMonitor> = Vec::new();

    {
        let mut table = processes.write().await;

        let targets: Vec<String> = match names {
            Some(ref requested) => {
                for name in requested {
                    if !table.contains_key(name) {
                        return Response::Error {
                            message: format!("process not found: {name}"),
                        };
                    }
                }
                requested.clone()
            }
            None => table.keys().cloned().collect(),
        };

        for name in &targets {
            let managed = table.get_mut(name).unwrap();
            let config = managed.config.clone();
            let old_restarts = managed.restarts;

            if managed.status != protocol::ProcessStatus::Stopped
                && let Err(e) = managed.graceful_stop().await
            {
                return Response::Error {
                    message: format!("failed to stop '{}': {}", name, e),
                };
            }

            let health_check = config.health_check.clone();
            match process::spawn_process(name.clone(), config, paths).await {
                Ok((mut new_managed, child)) => {
                    new_managed.restarts = old_restarts + 1;
                    let pid = new_managed.pid;
                    let shutdown_rx = new_managed
                        .monitor_shutdown
                        .as_ref()
                        .map(|tx| tx.subscribe())
                        .unwrap();
                    let health_shutdown_rx = health_check.as_ref().map(|_| {
                        new_managed
                            .monitor_shutdown
                            .as_ref()
                            .map(|tx| tx.subscribe())
                            .unwrap()
                    });
                    table.insert(name.clone(), new_managed);
                    children_to_monitor.push((
                        name.clone(),
                        child,
                        pid,
                        shutdown_rx,
                        health_check,
                        health_shutdown_rx,
                    ));
                    restarted.push(name.clone());
                }
                Err(e) => {
                    return Response::Error {
                        message: format!("failed to restart '{}': {}", name, e),
                    };
                }
            }
        }
    }

    // Spawn monitors and health checkers outside the lock
    for (name, child, pid, shutdown_rx, health_check, health_shutdown_rx) in children_to_monitor {
        process::spawn_monitor(
            name.clone(),
            child,
            pid,
            Arc::clone(processes),
            paths.clone(),
            shutdown_rx,
        );
        if let (Some(hc), Some(hc_rx)) = (health_check, health_shutdown_rx) {
            health::spawn_health_checker(name, hc, Arc::clone(processes), hc_rx);
        }
    }

    Response::Success {
        message: Some(format!("restarted: {}", restarted.join(", "))),
    }
}

async fn handle_flush(
    names: Option<Vec<String>>,
    processes: &Arc<RwLock<ProcessTable>>,
    paths: &Paths,
) -> Response {
    let table = processes.read().await;

    let targets: Vec<String> = match names {
        Some(ref requested) => {
            for name in requested {
                if !table.contains_key(name) {
                    return Response::Error {
                        message: format!("process not found: {name}"),
                    };
                }
            }
            requested.clone()
        }
        None => table.keys().cloned().collect(),
    };

    drop(table);

    for name in &targets {
        // Truncate main log files
        let stdout_path = paths.stdout_log(name);
        let stderr_path = paths.stderr_log(name);

        if stdout_path.exists()
            && let Err(e) = fs::write(&stdout_path, b"").await
        {
            return Response::Error {
                message: format!("failed to truncate stdout log for '{}': {}", name, e),
            };
        }
        if stderr_path.exists()
            && let Err(e) = fs::write(&stderr_path, b"").await
        {
            return Response::Error {
                message: format!("failed to truncate stderr log for '{}': {}", name, e),
            };
        }

        // Delete rotated files
        for i in 1..=log::LOG_ROTATION_KEEP {
            let _ = fs::remove_file(paths.rotated_stdout_log(name, i)).await;
            let _ = fs::remove_file(paths.rotated_stderr_log(name, i)).await;
        }
    }

    Response::Success {
        message: Some(format!("flushed logs: {}", targets.join(", "))),
    }
}

async fn handle_info(
    name: String,
    processes: &Arc<RwLock<ProcessTable>>,
    paths: &Paths,
) -> Response {
    let table = processes.read().await;
    match table.get(&name) {
        Some(managed) => {
            let detail = managed.to_process_detail(paths);
            Response::ProcessDetail {
                info: Box::new(detail),
            }
        }
        None => Response::Error {
            message: format!("process not found: {name}"),
        },
    }
}

async fn handle_log(
    name: Option<String>,
    lines: usize,
    follow: bool,
    processes: &Arc<RwLock<ProcessTable>>,
    paths: &Paths,
    writer: &mut (impl AsyncWriteExt + Unpin),
) -> color_eyre::Result<()> {
    let table = processes.read().await;

    // Determine which processes to show logs for
    let targets: Vec<String> = match name {
        Some(ref n) => {
            if !table.contains_key(n) {
                let resp = Response::Error {
                    message: format!("process not found: {n}"),
                };
                let encoded = protocol::encode_response(&resp)?;
                writer.write_all(&encoded).await?;
                return Ok(());
            }
            vec![n.clone()]
        }
        None => table.keys().cloned().collect(),
    };

    let multi = targets.len() > 1;

    // Send tail lines
    for target in &targets {
        let stdout_lines = log::tail_file(&paths.stdout_log(target), lines).unwrap_or_default();
        let stderr_lines = log::tail_file(&paths.stderr_log(target), lines).unwrap_or_default();

        // Interleave stdout and stderr (stdout first, then stderr for simplicity)
        for line in stdout_lines {
            let resp = Response::LogLine {
                name: if multi { Some(target.clone()) } else { None },
                line,
            };
            let encoded = protocol::encode_response(&resp)?;
            writer.write_all(&encoded).await?;
        }
        for line in stderr_lines {
            let resp = Response::LogLine {
                name: if multi { Some(target.clone()) } else { None },
                line,
            };
            let encoded = protocol::encode_response(&resp)?;
            writer.write_all(&encoded).await?;
        }
    }

    if !follow {
        return Ok(());
    }

    // Subscribe to broadcasters for follow mode
    let mut receivers = Vec::new();
    for target in &targets {
        if let Some(managed) = table.get(target) {
            let rx = managed.log_broadcaster.subscribe();
            receivers.push((target.clone(), rx));
        }
    }

    // Drop read lock before entering follow loop
    drop(table);

    writer.flush().await?;

    // Follow loop: receive from all broadcasters
    loop {
        // Use a simple polling approach across receivers
        let mut any_received = false;
        for (target, rx) in &mut receivers {
            match rx.try_recv() {
                Ok(entry) => {
                    let resp = Response::LogLine {
                        name: if multi { Some(target.clone()) } else { None },
                        line: entry.line,
                    };
                    let encoded = protocol::encode_response(&resp)?;
                    if writer.write_all(&encoded).await.is_err() {
                        return Ok(()); // Client disconnected
                    }
                    any_received = true;
                }
                Err(tokio::sync::broadcast::error::TryRecvError::Empty) => {}
                Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => {}
                Err(tokio::sync::broadcast::error::TryRecvError::Closed) => {
                    return Ok(());
                }
            }
        }

        if !any_received {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }

        if writer.flush().await.is_err() {
            return Ok(()); // Client disconnected
        }
    }
}
