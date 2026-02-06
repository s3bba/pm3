use crate::config::ProcessConfig;
use crate::cron;
use crate::deps;
use crate::health;
use crate::log;
use crate::memory;
use crate::paths::Paths;
use crate::pid;
use crate::process::{self, ProcessTable};
use crate::protocol::{self, ProcessStatus, Request, Response};
use crate::watch as file_watch;
use color_eyre::eyre::bail;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
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
    Option<String>,
    Option<watch::Receiver<bool>>,
    ProcessConfig,
    Option<watch::Receiver<bool>>,
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
        for (name, managed) in table.iter_mut() {
            let _ = managed.graceful_stop().await;
            if let Some(ref hook) = managed.config.post_stop {
                let _ = process::run_hook(hook, name, managed.config.cwd.as_deref(), &paths).await;
            }
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
        Request::Stop { names } => handle_stop(names, processes, paths).await,
        Request::Restart { names } => handle_restart(names, processes, paths).await,
        Request::Kill => {
            let _ = shutdown_tx.send(true);
            Response::Success {
                message: Some("daemon shutting down".to_string()),
            }
        }
        Request::Info { name } => handle_info(name, processes, paths).await,
        Request::Signal { name, signal } => handle_signal(name, signal, processes).await,
        Request::Flush { names } => handle_flush(names, processes, paths).await,
        Request::Log { .. } => {
            // Handled in handle_connection directly
            Response::Error {
                message: "unexpected dispatch for log".to_string(),
            }
        }
        Request::Reload { names } => handle_reload(names, processes, paths).await,
        Request::Save => handle_save(processes, paths).await,
        Request::Resurrect => handle_resurrect(processes, paths).await,
    }
}

/// Resolve names that may be process names or group names.
/// Process names take priority over group names.
fn resolve_config_names(
    requested: &[String],
    configs: &HashMap<String, ProcessConfig>,
) -> Result<Vec<String>, String> {
    let mut result = Vec::new();
    for name in requested {
        if configs.contains_key(name) {
            result.push(name.clone());
        } else {
            let group_matches: Vec<String> = configs
                .iter()
                .filter(|(_, c)| c.group.as_deref() == Some(name))
                .map(|(k, _)| k.clone())
                .collect();
            if group_matches.is_empty() {
                return Err(format!("process or group '{}' not found in configs", name));
            }
            result.extend(group_matches);
        }
    }
    Ok(result)
}

/// Resolve names against running process table — process names take priority over group names.
fn resolve_table_names(requested: &[String], table: &ProcessTable) -> Result<Vec<String>, String> {
    let mut result = Vec::new();
    for name in requested {
        if table.contains_key(name) {
            result.push(name.clone());
        } else {
            let group_matches: Vec<String> = table
                .iter()
                .filter(|(_, m)| m.config.group.as_deref() == Some(name))
                .map(|(k, _)| k.clone())
                .collect();
            if group_matches.is_empty() {
                return Err(format!("process or group not found: {name}"));
            }
            result.extend(group_matches);
        }
    }
    Ok(result)
}

const DEP_WAIT_TIMEOUT: Duration = Duration::from_secs(60);
const DEP_POLL_INTERVAL: Duration = Duration::from_millis(200);

