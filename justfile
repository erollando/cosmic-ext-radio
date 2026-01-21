# Justfile for RadioWidget (COSMIC panel applet)

default: install

# Build and install everything
install:
	cargo build --release
	install -Dm755 target/release/radiowidget ~/.local/bin/radiowidget
	install -Dm644 resources/io.github.xinia.RadioWidget.desktop ~/.local/share/applications/io.github.xinia.RadioWidget.desktop
	install -Dm644 resources/icons/hicolor/symbolic/apps/io.github.xinia.RadioWidget-symbolic.svg \
	  ~/.local/share/icons/hicolor/symbolic/apps/io.github.xinia.RadioWidget-symbolic.svg

# Clean build artifacts
clean:
	cargo clean
