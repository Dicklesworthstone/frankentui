# Terminal compatibility matrix

Measured on 2026-09-02 with `scripts/pty_identity_matrix.py` driving the demo showcase under a real pty per terminal identity (100x30, `q` after 2 s). The script plays the terminal: it answers the DECRPM 2026 probe with the status a real terminal of that identity reports, or stays silent. Every row asserts the policy outcome; the same script runs in CI (`emulator_compat_matrix.yml`, job `pty-identity-matrix`) and uploads one JSONL line per run.

Columns: **sync** = DEC 2026 synchronized-output brackets emitted (pairs counted over the run, inline / alt); **scroll region** = DECSTBM set in inline mode; **probe** = DECRPM query sent; **exit** = clean exit on `q` with cursor and paste mode restored.

| Identity (env) | DECRPM answer | sync inline | sync alt | scroll region | probe sent | exit |
|---|---|---|---|---|---|---|
| kitty<br>`TERM=xterm-kitty`, `KITTY_WINDOW_ID=1` | silent | 20/20 | 20/20 | yes | no | ok |
| ghostty<br>`TERM=xterm-ghostty`, `TERM_PROGRAM=ghostty` | silent | 20/20 | 20/20 | yes | no | ok |
| alacritty_term_program<br>`TERM=xterm-256color`, `TERM_PROGRAM=Alacritty` | silent | 20/20 | 20/20 | yes | no | ok |
| alacritty_term_only<br>`TERM=alacritty` | silent | 20/20 | 20/20 | yes | no | ok |
| xterm256_answers_decrpm<br>`TERM=xterm-256color`, `COLORTERM=truecolor` | status 2 | 20/20 | 20/20 | yes | yes | ok |
| iterm2_answers_decrpm<br>`TERM=xterm-256color`, `TERM_PROGRAM=iTerm.app`, `LC_TERMINAL=iTerm2` | status 2 | 20/20 | 20/20 | yes | yes | ok |
| vscode_answers_decrpm<br>`TERM=xterm-256color`, `TERM_PROGRAM=vscode` | status 1 | 20/20 | 20/20 | yes | yes | ok |
| xterm256_rejects_decrpm<br>`TERM=xterm-256color` | status 0 | 0/0 | 0/0 | yes | yes | ok |
| apple_terminal_silent<br>`TERM=xterm-256color`, `TERM_PROGRAM=Apple_Terminal` | silent | 0/0 | 0/0 | yes | yes | ok |
| tmux<br>`TERM=tmux-256color`, `TMUX=/tmp/tmux-1/default,1,0` | status 2 | 0/0 | 0/0 | no | no | ok |
| wezterm_default_policy<br>`TERM=xterm-256color`, `TERM_PROGRAM=WezTerm` | status 2 | 0/0 | 0/0 | no | no | ok |
| wezterm_operator_opt_in<br>`TERM=xterm-256color`, `TERM_PROGRAM=WezTerm`, `FTUI_SYNC_OUTPUT=1`, `FTUI_SCROLL_REGION=1` | silent | 20/20 | 20/20 | yes | no | ok |
| operator_opt_out<br>`TERM=xterm-kitty`, `KITTY_WINDOW_ID=1`, `FTUI_SYNC_OUTPUT=0` | silent | 0/0 | 0/0 | yes | yes | ok |
| operator_opt_out_beats_probe<br>`TERM=xterm-256color`, `FTUI_SYNC_OUTPUT=0` | status 2 | 0/0 | 0/0 | yes | yes | ok |
| probe_disabled<br>`TERM=xterm-256color`, `FTUI_CAPS_PROBE=0` | status 2 | 0/0 | 0/0 | yes | no | ok |

## Policy summary

- **Allowlisted identities** (kitty, Ghostty, Alacritty by `TERM_PROGRAM` or `TERM=alacritty`, Contour) get sync output without a probe.
- **Everything else that answers the DECRPM 2026 query** with status 1, 2, or 3 gets sync output at startup: plain `xterm-256color`, iTerm2 (also recognized via `LC_TERMINAL` over ssh), VS Code, and any terminal reached over ssh. Status 0/4 or silence leaves the conservative default; the probe is bounded by a 300 ms timeout and never downgrades.
- **Multiplexers** (tmux, screen, zellij) and **WezTerm identities** never get sync output or the inline scroll region by policy (WezTerm mux sessions were observed to misbehave around `?2026 h/l`, and the markers that would separate a mux session from a local window do not survive every launch path).
- **Operator switches** win over everything: `FTUI_SYNC_OUTPUT=1|0`, `FTUI_SCROLL_REGION=1|0` (an explicit `1` lifts the WezTerm gate, never real multiplexer evidence), `FTUI_CAPS_PROBE=0` disables startup probing entirely.

Regenerate: build the showcase, then run `scripts/pty_identity_matrix.py --mode inline` and `--mode alt` and paste the JSONL summaries here.
