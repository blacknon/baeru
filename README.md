# baeru

Make existing terminal apps **baeru** — cinematic reveals, live ANSI themes, and per-command keymaps without patching the app.

`baeru` is a wrapper for existing terminal applications and command output. It can:

- reveal the initial screen of a TUI with a coalesce-style animation
- recolor ANSI SGR output on the fly using YAML themes
- remap keys per command using YAML keymaps
- animate ordinary CLI output inline below the prompt
- apply command-specific profiles from `baeru.yml`
- experimentally flash updated cells in a VT100-rebuilt live screen

This is a PoC intended as a base for further work with CodeX.

## Quick start

```bash
cargo run -- --mode reveal -- htop
cargo run -- --mode reveal --theme-file examples/themes/jirai-pink.yml -- htop
cargo run -- --mode color-live --theme-file examples/themes/jirai-pink.yml -- htop
cargo run -- --mode reveal --theme-file examples/themes/jirai-pink.yml --keymap-file keymaps/htop-vim.yml -- htop
cargo run -- --mode live-render --theme-file examples/themes/jirai-pink.yml -- htop
cargo run -- --backend cli -- ls -la
cargo run -- --backend cli -- git status
```

Using profile config:

```bash
cargo run -- -- htop
cargo run -- -- lazygit
cargo run -- -- ls -la
```

If `./baeru.yml` exists, it is loaded automatically. You can still override it explicitly:

```bash
cargo run -- --config-file baeru.yml -- htop
```

If no command is specified and stdin is a terminal, `htop` is used as a default PoC target:

```bash
cargo run
```

## Backends

`baeru` has two main execution paths.

### `tui`

Use this for interactive terminal applications such as `htop` or `lazygit`.

```bash
baeru --backend tui -- htop
baeru --mode reveal -- htop
```

### `cli`

Use this for ordinary commands such as `ls`, `df`, or `git status`.

```bash
baeru --backend cli -- ls -la
baeru --backend cli -- git status
printf 'hello\nworld\n' | baeru --backend cli
```

If you rely on profiles, `backend` is usually selected from `baeru.yml`.

## Modes / Features

### `reveal`

Starts the target command in a PTY, captures the initial terminal screen for a short period, renders a coalesce-style reveal animation, then switches to normal PTY passthrough.

```bash
baeru --mode reveal -- htop
baeru --mode reveal --capture-ms 420 --duration-ms 900 -- htop
```

### `color-live`

Does not rebuild the screen. It simply passes the target PTY output through while rewriting ANSI SGR color sequences.

```bash
baeru --mode color-live --theme-file examples/themes/jirai-pink.yml -- htop
```

### `splash`

Shows a simple startup splash, then starts the command normally.

```bash
baeru --mode splash -- htop
```

### `live-render` experimental

Rebuilds the target TUI screen from VT100 state, redraws it from baeru, and briefly flashes cells that changed since the previous frame.

```bash
baeru --mode live-render --theme-file examples/themes/jirai-pink.yml -- htop
```

This is intentionally experimental. Unlike `color-live`, it does not simply pass the target output through. It parses terminal output, maintains a screen buffer, compares frames, and redraws the whole screen. This makes it useful for future advanced effects, but it is much more fragile than `reveal` or `color-live`.

### CLI effects

For CLI backend output animation, the current implementation supports:

- `coalesce`
- `sweep`
- `fade`
- `plain`

Example:

```bash
baeru --backend cli --effect coalesce -- ls -la
baeru --backend cli --effect sweep -- git status
```


## Theme YAML

```yaml
name: jirai-pink
default_fg: "#ffcdeb"
default_bg: "#120018"
force_default: true
foreground:
  - { at: 0.00, color: "#84205c" }
  - { at: 0.30, color: "#ff45ac" }
  - { at: 0.62, color: "#ff8fd6" }
  - { at: 0.84, color: "#ffcdeb" }
  - { at: 1.00, color: "#fff2fa" }
background:
  - { at: 0.00, color: "#120018" }
  - { at: 0.55, color: "#570a41" }
  - { at: 1.00, color: "#ff8fcf" }
```

The current implementation maps source colors to the configured foreground/background gradients by luminance. This keeps the schema small while still changing the whole mood of existing TUIs.

## Keymap YAML

```yaml
keymap:
  j: down
  k: up
  h: left
  l: right
  ctrl-d: page-down
  ctrl-u: page-up
  r: f5
  "/": f3
```

Supported key names include:

- arrows: `up`, `down`, `left`, `right`
- navigation: `home`, `end`, `page-up`, `page-down`
- function keys: `f1` ... `f10`
- control keys: `ctrl-a` ... `ctrl-z`
- special keys: `enter`, `esc`, `tab`, `backspace`
- single printable characters such as `j`, `k`, `/`

## Profile config

```yaml
profiles:
  - name: htop-jirai-vim
    match:
      command: htop
      # path: /opt/homebrew/bin/htop
    backend: tui
    features:
      - reveal
      - live_color
      - keymap
    effect: coalesce
    keymap_file: keymaps/htop-vim.yml
    capture_ms: 360
    duration_ms: 720
    frames: 24

  - name: ls-inline
    match:
      command: ls
    backend: cli
    features:
      - inline_animation
    effect: coalesce
```

Profile matching currently checks the executable basename, exact path, and optional `args_prefix`.

## Known limitations

- This is a PoC; terminal restoration and signal handling should be hardened.
- `reveal` captures a single approximate initial screen. Applications with unstable startup screens may need `--capture-ms` tuning.
- `color-live` rewrites SGR colors only. It intentionally avoids rebuilding the whole screen.
- `live-render` is experimental and may flicker or desynchronize on complex TUIs.
- CLI inline animation is still a PoC. Multi-line output behavior depends on terminal behavior and is less robust than plain passthrough.
- Key remapping is byte-sequence based. Complex keyboard protocols, mouse input, bracketed paste, and terminal-emulator-reserved shortcuts need more careful handling.
- Mouse mapping is not implemented yet, but the architecture leaves room for a future `mousemap` layer.
- Command-specific semantic adapters, such as an `htop` adapter that understands CPU bars/process rows/status areas, are intentionally future work.

## Suggested next tasks

See [tmp/CODEX_TASKS.md](tmp/CODEX_TASKS.md) for concrete tasks to hand to CodeX.
