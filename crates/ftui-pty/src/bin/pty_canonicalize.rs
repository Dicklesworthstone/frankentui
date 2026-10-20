use ftui_pty::virtual_terminal::{QuirkSet, VirtualTerminal};
use std::env;
use std::error::Error;
use std::fs;
use std::path::PathBuf;

struct Config {
    input: PathBuf,
    output: PathBuf,
    cols: u16,
    rows: u16,
    quirks: QuirkSet,
    scrollback: bool,
    max_scrollback: usize,
}

fn print_usage() {
    eprintln!(
        "Usage: pty_canonicalize --input <file> --output <file> --cols <n> --rows <n> [--scrollback] [--max-scrollback <n>] [--profile <name>] [--quirk <name>]\n\
         \n\
         Profiles:\n\
           none | tmux_nested | screen | windows_console\n\
         Quirks (repeatable):\n\
           tmux_nested_cursor | screen_immediate_wrap | windows_no_alt_screen\n\
         \n\
         Example:\n\
           pty_canonicalize --input /tmp/run.pty --output /tmp/run.canonical.txt --cols 80 --rows 24 --profile screen"
    );
}

fn parse_profile(name: &str) -> Result<QuirkSet, String> {
    match name {
        "none" => Ok(QuirkSet::empty()),
        "tmux_nested" | "tmux" => Ok(QuirkSet::tmux_nested()),
        "screen" | "gnu_screen" => Ok(QuirkSet::gnu_screen()),
        "windows" | "windows_console" => Ok(QuirkSet::windows_console()),
        _ => Err(format!("unknown --profile value: {name}")),
    }
}

fn apply_quirk(quirks: QuirkSet, name: &str) -> Result<QuirkSet, String> {
    match name {
        "tmux_nested_cursor" | "tmux_nested" => Ok(quirks.with_tmux_nested_cursor(true)),
        "screen_immediate_wrap" | "screen_wrap" => Ok(quirks.with_screen_immediate_wrap(true)),
        "windows_no_alt_screen" | "windows_console" => Ok(quirks.with_windows_no_alt_screen(true)),
        _ => Err(format!("unknown --quirk value: {name}")),
    }
}

fn parse_args() -> Result<Config, String> {
    let mut args = env::args().skip(1);
    let mut input: Option<PathBuf> = None;
    let mut output: Option<PathBuf> = None;
    let mut cols: Option<u16> = None;
    let mut rows: Option<u16> = None;
    let mut quirks = QuirkSet::empty();
    let mut scrollback = false;
    let mut max_scrollback: Option<usize> = None;
    let mut positional: Vec<String> = Vec::new();

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--input" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--input requires a value".to_string())?;
                input = Some(PathBuf::from(value));
            }
            "--output" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--output requires a value".to_string())?;
                output = Some(PathBuf::from(value));
            }
            "--cols" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--cols requires a value".to_string())?;
                cols = Some(
                    value
                        .parse::<u16>()
                        .map_err(|_| "invalid --cols value".to_string())?,
                );
            }
            "--rows" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--rows requires a value".to_string())?;
                rows = Some(
                    value
                        .parse::<u16>()
                        .map_err(|_| "invalid --rows value".to_string())?,
                );
            }
            "--scrollback" => {
                scrollback = true;
            }
            "--max-scrollback" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--max-scrollback requires a value".to_string())?;
                max_scrollback = Some(
                    value
                        .parse::<usize>()
                        .map_err(|_| "invalid --max-scrollback value".to_string())?,
                );
            }
            "--profile" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--profile requires a value".to_string())?;
                quirks = parse_profile(&value)?;
            }
            "--quirk" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--quirk requires a value".to_string())?;
                quirks = apply_quirk(quirks, &value)?;
            }
            "-h" | "--help" => {
                print_usage();
                std::process::exit(0);
            }
            _ => positional.push(arg),
        }
    }

    if input.is_none() && positional.len() == 4 {
        input = Some(PathBuf::from(&positional[0]));
        output = Some(PathBuf::from(&positional[1]));
        cols = Some(
            positional[2]
                .parse::<u16>()
                .map_err(|_| "invalid cols positional value".to_string())?,
        );
        rows = Some(
            positional[3]
                .parse::<u16>()
                .map_err(|_| "invalid rows positional value".to_string())?,
        );
    }

    let input = input.ok_or_else(|| "missing --input".to_string())?;
    let output = output.ok_or_else(|| "missing --output".to_string())?;
    let cols = cols.ok_or_else(|| "missing --cols".to_string())?;
    let rows = rows.ok_or_else(|| "missing --rows".to_string())?;

    if cols == 0 || rows == 0 {
        return Err("cols/rows must be > 0".to_string());
    }

    let max_scrollback = max_scrollback.unwrap_or(if scrollback { 20_000 } else { 1_000 });

    Ok(Config {
        input,
        output,
        cols,
        rows,
        quirks,
        scrollback,
        max_scrollback,
    })
}

fn render_canonical(
    bytes: &[u8],
    cols: u16,
    rows: u16,
    quirks: QuirkSet,
    include_scrollback: bool,
    max_scrollback: usize,
) -> String {
    let mut vt = VirtualTerminal::with_quirks(cols, rows, quirks);
    vt.set_max_scrollback(max_scrollback);
    vt.feed(bytes);
    if include_scrollback && vt.scrollback_len() > 0 {
        let mut lines = Vec::with_capacity(vt.scrollback_len() + usize::from(rows));
        for idx in 0..vt.scrollback_len() {
            if let Some(line) = vt.scrollback_line(idx) {
                lines.push(line);
            }
        }
        lines.push(vt.screen_text());
        lines.join("\n")
    } else {
        vt.screen_text()
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let cfg = parse_args().inspect_err(|_| {
        print_usage();
    })?;

    let bytes = fs::read(&cfg.input)?;
    let text = render_canonical(
        &bytes,
        cfg.cols,
        cfg.rows,
        cfg.quirks,
        cfg.scrollback,
        cfg.max_scrollback,
    );
    fs::write(&cfg.output, text)?;
    Ok(())
}

fn main() {
    if let Err(err) = run() {
        eprintln!("pty_canonicalize error: {err}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrollback_mode_includes_scrolled_lines_before_screen_text() {
        let stream = b"L001\r\nL002\r\nL003\r\nL004\r\nL005\r\n";
        let screen_only = render_canonical(stream, 20, 3, QuirkSet::empty(), false, 100);
        assert!(!screen_only.contains("L001"));
        let with_scrollback = render_canonical(stream, 20, 3, QuirkSet::empty(), true, 100);
        for tok in ["L001", "L002", "L003", "L004", "L005"] {
            assert!(
                with_scrollback.contains(tok),
                "expected {tok} in scrollback output: {with_scrollback:?}"
            );
        }
    }
}
