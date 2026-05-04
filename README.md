# baeru

[![CI](https://github.com/blacknon/baeru/actions/workflows/ci.yml/badge.svg)](https://github.com/blacknon/baeru/actions/workflows/ci.yml)

`baeru` is a Rust wrapper for existing terminal applications and command output.
It adds presentation and interaction layers on top of tools you already use, without patching the target app itself.

Today, the PoC can:

- reveal the initial screen of a TUI with a cinematic startup animation
- rewrite ANSI colors live using YAML themes
- remap keys per command using YAML keymaps
- animate ordinary CLI output inline below the prompt
- switch behavior per command with `baeru.yml`
- experiment with VT100-based live screen rendering and change highlighting

## Demo

### TUI reveal + live color

![baeru htop demo](assets/htop-demo.gif)

### CLI inline animation

![baeru cli demo](assets/cli-demo.gif)

## Concept

`baeru` is built around two ideas:

- `backend`
  Choose the execution path: `tui`, `cli`, or `raw`
- `features`
  Layer behaviors such as `reveal`, `live_color`, `keymap`, and `inline_animation`

That separation makes it easier to say things like:

- `htop` should use `reveal + live_color + keymap`
- `vim` should stay close to passthrough
- `ls` should use CLI animation only

## Quick start

Build and run:

```bash
cargo run -- -- htop
cargo run -- -- lazygit
cargo run -- -- ls -la
```

Explicit examples:

```bash
cargo run -- --mode reveal -- htop
cargo run -- --mode color-live --theme-file examples/themes/jirai-pink.yml -- htop
cargo run -- --mode reveal --theme-file examples/themes/jirai-pink.yml --keymap-file keymaps/htop-vim.yml -- htop
cargo run -- --mode live-render --theme-file examples/themes/jirai-pink.yml -- htop
cargo run -- --backend cli -- ls -la
cargo run -- --backend cli -- git status
```

If `./baeru.yml` exists, it is loaded automatically:

```bash
cargo run -- -- htop
cargo run -- -- ls -la
```

You can still point to a config explicitly:

```bash
cargo run -- --config-file baeru.yml -- htop
```

If no command is specified and stdin is a terminal, `htop` is used as the default PoC target:

```bash
cargo run
```

## Backends

### `tui`

For interactive terminal applications such as `htop` or `lazygit`.

```bash
baeru --backend tui -- htop
baeru --mode reveal -- htop
```

### `cli`

For ordinary commands such as `ls`, `df`, or `git status`.

```bash
baeru --backend cli -- ls -la
baeru --backend cli -- git status
printf 'hello\nworld\n' | baeru --backend cli
```

### `raw`

For cases where you want plain passthrough behavior without effects.

## Features

### `reveal`

Starts the target command in a PTY, captures the initial screen briefly, renders a startup animation, then switches to normal PTY passthrough.

```bash
baeru --mode reveal -- htop
baeru --mode reveal --capture-ms 420 --duration-ms 900 -- htop
```

### `live_color`

Passes PTY output through while rewriting ANSI SGR colors.

```bash
baeru --mode color-live --theme-file examples/themes/jirai-pink.yml -- htop
```

This now supports both:

- gradient-based recoloring
- simple indexed ANSI palette replacement via `palette_map`

### `keymap`

Rewrites key input per command using YAML mapping rules.

### `inline_animation`

Animates CLI output inline below the prompt.

Supported effects:

- `coalesce`
- `sweep`
- `fade`
- `plain`

```bash
baeru --backend cli --effect coalesce -- ls -la
baeru --backend cli --effect sweep -- git status
```

### `live-render` experimental

Rebuilds the target TUI screen from VT100 state, redraws it from `baeru`, and flashes changed cells.

```bash
baeru --mode live-render --theme-file examples/themes/jirai-pink.yml -- htop
```

This is intentionally experimental and much more fragile than `reveal` or `live_color`.

## Configuration

## `baeru.yml`

`baeru` resolves behavior from profiles matched against command name, exact path, and optional `args_prefix`.

```yaml
profiles:
  - name: htop-jirai-vim
    match:
      command: htop
    backend: tui
    features:
      - reveal
      - live_color
      - keymap
    effect: coalesce
    keymap_file: keymaps/htop-vim.yml
    theme_file: examples/themes/jirai-pink.yml
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
    theme_file: themes/matrix-green.yml
```

## Theme YAML

Theme files support the original gradient-based recoloring style:

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

They also support simple palette replacement for indexed ANSI colors:

```yaml
name: gundam-tricolor-htop
default_fg: "#f3f6ff"
default_bg: "#08111f"
force_default: true
palette_map:
  1: "#ff5a5f"
  3: "#ffd84a"
  4: "#3f7dff"
  15: "#ffffff"
background_palette_map:
  4: "#0f214a"
```

This is especially useful for `htop`-style TUI recoloring where preserving rough semantic color roles matters more than luminance mapping.

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

## Themes

There are two theme buckets right now:

- `themes/`
  Built-in style examples that feel close to baseline usage
- `examples/themes/`
  More expressive or experimental sample themes

Current example themes include:

- `jirai-pink`
- `eva-unit-01`
- `eva-unit-01-htop`
- `gundam-tricolor-htop`

## Testing and CI

CI runs on:

- macOS
- Linux
- Windows

The workflow checks:

- `cargo fmt --check`
- `cargo build --locked`
- `cargo clippy --locked --all-targets -- -D warnings`
- `cargo test --locked --quiet`

Locally, the same commands are enough:

```bash
cargo fmt --check
cargo build --locked
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --quiet
```

## Recording demo GIFs

README demo GIFs are generated with [VHS](https://github.com/charmbracelet/vhs).

Tape files live here:

- `examples/vhs/htop-demo.tape`
- `examples/vhs/cli-demo.tape`

Regenerate the current demo assets with:

```bash
vhs examples/vhs/htop-demo.tape
vhs examples/vhs/cli-demo.tape
```

## Known limitations

- This is still a PoC. Terminal restoration and signal handling can be hardened further.
- `reveal` captures a single approximate startup screen. Very unstable startup screens may need `capture_ms` tuning.
- `live-render` is experimental and may flicker or desynchronize on complex TUIs.
- CLI inline animation is still less robust than plain passthrough for some terminals and very large outputs.
- Key remapping is byte-sequence based. Complex keyboard protocols, mouse input, bracketed paste, and emulator-reserved shortcuts need more careful handling.
- Mouse mapping is not implemented yet, though the architecture leaves room for a future `mousemap` layer.
- Command-specific semantic adapters are still future work.

## Notes for CodeX

Concrete implementation notes and follow-up tasks live in [tmp/CODEX_TASKS.md](tmp/CODEX_TASKS.md) and [tmp/BAERU_BACKEND_FEATURES_DESIGN.md](tmp/BAERU_BACKEND_FEATURES_DESIGN.md).
