use std::io::{BufRead, IsTerminal, Write};
use std::path::Path;

use color_eyre::eyre::{bail, eyre};

struct InitProcess {
    name: String,
    command: String,
    cwd: Option<String>,
    env: Vec<(String, String)>,
    restart: Option<String>,
    readiness_check: Option<String>,
    health_check: Option<String>,
    group: Option<String>,
    depends_on: Vec<String>,
}

fn escape_toml_string(s: &str) -> String {
    toml::Value::String(s.to_string()).to_string()
}

fn generate_toml(processes: &[InitProcess]) -> String {
    let mut out = String::new();

    for (i, p) in processes.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }

        out.push_str(&format!("[{}]\n", p.name));
        out.push_str(&format!("command = {}\n", escape_toml_string(&p.command)));

        if let Some(cwd) = &p.cwd {
            out.push_str(&format!("cwd = {}\n", escape_toml_string(cwd)));
        }

        if !p.env.is_empty() {
            out.push_str("env = { ");
            let pairs: Vec<String> = p
                .env
                .iter()
                .map(|(k, v)| format!("{} = {}", k, escape_toml_string(v)))
                .collect();
            out.push_str(&pairs.join(", "));
            out.push_str(" }\n");
        }

        if let Some(restart) = &p.restart {
            out.push_str(&format!("restart = {}\n", escape_toml_string(restart)));
        }

        if let Some(readiness_check) = &p.readiness_check {
            out.push_str(&format!(
                "readiness_check = {}\n",
                escape_toml_string(readiness_check)
            ));
        }

        if let Some(health_check) = &p.health_check {
            out.push_str(&format!(
                "health_check = {}\n",
                escape_toml_string(health_check)
            ));
        }

        if let Some(group) = &p.group {
            out.push_str(&format!("group = {}\n", escape_toml_string(group)));
        }

        if !p.depends_on.is_empty() {
            let deps: Vec<String> = p.depends_on.iter().map(|d| escape_toml_string(d)).collect();
            out.push_str(&format!("depends_on = [{}]\n", deps.join(", ")));
        }
    }

    out
}

fn parse_env_pairs(input: &str) -> Result<Vec<(String, String)>, String> {
    let mut pairs = Vec::new();
    for part in input.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let (key, value) = part
            .split_once('=')
            .ok_or_else(|| format!("invalid KEY=VALUE pair: {part}"))?;
        let key = key.trim();
        let value = value.trim();
        if key.is_empty() {
            return Err(format!("empty key in: {part}"));
        }
        pairs.push((key.to_string(), value.to_string()));
    }
    Ok(pairs)
}

fn finalize(dir: &Path, processes: &[InitProcess]) -> color_eyre::Result<()> {
    let config_path = dir.join("pm3.toml");
    let toml_content = generate_toml(processes);

    crate::config::parse_config(&toml_content)
        .map_err(|e| eyre!("generated TOML failed validation: {e}"))?;

    std::fs::write(&config_path, &toml_content)?;
    Ok(())
}

pub fn run(dir: &Path) -> color_eyre::Result<()> {
    if std::io::stdin().is_terminal() {
        run_interactive(dir)
    } else {
        let stdin = std::io::stdin();
        let mut reader = stdin.lock();
        run_piped(dir, &mut reader)
    }
}

// ── Interactive mode (cliclack) ─────────────────────────────────────

