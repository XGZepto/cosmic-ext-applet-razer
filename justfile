name := 'cosmic-ext-applet-razer'
appid := 'io.github.xgzepto.' + name
prefix := '/usr/local'
icondir := prefix / 'share/icons/hicolor/scalable/apps'

default: build

build:
    cargo build --release

test:
    cargo test

# Install binary + panel entry (asks for sudo)
install: build
    sudo install -Dm0755 target/release/{{name}} {{prefix}}/bin/{{name}}
    sudo install -Dm0644 data/{{appid}}.desktop {{prefix}}/share/applications/{{appid}}.desktop
    sudo install -Dm0644 data/icons/{{appid}}-symbolic.svg {{icondir}}/{{appid}}-symbolic.svg
    @echo "Done. Add it in Settings → Desktop → Panel → Configure panel applets."

# Give your user access to the laptop's HID control device (only needed once)
install-udev:
    sudo install -Dm0644 data/70-razer-laptop-hidraw.rules /etc/udev/rules.d/70-razer-laptop-hidraw.rules
    sudo udevadm control --reload-rules
    sudo udevadm trigger --subsystem-match=hidraw
    @echo "udev rule installed."

uninstall:
    sudo rm -f {{prefix}}/bin/{{name}} {{prefix}}/share/applications/{{appid}}.desktop {{icondir}}/{{appid}}-symbolic.svg

# Run from the terminal with logs (useful for debugging)
run: build
    RUST_LOG=cosmic_ext_applet_razer=debug target/release/{{name}}
