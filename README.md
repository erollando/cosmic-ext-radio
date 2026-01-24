# RadioWidget (COSMIC panel applet)

COSMIC (Pop!_OS COSMIC) panel applet that plays internet radio via **mpv** and discovers stations via **Radio Browser**.

## Dependencies

- Rust (stable)
- `rustfmt` + `clippy` (recommended): `rustup component add rustfmt clippy`
- `mpv` (required at runtime)
- COSMIC / `libcosmic` development dependencies (provided by Pop!_OS COSMIC SDK or your distro)

## Build

To only build the project:

```sh
just build
```

Or build and install everything:

```sh
just install
```

The install command will build the project and install the binary, desktop entry, and icon in the correct locations for your user.

## Install (user-local)

If you do not have `just`, you can still install manually:

1. Install the binary:

```sh
install -Dm755 target/release/radiowidget ~/.local/bin/radiowidget
```

2. Install the desktop entry (applet):

```sh
install -Dm644 resources/io.github.xinia.RadioWidget.desktop ~/.local/share/applications/io.github.xinia.RadioWidget.desktop
```

3. Install the icon (optional but recommended):

```sh
install -Dm644 resources/icons/hicolor/symbolic/apps/io.github.xinia.RadioWidget-symbolic.svg \
  ~/.local/share/icons/hicolor/symbolic/apps/io.github.xinia.RadioWidget-symbolic.svg
```

4. Restart the COSMIC panel session (or log out/in), then add the applet to the panel.

## Troubleshooting

- **mpv IPC socket errors**: ensure `XDG_RUNTIME_DIR` is set and writable; RadioWidget creates its socket under `$XDG_RUNTIME_DIR/radiowidget/`.
- **No stations / search failures**: Radio Browser mirrors may be down; RadioWidget retries with backoff and rotates mirrors.
- **Nothing plays**: verify the station URL is reachable and `mpv` can play it: `mpv "<url>"`.
- **Config reset**: delete `~/.config/radiowidget/config.toml`.
- **Logs**: run with `RUST_LOG=info` (or `debug`) to troubleshoot.