fn run_interactive(dir: &Path) -> color_eyre::Result<()> {
    let config_path = dir.join("pm3.toml");

    cliclack::intro("pm3 init")?;

    if config_path.exists() {
        let overwrite: bool = cliclack::confirm("pm3.toml already exists. Overwrite?")
            .initial_value(false)
            .interact()?;
        if !overwrite {
            cliclack::outro_cancel("Aborted.")?;
            bail!("aborted");
        }
    }

    let mut processes: Vec<InitProcess> = Vec::new();
    let mut process_num = 1u32;

    loop {
        if process_num > 1 {
            cliclack::log::step(format!("Process #{process_num}"))?;
        }

        let existing_names: Vec<String> = processes.iter().map(|p| p.name.clone()).collect();
        let name: String = cliclack::input("Process name")
            .placeholder("web")
            .required(true)
            .validate(move |input: &String| {
                if input.contains(' ')
                    || input.contains('.')
                    || input.contains('[')
                    || input.contains(']')
                    || input.contains('#')
                {
                    Err("name cannot contain spaces, dots, brackets, or #")
                } else if existing_names.contains(input) {
                    Err("a process with this name already exists")
                } else {
                    Ok(())
                }
            })
            .interact()?;

        let command: String = cliclack::input("Command")
            .placeholder("npm start")
            .required(true)
            .interact()?;

        let cwd: String = cliclack::input("Working directory")
            .placeholder("leave empty to use current directory")
            .default_input("")
            .required(false)
            .interact()?;
        let cwd = if cwd.is_empty() { None } else { Some(cwd) };

        let env_input: String = cliclack::input("Environment variables")
            .placeholder("KEY=VALUE, KEY2=VALUE2")
            .default_input("")
            .required(false)
            .validate(|input: &String| {
                if input.is_empty() {
                    return Ok(());
                }
                for part in input.split(',') {
                    let part = part.trim();
                    if part.is_empty() {
                        continue;
                    }
                    match part.split_once('=') {
                        None => return Err("expected KEY=VALUE format"),
                        Some((k, _)) if k.trim().is_empty() => return Err("key cannot be empty"),
                        _ => {}
                    }
                }
                Ok(())
            })
            .interact()?;
        let env = if env_input.is_empty() {
            Vec::new()
        } else {
            parse_env_pairs(&env_input).map_err(|e| eyre!(e))?
        };

        let restart: &str = cliclack::select("Restart policy")
            .item("on_failure", "On failure", "restart only on non-zero exit")
            .item("always", "Always", "restart regardless of exit code")
            .item("never", "Never", "run once, don't restart")
            .initial_value("on_failure")
            .interact()?;
        let restart = Some(restart.to_string());

        let health_check: String = cliclack::input("Health check URL")
            .placeholder("http://localhost:3000/health")
            .default_input("")
            .required(false)
            .validate(|input: &String| {
                if input.is_empty() {
                    return Ok(());
                }
                if !input.starts_with("http://")
                    && !input.starts_with("https://")
                    && !input.starts_with("tcp://")
                {
                    Err("must start with http://, https://, or tcp://")
                } else {
                    Ok(())
                }
            })
            .interact()?;
        let health_check = if health_check.is_empty() {
            None
        } else {
            Some(health_check)
        };

        let group: String = cliclack::input("Group name")
            .placeholder("backend")
            .default_input("")
            .required(false)
            .interact()?;
        let group = if group.is_empty() { None } else { Some(group) };

        let existing_names: Vec<&str> = processes.iter().map(|p| p.name.as_str()).collect();
        let depends_on: Vec<String> = if existing_names.is_empty() {
            Vec::new()
        } else {
            let selected: Vec<String> = cliclack::multiselect(format!(
                "Dependencies (processes that must start before {})",
                name
            ))
            .items(
                &existing_names
                    .iter()
                    .map(|n| (n.to_string(), *n, ""))
                    .collect::<Vec<_>>(),
            )
            .required(false)
            .interact()?;
            selected
        };

        processes.push(InitProcess {
            name,
            command,
            cwd,
            env,
            restart,
            readiness_check: None,
            health_check,
            group,
            depends_on,
        });

        let add_another: bool = cliclack::confirm("Add another process?")
            .initial_value(false)
            .interact()?;
        if !add_another {
            break;
        }
        process_num += 1;
    }

    finalize(dir, &processes)?;

    cliclack::outro(format!("Created {}", dir.join("pm3.toml").display()))?;

    Ok(())
}

// ── Piped mode (plain stdin/stderr for E2E tests) ───────────────────

fn plain_prompt(
    reader: &mut impl BufRead,
    prompt: &str,
    default: Option<&str>,
) -> color_eyre::Result<String> {
    let stderr = std::io::stderr();
    let mut stderr = stderr.lock();
    if let Some(def) = default {
        write!(stderr, "{prompt} [{def}]: ")?;
    } else {
        write!(stderr, "{prompt}: ")?;
    }
    stderr.flush()?;

    let mut line = String::new();
    reader.read_line(&mut line)?;
    let line = line.trim_end_matches('\n').trim_end_matches('\r');

    if line.is_empty()
        && let Some(def) = default
    {
        return Ok(def.to_string());
    }

    Ok(line.to_string())
}

