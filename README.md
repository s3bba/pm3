# pm3

A Rust-based process manager. Define processes in `pm3.toml`, manage them with simple commands.

## Usage

Create a `pm3.toml` in your project directory:

```toml
[web]
command = "node server.js"
cwd = "./frontend"
env = { PORT = "3000" }

[worker]
command = "python worker.py"
restart = "on-failure"
max_restarts = 10
```

Then manage your processes:

```sh
pm3 start           # start all processes
pm3 start web       # start one by name
pm3 stop [name]     # stop all or one
pm3 restart [name]  # restart all or one
pm3 list            # show process table
pm3 log [name]      # view logs
pm3 kill            # stop everything and shut down the daemon
```

## Install

```sh
cargo install --path .
```

## License

MIT