async fn handle_start(
    configs: HashMap<String, ProcessConfig>,
    names: Option<Vec<String>>,
    env: Option<String>,
    processes: &Arc<RwLock<ProcessTable>>,
    paths: &Paths,
) -> Response {
    let mut to_start: Vec<(String, ProcessConfig)> = match names {
        Some(ref requested) => {
            let resolved = match resolve_config_names(requested, &configs) {
                Ok(r) => r,
                Err(msg) => return Response::Error { message: msg },
            };
            resolved
                .into_iter()
                .map(|name| {
                    let config = configs.get(&name).unwrap().clone();
                    (name, config)
                })
                .collect()
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

    // Build subset configs map for dependency analysis
    let subset_configs: HashMap<String, ProcessConfig> = to_start.iter().cloned().collect();

    // Validate dependencies
    if let Err(e) = deps::validate_deps(&subset_configs) {
        return Response::Error {
            message: e.to_string(),
        };
    }

    // Get level-grouped start order
    let levels = match deps::topological_levels(&subset_configs) {
        Ok(l) => l,
        Err(e) => {
            return Response::Error {
                message: e.to_string(),
            };
        }
    };

    let mut started = Vec::new();

    for (level_idx, level) in levels.iter().enumerate() {
        let mut children_to_monitor: Vec<ChildToMonitor> = Vec::new();
        let level_names: Vec<String>;

        {
            let mut table = processes.write().await;
            let mut names_this_level = Vec::new();

            for name in level {
                if table.contains_key(name) {
                    continue;
                }

                let config = subset_configs.get(name).unwrap().clone();
                let health_check = config.health_check.clone();
                let max_memory = config.max_memory.clone();
                let cron_restart = config.cron_restart.clone();
                match process::spawn_process(name.clone(), config.clone(), paths).await {
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
                        let mem_shutdown_rx = max_memory.as_ref().map(|_| {
                            managed
                                .monitor_shutdown
                                .as_ref()
                                .map(|tx| tx.subscribe())
                                .unwrap()
                        });
                        let watch_shutdown_rx = file_watch::resolve_watch_path(&config).map(|_| {
                            managed
                                .monitor_shutdown
                                .as_ref()
                                .map(|tx| tx.subscribe())
                                .unwrap()
                        });
                        let cron_shutdown_rx = cron_restart.as_ref().map(|_| {
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
                            max_memory,
                            mem_shutdown_rx,
                            config,
                            watch_shutdown_rx,
                            cron_restart,
                            cron_shutdown_rx,
                        ));
                        names_this_level.push(name.clone());
                    }
                    Err(e) => {
                        return Response::Error {
                            message: format!("failed to start '{}': {}", name, e),
                        };
                    }
                }
            }

            level_names = names_this_level;
        }

        // Spawn monitors, health checkers, memory monitors, file watchers, and cron outside the lock
        for (
            name,
            child,
            pid,
            shutdown_rx,
            health_check,
            health_shutdown_rx,
            max_memory,
            mem_shutdown_rx,
            config,
            watch_shutdown_rx,
            cron_restart,
            cron_shutdown_rx,
        ) in children_to_monitor
        {
            process::spawn_monitor(
                name.clone(),
                child,
                pid,
                Arc::clone(processes),
                paths.clone(),
                shutdown_rx,
            );
            if let (Some(hc), Some(hc_rx)) = (health_check, health_shutdown_rx) {
                health::spawn_health_checker(name.clone(), hc, Arc::clone(processes), hc_rx);
            }
            if let (Some(mm), Some(mm_rx)) = (max_memory, mem_shutdown_rx) {
                memory::spawn_memory_monitor(
                    name.clone(),
                    mm,
                    Arc::clone(processes),
                    paths.clone(),
                    mm_rx,
                );
            }
            if let Some(w_rx) = watch_shutdown_rx {
                file_watch::spawn_watcher(
                    name.clone(),
                    config,
                    Arc::clone(processes),
                    paths.clone(),
                    w_rx,
                );
            }
            if let (Some(cr), Some(cr_rx)) = (cron_restart, cron_shutdown_rx) {
                cron::spawn_cron_restart(name, cr, Arc::clone(processes), paths.clone(), cr_rx);
            }
        }

        started.extend(level_names.clone());

        // Wait for this level to come online before starting the next level
        let is_last_level = level_idx == levels.len() - 1;
        if !is_last_level
            && !level_names.is_empty()
            && let Err(msg) = wait_for_online(&level_names, processes).await
        {
            return Response::Error { message: msg };
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

/// Poll the process table until all named processes are Online, or timeout/failure.
async fn wait_for_online(
    names: &[String],
    processes: &Arc<RwLock<ProcessTable>>,
) -> Result<(), String> {
    let deadline = tokio::time::Instant::now() + DEP_WAIT_TIMEOUT;

    loop {
        if tokio::time::Instant::now() >= deadline {
            return Err(format!(
                "timeout waiting for dependencies to come online: {}",
                names.join(", ")
            ));
        }

        {
            let table = processes.read().await;
            let mut all_online = true;
            for name in names {
                if let Some(managed) = table.get(name) {
                    match managed.status {
                        ProcessStatus::Online => {}
                        ProcessStatus::Stopped | ProcessStatus::Errored => {
                            return Err(format!(
                                "dependency '{}' failed (status: {})",
                                name, managed.status
                            ));
                        }
                        ProcessStatus::Unhealthy => {
                            return Err(format!("dependency '{}' is unhealthy", name));
                        }
                        ProcessStatus::Starting => {
                            all_online = false;
                        }
                    }
                } else {
                    return Err(format!("dependency '{}' not found in process table", name));
                }
            }
            if all_online {
                return Ok(());
            }
        }

        tokio::time::sleep(DEP_POLL_INTERVAL).await;
    }
}

async fn handle_stop(
    names: Option<Vec<String>>,
    processes: &Arc<RwLock<ProcessTable>>,
    paths: &Paths,
) -> Response {
    let mut table = processes.write().await;

    let targets: Vec<String> = match names {
        Some(ref requested) => match resolve_table_names(requested, &table) {
            Ok(r) => r,
            Err(msg) => return Response::Error { message: msg },
        },
        None => table.keys().cloned().collect(),
    };

    // Build configs map from running processes for dependency analysis
    let running_configs: HashMap<String, ProcessConfig> = table
        .iter()
        .map(|(k, v)| (k.clone(), v.config.clone()))
        .collect();

    // Expand targets to include transitive dependents and order for stop
    let stop_order = match deps::expand_dependents(&targets, &running_configs) {
        Ok(order) => order,
        Err(e) => {
            return Response::Error {
                message: e.to_string(),
            };
        }
    };

    let mut stopped = Vec::new();
    for name in &stop_order {
        let managed = match table.get_mut(name) {
            Some(m) => m,
            None => continue,
        };
        if managed.status == ProcessStatus::Stopped {
            continue;
        }
        if let Err(e) = managed.graceful_stop().await {
            return Response::Error {
                message: format!("failed to stop '{}': {}", name, e),
            };
        }
        if let Some(ref hook) = managed.config.post_stop {
            let _ = process::run_hook(hook, name, managed.config.cwd.as_deref(), paths).await;
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
    // Collect targets and configs under a single lock scope
    let (targets, restart_configs) = {
        let table = processes.read().await;

        let targets: Vec<String> = match names {
            Some(ref requested) => match resolve_table_names(requested, &table) {
                Ok(r) => r,
                Err(msg) => return Response::Error { message: msg },
            },
            None => table.keys().cloned().collect(),
        };

        let running_configs: HashMap<String, ProcessConfig> = table
            .iter()
            .map(|(k, v)| (k.clone(), v.config.clone()))
            .collect();

        (targets, running_configs)
    };

    // Compute stop order (reverse topo: dependents first)
    let stop_order = match deps::expand_dependents(&targets, &restart_configs) {
        Ok(order) => order,
        Err(e) => {
            return Response::Error {
                message: e.to_string(),
            };
        }
    };

    // Stop phase: stop in reverse dependency order
    let mut old_restarts_map: HashMap<String, u32> = HashMap::new();
    {
        let mut table = processes.write().await;
        for name in &stop_order {
            let managed = match table.get_mut(name) {
                Some(m) => m,
                None => continue,
            };
            old_restarts_map.insert(name.clone(), managed.restarts);

            if managed.status != ProcessStatus::Stopped
                && let Err(e) = managed.graceful_stop().await
            {
                return Response::Error {
                    message: format!("failed to stop '{}': {}", name, e),
                };
            }
            if let Some(ref hook) = managed.config.post_stop {
                let _ = process::run_hook(hook, name, managed.config.cwd.as_deref(), paths).await;
            }
        }
    }

    // Build subset configs for the processes we're restarting
    let subset_configs: HashMap<String, ProcessConfig> = restart_configs
        .iter()
        .filter(|(k, _)| stop_order.contains(k))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();

    // Get forward start order (topological levels)
    let levels = match deps::topological_levels(&subset_configs) {
        Ok(l) => l,
        Err(e) => {
            return Response::Error {
                message: e.to_string(),
            };
        }
    };

    // Start phase: start in forward dependency order, level by level
    let mut restarted = Vec::new();

    for (level_idx, level) in levels.iter().enumerate() {
        let mut children_to_monitor: Vec<ChildToMonitor> = Vec::new();
        let mut level_names: Vec<String> = Vec::new();

        {
            let mut table = processes.write().await;

            for name in level {
                let config = match subset_configs.get(name) {
                    Some(c) => c.clone(),
                    None => continue,
                };
                let old_restarts = old_restarts_map.get(name).copied().unwrap_or(0);

                let health_check = config.health_check.clone();
                let max_memory = config.max_memory.clone();
                let cron_restart = config.cron_restart.clone();
                match process::spawn_process(name.clone(), config.clone(), paths).await {
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
                        let mem_shutdown_rx = max_memory.as_ref().map(|_| {
                            new_managed
                                .monitor_shutdown
                                .as_ref()
                                .map(|tx| tx.subscribe())
                                .unwrap()
                        });
                        let watch_shutdown_rx = file_watch::resolve_watch_path(&config).map(|_| {
                            new_managed
                                .monitor_shutdown
                                .as_ref()
                                .map(|tx| tx.subscribe())
                                .unwrap()
                        });
                        let cron_shutdown_rx = cron_restart.as_ref().map(|_| {
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
                            max_memory,
                            mem_shutdown_rx,
                            config,
                            watch_shutdown_rx,
                            cron_restart,
                            cron_shutdown_rx,
                        ));
                        level_names.push(name.clone());
                    }
                    Err(e) => {
                        return Response::Error {
                            message: format!("failed to restart '{}': {}", name, e),
                        };
                    }
                }
            }
        }

        // Spawn monitors, health checkers, memory monitors, file watchers, and cron outside the lock
        for (
            name,
            child,
            pid,
            shutdown_rx,
            health_check,
            health_shutdown_rx,
            max_memory,
            mem_shutdown_rx,
            config,
            watch_shutdown_rx,
            cron_restart,
            cron_shutdown_rx,
        ) in children_to_monitor
        {
            process::spawn_monitor(
                name.clone(),
                child,
                pid,
                Arc::clone(processes),
                paths.clone(),
                shutdown_rx,
            );
            if let (Some(hc), Some(hc_rx)) = (health_check, health_shutdown_rx) {
                health::spawn_health_checker(name.clone(), hc, Arc::clone(processes), hc_rx);
            }
            if let (Some(mm), Some(mm_rx)) = (max_memory, mem_shutdown_rx) {
                memory::spawn_memory_monitor(
                    name.clone(),
                    mm,
                    Arc::clone(processes),
                    paths.clone(),
                    mm_rx,
                );
            }
            if let Some(w_rx) = watch_shutdown_rx {
                file_watch::spawn_watcher(
                    name.clone(),
                    config,
                    Arc::clone(processes),
                    paths.clone(),
                    w_rx,
                );
            }
            if let (Some(cr), Some(cr_rx)) = (cron_restart, cron_shutdown_rx) {
                cron::spawn_cron_restart(name, cr, Arc::clone(processes), paths.clone(), cr_rx);
            }
        }

        restarted.extend(level_names.clone());

        // Wait for this level to come online before starting the next level
        let is_last_level = level_idx == levels.len() - 1;
        if !is_last_level
            && !level_names.is_empty()
            && let Err(msg) = wait_for_online(&level_names, processes).await
        {
            return Response::Error { message: msg };
        }
    }

    Response::Success {
        message: Some(format!("restarted: {}", restarted.join(", "))),
    }
}

async fn handle_reload(
    names: Option<Vec<String>>,
    processes: &Arc<RwLock<ProcessTable>>,
    paths: &Paths,
) -> Response {
    // Resolve targets from process table
    let targets = {
        let table = processes.read().await;
        let targets: Vec<String> = match names {
            Some(ref requested) => match resolve_table_names(requested, &table) {
                Ok(r) => r,
                Err(msg) => return Response::Error { message: msg },
            },
            None => table.keys().cloned().collect(),
        };
        targets
    };

    // Separate processes with and without health checks
    let mut with_hc: Vec<(String, ProcessConfig, u32)> = Vec::new();
    let mut without_hc: Vec<String> = Vec::new();

    {
        let table = processes.read().await;
        for name in &targets {
            if let Some(managed) = table.get(name) {
                if managed.config.health_check.is_some() {
                    with_hc.push((name.clone(), managed.config.clone(), managed.restarts));
                } else {
                    without_hc.push(name.clone());
                }
            }
        }
    }

    let mut reloaded = Vec::new();
    let mut failed = Vec::new();

    // Handle processes WITH health checks: zero-downtime reload
    for (name, config, old_restarts) in with_hc {
        let temp_name = format!("__reload_{}", name);
        let health_check = config.health_check.clone();
        let max_memory = config.max_memory.clone();
        let cron_restart = config.cron_restart.clone();

        // Spawn new process under temporary name
        match process::spawn_process(temp_name.clone(), config.clone(), paths).await {
            Ok((mut new_managed, new_child)) => {
                new_managed.restarts = old_restarts;
                let new_pid = new_managed.pid;
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

                // Insert temp entry into process table
                {
                    let mut table = processes.write().await;
                    table.insert(temp_name.clone(), new_managed);
                }

                // Spawn monitor for temp process
                process::spawn_monitor(
                    temp_name.clone(),
                    new_child,
                    new_pid,
                    Arc::clone(processes),
                    paths.clone(),
                    shutdown_rx,
                );

                // Spawn health checker for temp process
                if let (Some(hc), Some(hc_rx)) = (health_check, health_shutdown_rx) {
                    health::spawn_health_checker(
                        temp_name.clone(),
                        hc,
                        Arc::clone(processes),
                        hc_rx,
                    );
                }

                // Wait for new process to come online
                match wait_for_online(std::slice::from_ref(&temp_name), processes).await {
                    Ok(()) => {
                        // New process is online — stop the old one and swap
                        let mut table = processes.write().await;

                        // Stop old process
                        if let Some(old_managed) = table.get_mut(&name) {
                            let _ = old_managed.graceful_stop().await;
                            if let Some(ref hook) = config.post_stop {
                                let _ =
                                    process::run_hook(hook, &name, config.cwd.as_deref(), paths)
                                        .await;
                            }
                        }

                        // Move temp entry to original name and spawn memory monitor + watcher + cron
                        if let Some(mut new_managed) = table.remove(&temp_name) {
                            new_managed.name = name.clone();
                            let mem_shutdown_rx = max_memory.as_ref().map(|_| {
                                new_managed
                                    .monitor_shutdown
                                    .as_ref()
                                    .map(|tx| tx.subscribe())
                                    .unwrap()
                            });
                            let watch_shutdown_rx =
                                file_watch::resolve_watch_path(&config).map(|_| {
                                    new_managed
                                        .monitor_shutdown
                                        .as_ref()
                                        .map(|tx| tx.subscribe())
                                        .unwrap()
                                });
                            let cron_shutdown_rx = cron_restart.as_ref().map(|_| {
                                new_managed
                                    .monitor_shutdown
                                    .as_ref()
                                    .map(|tx| tx.subscribe())
                                    .unwrap()
                            });
                            table.insert(name.clone(), new_managed);
                            drop(table);
                            if let (Some(mm), Some(mm_rx)) = (max_memory.clone(), mem_shutdown_rx) {
                                memory::spawn_memory_monitor(
                                    name.clone(),
                                    mm,
                                    Arc::clone(processes),
                                    paths.clone(),
                                    mm_rx,
                                );
                            }
                            if let Some(w_rx) = watch_shutdown_rx {
                                file_watch::spawn_watcher(
                                    name.clone(),
                                    config.clone(),
                                    Arc::clone(processes),
                                    paths.clone(),
                                    w_rx,
                                );
                            }
                            if let (Some(cr), Some(cr_rx)) =
                                (cron_restart.clone(), cron_shutdown_rx)
                            {
                                cron::spawn_cron_restart(
                                    name.clone(),
                                    cr,
                                    Arc::clone(processes),
                                    paths.clone(),
                                    cr_rx,
                                );
                            }
                        }

                        reloaded.push(name);
                    }
                    Err(_) => {
                        // New process failed health check — kill it and keep old one
                        let mut table = processes.write().await;
                        if let Some(temp_managed) = table.get_mut(&temp_name) {
                            let _ = temp_managed.graceful_stop().await;
                        }
                        table.remove(&temp_name);
                        failed.push(name);
                    }
                }
            }
            Err(e) => {
                failed.push(format!("{} (spawn failed: {})", name, e));
            }
        }
    }

    // Handle processes WITHOUT health checks: fall back to restart
    if !without_hc.is_empty() {
        match handle_restart(Some(without_hc.clone()), processes, paths).await {
            Response::Success { .. } => {
                reloaded.extend(without_hc);
            }
            Response::Error { message } => {
                return Response::Error { message };
            }
            _ => {}
        }
    }

    if reloaded.is_empty() && !failed.is_empty() {
        return Response::Error {
            message: format!("reload failed: {}", failed.join(", ")),
        };
    }

    let mut msg = format!("reloaded: {}", reloaded.join(", "));
    if !failed.is_empty() {
        msg.push_str(&format!(" (failed: {})", failed.join(", ")));
    }

    Response::Success { message: Some(msg) }
}

// ---------------------------------------------------------------------------
// State persistence (save / resurrect)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DumpEntry {
    name: String,
    config: ProcessConfig,
    pid: Option<u32>,
    restarts: u32,
}

async fn handle_save(processes: &Arc<RwLock<ProcessTable>>, paths: &Paths) -> Response {
    let table = processes.read().await;

    let entries: Vec<DumpEntry> = table
        .values()
        .map(|managed| DumpEntry {
            name: managed.name.clone(),
            config: managed.config.clone(),
            pid: managed.pid,
            restarts: managed.restarts,
        })
        .collect();

    drop(table);

    let json = match serde_json::to_string_pretty(&entries) {
        Ok(j) => j,
        Err(e) => {
            return Response::Error {
                message: format!("failed to serialize state: {}", e),
            };
        }
    };

    if let Err(e) = fs::write(paths.dump_file(), json.as_bytes()).await {
        return Response::Error {
            message: format!("failed to write dump file: {}", e),
        };
    }

    Response::Success {
        message: Some(format!("saved {} process(es) to dump file", entries.len())),
    }
}

fn is_pid_alive(pid: u32) -> bool {
    nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32), None).is_ok()
}

async fn handle_resurrect(processes: &Arc<RwLock<ProcessTable>>, paths: &Paths) -> Response {
    let dump_path = paths.dump_file();
    if !dump_path.exists() {
        return Response::Error {
            message: "no dump file found".to_string(),
        };
    }

    let data = match fs::read_to_string(&dump_path).await {
        Ok(d) => d,
        Err(e) => {
            return Response::Error {
                message: format!("failed to read dump file: {}", e),
            };
        }
    };

    let entries: Vec<DumpEntry> = match serde_json::from_str(&data) {
        Ok(e) => e,
        Err(e) => {
            return Response::Error {
                message: format!("failed to parse dump file: {}", e),
            };
        }
    };

    // Skip entries that are already running
    let already_running: Vec<String> = {
        let table = processes.read().await;
        entries
            .iter()
            .filter(|e| table.contains_key(&e.name))
            .map(|e| e.name.clone())
            .collect()
    };

    let to_restore: Vec<DumpEntry> = entries
        .into_iter()
        .filter(|e| !already_running.contains(&e.name))
        .collect();

    if to_restore.is_empty() {
        return Response::Success {
            message: Some("all processes already running".to_string()),
        };
    }

    // Build subset configs for dependency ordering
    let subset_configs: HashMap<String, ProcessConfig> = to_restore
        .iter()
        .map(|e| (e.name.clone(), e.config.clone()))
        .collect();

    // Validate and order by dependencies
    if let Err(e) = deps::validate_deps(&subset_configs) {
        return Response::Error {
            message: e.to_string(),
        };
    }

    let levels = match deps::topological_levels(&subset_configs) {
        Ok(l) => l,
        Err(e) => {
            return Response::Error {
                message: e.to_string(),
            };
        }
    };

    // Build a lookup from name -> DumpEntry for restarts
    let entry_map: HashMap<String, &DumpEntry> =
        to_restore.iter().map(|e| (e.name.clone(), e)).collect();

    let mut restored = Vec::new();

    for (level_idx, level) in levels.iter().enumerate() {
        let mut children_to_monitor: Vec<ChildToMonitor> = Vec::new();
        let mut level_names: Vec<String> = Vec::new();

        {
            let mut table = processes.write().await;

            for name in level {
                if table.contains_key(name) {
                    continue;
                }

                let entry = match entry_map.get(name) {
                    Some(e) => e,
                    None => continue,
                };

                // Check if the old PID is still alive
                let old_alive = entry.pid.is_some_and(is_pid_alive);

                if old_alive {
                    // Re-adopt: the process is still running from before the daemon restart.
                    // We cannot re-attach to its stdout/stderr, but we track it.
                    let (log_tx, _) = tokio::sync::broadcast::channel(1024);
                    let (monitor_tx, _) = watch::channel(false);

                    let status = if entry.config.health_check.is_some() {
                        ProcessStatus::Starting
                    } else {
                        ProcessStatus::Online
                    };

                    let managed = process::ManagedProcess {
                        name: name.clone(),
                        config: entry.config.clone(),
                        pid: entry.pid,
                        status,
                        started_at: tokio::time::Instant::now(),
                        restarts: entry.restarts,
                        log_broadcaster: log_tx,
                        monitor_shutdown: Some(monitor_tx),
                    };

                    table.insert(name.clone(), managed);
                    level_names.push(name.clone());
                } else {
                    // PID is dead or missing — spawn fresh
                    let config = entry.config.clone();
                    let health_check = config.health_check.clone();
                    let max_memory = config.max_memory.clone();
                    let cron_restart = config.cron_restart.clone();
                    match process::spawn_process(name.clone(), config.clone(), paths).await {
                        Ok((mut managed, child)) => {
                            managed.restarts = entry.restarts;
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
                            let mem_shutdown_rx = max_memory.as_ref().map(|_| {
                                managed
                                    .monitor_shutdown
                                    .as_ref()
                                    .map(|tx| tx.subscribe())
                                    .unwrap()
                            });
                            let watch_shutdown_rx =
                                file_watch::resolve_watch_path(&config).map(|_| {
                                    managed
                                        .monitor_shutdown
                                        .as_ref()
                                        .map(|tx| tx.subscribe())
                                        .unwrap()
                                });
                            let cron_shutdown_rx = cron_restart.as_ref().map(|_| {
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
                                max_memory,
                                mem_shutdown_rx,
                                config,
                                watch_shutdown_rx,
                                cron_restart,
                                cron_shutdown_rx,
                            ));
                            level_names.push(name.clone());
                        }
                        Err(e) => {
                            return Response::Error {
                                message: format!("failed to resurrect '{}': {}", name, e),
                            };
                        }
                    }
                }
            }
        }

        // Spawn monitors for freshly spawned processes
        for (
            name,
            child,
            pid,
            shutdown_rx,
            health_check,
            health_shutdown_rx,
            max_memory,
            mem_shutdown_rx,
            config,
            watch_shutdown_rx,
            cron_restart,
            cron_shutdown_rx,
        ) in children_to_monitor
        {
            process::spawn_monitor(
                name.clone(),
                child,
                pid,
                Arc::clone(processes),
                paths.clone(),
                shutdown_rx,
            );
            if let (Some(hc), Some(hc_rx)) = (health_check, health_shutdown_rx) {
                health::spawn_health_checker(name.clone(), hc, Arc::clone(processes), hc_rx);
            }
            if let (Some(mm), Some(mm_rx)) = (max_memory, mem_shutdown_rx) {
                memory::spawn_memory_monitor(
                    name.clone(),
                    mm,
                    Arc::clone(processes),
                    paths.clone(),
                    mm_rx,
                );
            }
            if let Some(w_rx) = watch_shutdown_rx {
                file_watch::spawn_watcher(
                    name.clone(),
                    config,
                    Arc::clone(processes),
                    paths.clone(),
                    w_rx,
                );
            }
            if let (Some(cr), Some(cr_rx)) = (cron_restart, cron_shutdown_rx) {
                cron::spawn_cron_restart(name, cr, Arc::clone(processes), paths.clone(), cr_rx);
            }
        }

        // Spawn health/memory/cron monitors for re-adopted processes
        {
            let table = processes.read().await;
            for name in &level_names {
                let managed = match table.get(name) {
                    Some(m) => m,
                    None => continue,
                };
                // Only for re-adopted (has PID, no child handle in children_to_monitor)
                let entry = match entry_map.get(name) {
                    Some(e) => e,
                    None => continue,
                };
                if !entry.pid.is_some_and(is_pid_alive) {
                    continue; // Was spawned fresh — already handled above
                }

                if let Some(ref hc) = entry.config.health_check {
                    let hc_rx = managed
                        .monitor_shutdown
                        .as_ref()
                        .map(|tx| tx.subscribe())
                        .unwrap();
                    health::spawn_health_checker(
                        name.clone(),
                        hc.clone(),
                        Arc::clone(processes),
                        hc_rx,
                    );
                }
                if let Some(ref mm) = entry.config.max_memory {
                    let mm_rx = managed
                        .monitor_shutdown
                        .as_ref()
                        .map(|tx| tx.subscribe())
                        .unwrap();
                    memory::spawn_memory_monitor(
                        name.clone(),
                        mm.clone(),
                        Arc::clone(processes),
                        paths.clone(),
                        mm_rx,
                    );
                }
                if let Some(ref cr) = entry.config.cron_restart {
                    let cr_rx = managed
                        .monitor_shutdown
                        .as_ref()
                        .map(|tx| tx.subscribe())
                        .unwrap();
                    cron::spawn_cron_restart(
                        name.clone(),
                        cr.clone(),
                        Arc::clone(processes),
                        paths.clone(),
                        cr_rx,
                    );
                }
            }
        }

        restored.extend(level_names.clone());

        // Wait for this level to come online before starting the next level
        let is_last_level = level_idx == levels.len() - 1;
        if !is_last_level
            && !level_names.is_empty()
            && let Err(msg) = wait_for_online(&level_names, processes).await
        {
            return Response::Error { message: msg };
        }
    }

    Response::Success {
        message: Some(format!("resurrected: {}", restored.join(", "))),
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

async fn handle_signal(
    name: String,
    signal: String,
    processes: &Arc<RwLock<ProcessTable>>,
) -> Response {
    let table = processes.read().await;
    let managed = match table.get(&name) {
        Some(m) => m,
        None => {
            return Response::Error {
                message: format!("process not found: {name}"),
            };
        }
    };

    let raw_pid = match managed.pid {
        Some(pid) => pid,
        None => {
            return Response::Error {
                message: format!("process '{name}' is not running"),
            };
        }
    };

    let sig = match process::parse_signal(&signal) {
        Ok(s) => s,
        Err(e) => {
            return Response::Error {
                message: e.to_string(),
            };
        }
    };

    let pid = nix::unistd::Pid::from_raw(raw_pid as i32);
    if let Err(e) = nix::sys::signal::kill(pid, sig) {
        return Response::Error {
            message: format!("failed to send signal to '{}': {}", name, e),
        };
    }

    Response::Success {
        message: Some(format!("sent {} to '{}'", signal, name)),
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