fn plain_prompt_required(
    reader: &mut impl BufRead,
    prompt_text: &str,
) -> color_eyre::Result<String> {
    loop {
        let value = plain_prompt(reader, prompt_text, None)?;
        if !value.is_empty() {
            return Ok(value);
        }
        eprintln!("This field is required.");
    }
}

fn plain_prompt_confirm(
    reader: &mut impl BufRead,
    prompt_text: &str,
    default: bool,
) -> color_eyre::Result<bool> {
    let def_str = if default { "Y/n" } else { "y/N" };
    let stderr = std::io::stderr();
    let mut stderr = stderr.lock();
    write!(stderr, "{prompt_text} [{def_str}]: ")?;
    stderr.flush()?;

    let mut line = String::new();
    reader.read_line(&mut line)?;
    let line = line.trim().to_lowercase();

    if line.is_empty() {
        return Ok(default);
    }

    match line.as_str() {
        "y" | "yes" => Ok(true),
        "n" | "no" => Ok(false),
        _ => Ok(default),
    }
}

fn run_piped(dir: &Path, reader: &mut impl BufRead) -> color_eyre::Result<()> {
    let config_path = dir.join("pm3.toml");

    if config_path.exists() {
        let overwrite = plain_prompt_confirm(reader, "pm3.toml already exists. Overwrite?", false)?;
        if !overwrite {
            bail!("aborted");
        }
    }

    let mut processes: Vec<InitProcess> = Vec::new();

    loop {
        let name = plain_prompt_required(reader, "Process name")?;
        let command = plain_prompt_required(reader, "Command")?;

        let cwd = plain_prompt(reader, "Working directory", Some(""))?;
        let cwd = if cwd.is_empty() { None } else { Some(cwd) };

        let env_input = plain_prompt(reader, "Environment variables", Some(""))?;
        let env = if env_input.is_empty() {
            Vec::new()
        } else {
            parse_env_pairs(&env_input).map_err(|e| eyre!(e))?
        };

        let restart = loop {
            let value = plain_prompt(
                reader,
                "Restart policy (on_failure/always/never)",
                Some("on_failure"),
            )?;
            match value.as_str() {
                "on_failure" | "always" | "never" => break value,
                _ => eprintln!("Must be one of: on_failure, always, never"),
            }
        };
        let restart = Some(restart);

        let health_check = plain_prompt(reader, "Health check URL", Some(""))?;
        let health_check = if health_check.is_empty() {
            None
        } else {
            Some(health_check)
        };

        let group = plain_prompt(reader, "Group name", Some(""))?;
        let group = if group.is_empty() { None } else { Some(group) };

        let existing_names: Vec<&str> = processes.iter().map(|p| p.name.as_str()).collect();
        let deps_input: String = if existing_names.is_empty() {
            String::new()
        } else {
            plain_prompt(
                reader,
                &format!(
                    "Dependencies (comma-separated, available: {})",
                    existing_names.join(", ")
                ),
                Some(""),
            )?
        };
        let depends_on: Vec<String> = if deps_input.is_empty() {
            Vec::new()
        } else {
            deps_input
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        };

        processes.push(InitProcess {
            name,
            command,
            cwd,
            env,
            restart,
            readiness_check: None,
            health_check,
            group,
            depends_on,
        });

        let add_another = plain_prompt_confirm(reader, "Add another process?", false)?;
        if !add_another {
            break;
        }
    }

    finalize(dir, &processes)?;
    println!("Created {}", config_path.display());

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_toml_single_process() {
        let processes = vec![InitProcess {
            name: "web".to_string(),
            command: "node server.js".to_string(),
            cwd: None,
            env: Vec::new(),
            restart: None,
            readiness_check: None,
            health_check: None,
            group: None,
            depends_on: Vec::new(),
        }];
        let toml = generate_toml(&processes);
        assert!(toml.contains("[web]"));
        assert!(toml.contains("command = \"node server.js\""));
        assert!(!toml.contains("cwd"));
        assert!(!toml.contains("env"));
        assert!(!toml.contains("restart"));
        assert!(!toml.contains("readiness_check"));
        assert!(!toml.contains("health_check"));
        assert!(!toml.contains("group"));
        assert!(!toml.contains("depends_on"));
    }

    #[test]
    fn test_generate_toml_multiple_processes() {
        let processes = vec![
            InitProcess {
                name: "web".to_string(),
                command: "node server.js".to_string(),
                cwd: None,
                env: Vec::new(),
                restart: None,
                readiness_check: None,
                health_check: None,
                group: None,
                depends_on: Vec::new(),
            },
            InitProcess {
                name: "worker".to_string(),
                command: "python worker.py".to_string(),
                cwd: None,
                env: Vec::new(),
                restart: None,
                readiness_check: None,
                health_check: None,
                group: None,
                depends_on: Vec::new(),
            },
        ];
        let toml = generate_toml(&processes);
        assert!(toml.contains("[web]"));
        assert!(toml.contains("[worker]"));
        assert!(toml.contains("\n\n[worker]"));
    }

    #[test]
    fn test_generate_toml_all_fields() {
        let processes = vec![InitProcess {
            name: "web".to_string(),
            command: "node server.js".to_string(),
            cwd: Some("./frontend".to_string()),
            env: vec![("PORT".to_string(), "3000".to_string())],
            restart: Some("on_failure".to_string()),
            readiness_check: Some("tcp://localhost:3000".to_string()),
            health_check: Some("http://localhost:3000/health".to_string()),
            group: Some("backend".to_string()),
            depends_on: vec!["db".to_string()],
        }];
        let toml = generate_toml(&processes);
        assert!(toml.contains("[web]"));
        assert!(toml.contains("command = \"node server.js\""));
        assert!(toml.contains("cwd = \"./frontend\""));
        assert!(toml.contains("env = { PORT = \"3000\" }"));
        assert!(toml.contains("restart = \"on_failure\""));
        assert!(toml.contains("readiness_check = \"tcp://localhost:3000\""));
        assert!(toml.contains("health_check = \"http://localhost:3000/health\""));
        assert!(toml.contains("group = \"backend\""));
        assert!(toml.contains("depends_on = [\"db\"]"));
    }

    #[test]
    fn test_generate_toml_roundtrips() {
        let processes = vec![
            InitProcess {
                name: "web".to_string(),
                command: "node server.js".to_string(),
                cwd: Some("./frontend".to_string()),
                env: vec![("PORT".to_string(), "3000".to_string())],
                restart: Some("on_failure".to_string()),
                readiness_check: Some("tcp://localhost:3000".to_string()),
                health_check: Some("http://localhost:3000/health".to_string()),
                group: Some("backend".to_string()),
                depends_on: vec!["db".to_string()],
            },
            InitProcess {
                name: "db".to_string(),
                command: "postgres -D /data".to_string(),
                cwd: None,
                env: Vec::new(),
                restart: Some("always".to_string()),
                readiness_check: None,
                health_check: None,
                group: None,
                depends_on: Vec::new(),
            },
        ];
        let toml = generate_toml(&processes);
        let configs = crate::config::parse_config(&toml).expect("generated TOML should parse");
        assert_eq!(configs.len(), 2);
        assert!(configs.contains_key("web"));
        assert!(configs.contains_key("db"));
        assert_eq!(configs["web"].command, "node server.js");
        assert_eq!(configs["db"].command, "postgres -D /data");
    }

    #[test]
    fn test_parse_env_pairs() {
        let pairs = parse_env_pairs("PORT=3000,HOST=localhost").unwrap();
        assert_eq!(pairs.len(), 2);
        assert_eq!(pairs[0], ("PORT".to_string(), "3000".to_string()));
        assert_eq!(pairs[1], ("HOST".to_string(), "localhost".to_string()));
    }

    #[test]
    fn test_parse_env_pairs_empty() {
        let pairs = parse_env_pairs("").unwrap();
        assert!(pairs.is_empty());
    }

    #[test]
    fn test_parse_env_pairs_invalid() {
        assert!(parse_env_pairs("NOEQUALS").is_err());
    }
}
