use std::env;
use std::io;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use ftui_pty::ws_bridge::{WsPtyBridgeConfig, run_ws_pty_bridge};

fn main() -> io::Result<()> {
    let config = parse_args(env::args().skip(1))?;
    run_ws_pty_bridge(config)
}

fn parse_args<I>(args: I) -> io::Result<WsPtyBridgeConfig>
where
    I: IntoIterator<Item = String>,
{
    let mut config = WsPtyBridgeConfig::default();
    let mut no_auth = false;
    let mut iter = args.into_iter();

    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                print_help();
                std::process::exit(0);
            }
            "--bind" => {
                let value = next_value(&mut iter, "--bind")?;
                config.bind_addr = parse_socket_addr(&value)?;
            }
            "--cmd" => {
                config.command = next_value(&mut iter, "--cmd")?;
            }
            "--arg" => {
                config.args.push(next_value(&mut iter, "--arg")?);
            }
            "--cols" => {
                let value = next_value(&mut iter, "--cols")?;
                config.cols = parse_u16(&value, "--cols")?;
            }
            "--rows" => {
                let value = next_value(&mut iter, "--rows")?;
                config.rows = parse_u16(&value, "--rows")?;
            }
            "--term" => {
                config.term = next_value(&mut iter, "--term")?;
            }
            "--env" => {
                let pair = next_value(&mut iter, "--env")?;
                let (key, value) = parse_env_pair(&pair)?;
                config.env.push((key, value));
            }
            "--origin" => {
                config
                    .allowed_origins
                    .push(next_value(&mut iter, "--origin")?);
            }
            "--token" => {
                config.auth_token = Some(next_value(&mut iter, "--token")?);
            }
            "--no-auth" => {
                no_auth = true;
            }
            "--telemetry" => {
                config.telemetry_path = Some(PathBuf::from(next_value(&mut iter, "--telemetry")?));
            }
            "--max-message-bytes" => {
                let value = next_value(&mut iter, "--max-message-bytes")?;
                config.max_message_bytes = parse_usize(&value, "--max-message-bytes")?;
            }
            "--idle-ms" => {
                let value = next_value(&mut iter, "--idle-ms")?;
                config.idle_sleep = Duration::from_millis(parse_u64(&value, "--idle-ms")?);
            }
            "--serve-forever" => {
                config.accept_once = false;
            }
            "--accept-once" => {
                config.accept_once = true;
            }
            unknown => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("unknown argument: {unknown}"),
                ));
            }
        }
    }

    if config.cols == 0 || config.rows == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "cols/rows must be > 0",
        ));
    }

    // With neither check, any page open in the operator's browser could drive
    // the shell (see Security in --help). Running that way has to be asked for.
    let checked = !config.allowed_origins.is_empty() || config.auth_token.is_some();
    if checked == no_auth {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            if no_auth {
                "--no-auth contradicts --origin and --token"
            } else {
                "refusing to serve a shell to any client: pass --origin, --token, \
                 or --no-auth to accept every connection"
            },
        ));
    }

    Ok(config)
}

fn next_value<I>(iter: &mut I, flag: &str) -> io::Result<String>
where
    I: Iterator<Item = String>,
{
    iter.next().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("missing value for {flag}"),
        )
    })
}

fn parse_socket_addr(value: &str) -> io::Result<SocketAddr> {
    value.parse().map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid --bind value `{value}`: {error}"),
        )
    })
}

fn parse_u16(value: &str, flag: &str) -> io::Result<u16> {
    value.parse().map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid {flag} value `{value}`: {error}"),
        )
    })
}

fn parse_u64(value: &str, flag: &str) -> io::Result<u64> {
    value.parse().map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid {flag} value `{value}`: {error}"),
        )
    })
}

fn parse_usize(value: &str, flag: &str) -> io::Result<usize> {
    value.parse().map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid {flag} value `{value}`: {error}"),
        )
    })
}

fn parse_env_pair(pair: &str) -> io::Result<(String, String)> {
    let mut pieces = pair.splitn(2, '=');
    let key = pieces.next().unwrap_or_default();
    let value = pieces.next().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid --env pair `{pair}`, expected KEY=VALUE"),
        )
    })?;
    if key.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid --env pair `{pair}`, key must be non-empty"),
        ));
    }
    Ok((key.to_string(), value.to_string()))
}

fn print_help() {
    println!(
        "\
frankenterm_ws_bridge

Usage:
  frankenterm_ws_bridge [options]

Options:
  --bind <addr>                Bind address (default: 127.0.0.1:9231)
  --cmd <path>                 Command to spawn in PTY (default: $SHELL or /bin/sh)
  --arg <value>                Command arg (repeatable)
  --cols <n>                   Initial columns (default: 120)
  --rows <n>                   Initial rows (default: 40)
  --term <value>               TERM for child (default: xterm-256color)
  --env <KEY=VALUE>            Extra environment variable (repeatable)
  --origin <url>               Allowed Origin header value (repeatable)
  --token <secret>             Require ?token=<secret> on websocket URI
                               (compared undecoded; use only A-Z a-z 0-9 - . _ ~)
  --no-auth                    Accept every connection (see Security)
  --telemetry <path>           Append JSONL telemetry at path
  --max-message-bytes <n>      Max websocket frame/message size
  --idle-ms <n>                Idle loop sleep (default: 5 ms)
  --accept-once                Handle one client then exit (default)
  --serve-forever              Accept clients continuously
  -h, --help                   Show this help

Security:
  The bridge refuses to start without --origin, --token, or --no-auth.
  It hands a client the stdin and stdout of a real shell, and a websocket
  handshake is not subject to the same-origin policy, so with no checks any
  page visited in a browser can open ws://127.0.0.1:9231/ and drive that
  shell. Binding loopback does not prevent it - the browser is already
  inside it. Use --no-auth only for a throwaway session.
  A --token value is visible to other local users in the process list.
"
    );
}

#[cfg(test)]
mod tests {
    use super::parse_args;

    fn parse(args: &[&str]) -> std::io::Result<ftui_pty::ws_bridge::WsPtyBridgeConfig> {
        parse_args(args.iter().map(|arg| (*arg).to_string()))
    }

    #[test]
    fn an_open_bridge_has_to_be_asked_for() {
        assert!(parse(&[]).is_err());
        assert!(parse(&["--cmd", "cat"]).is_err());
        let open = parse(&["--no-auth"]).unwrap();
        assert!(open.allowed_origins.is_empty() && open.auth_token.is_none());
    }

    #[test]
    fn either_check_is_enough_and_contradicts_no_auth() {
        assert!(parse(&["--token", "t0k3n"]).is_ok());
        assert!(parse(&["--origin", "http://localhost:3000"]).is_ok());
        assert!(parse(&["--no-auth", "--token", "t0k3n"]).is_err());
        assert!(parse(&["--origin", "http://localhost:3000", "--no-auth"]).is_err());
    }
}
