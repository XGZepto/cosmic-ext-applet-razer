# cosmic-ext-applet-razer

A [COSMIC](https://system76.com/cosmic) panel applet for Razer laptops. It controls performance
modes, fan speed, keyboard and logo lighting, and the battery charge limit from the panel, with no
Synapse, browser, or background daemon.

## Features

- **Performance:** Balanced or Custom mode, with CPU (Low / Mid / High / Boost) and GPU (Low / Mid / High) levels
- **Fans:** automatic, or a fixed speed from 2000 to 5400 rpm
- **Keyboard lighting:** Off, Static, Spectrum, Wave, Reactive and Starlight, with colour, speed and wave direction
- **Logo:** on or off, static or breathing
- **Battery:** charge limit from 50 to 80 %
- **Device info:** model, part number, BIOS and EC firmware versions

Each section collapses to a one-line summary of its current settings. Sections your model doesn't
support are hidden.

## How it works

The applet sends the same HID feature reports as the
[RazerControl](https://github.com/Razer-Linux/RazerControl) web app, directly to the laptop's
`/dev/hidraw*` interface. There is no daemon or kernel module, and nothing runs as root: a udev rule
gives the logged-in user access to Razer HID devices.

Settings are stored in the laptop's firmware, so most of them survive a reboot.

## Supported laptops

The applet recognises the same USB product IDs as RazerControl, covering Blade 14, 15, 16 and 17,
Blade Stealth and Book models. The full list is `LAPTOP_PIDS` in [`src/razer.rs`](src/razer.rs).

Tested on:

| Model | Part number | USB ID |
| --- | --- | --- |
| Razer Blade 14 (2021) | RZ09-0370 | `1532:0270` |

If it works on yours, please open an issue or PR so it can be added here.

## Install

You need a COSMIC desktop (for example Pop!_OS 24.04), plus:

- Rust 1.93 or newer. Distribution packages are usually too old, so install it with [rustup](https://rustup.rs):
  ```bash
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
  ```
- `just`, `pkg-config` and the xkbcommon headers:
  ```bash
  sudo apt install just libxkbcommon-dev pkg-config
  ```

Then build and install:

```bash
git clone https://github.com/XGZepto/cosmic-ext-applet-razer
cd cosmic-ext-applet-razer
just install-udev   # once: lets your user talk to the laptop
just install        # builds and installs to /usr/local
```

The first build takes a few minutes because libcosmic is a large dependency.

Finally, add it to the panel: **Settings → Desktop → Panel → Configure panel applets → Add applet →
Razer Control**. If it isn't listed, log out and back in; the panel reads applet entries at startup.

To remove it, run `just uninstall`. The udev rule stays in
`/etc/udev/rules.d/70-razer-laptop-hidraw.rules`; delete that file too if you no longer need it.

## Troubleshooting

- **"No permission to open /dev/hidrawN":** run `just install-udev`, then log out and back in.
- **"No supported Razer laptop found":** your model's USB product ID isn't in the list. See
  [Adding a laptop](#adding-a-laptop).
- **Wrong device picked:** force one with `RAZER_HIDRAW=/dev/hidraw3 cosmic-ext-applet-razer`.
- **Logs:** `just run` starts a copy in the terminal with debug logging. It can run alongside the
  panel's copy.

## Adding a laptop

1. Find your laptop's product ID: `lsusb | grep 1532` prints something like `ID 1532:0270 Razer USA, Ltd`.
2. Add it to `LAPTOP_PIDS` in [`src/razer.rs`](src/razer.rs), then `just install`.
3. If everything works, open a PR with the ID and your model name.

## Notes

- Choosing a CPU or GPU level switches the laptop to Custom mode, as in Synapse.
- Switching performance mode keeps a manual fan speed. The web app resets it to automatic.
- Some models, including the Blade 14 (2021), don't report the wave direction back. The applet
  remembers the last direction it set in `~/.local/state/cosmic-ext-applet-razer/`.
- The panel runs one copy of the applet per monitor. They share the laptop's control channel and
  take turns through a lock file in `$XDG_RUNTIME_DIR`.

## Credits

- [RazerControl](https://github.com/Razer-Linux/RazerControl), the web app whose protocol and device
  list this applet follows.
- [OpenRazer](https://github.com/openrazer/openrazer), for documenting the Razer HID protocol.

This project is not affiliated with or endorsed by Razer Inc. "Razer" and "Blade" are trademarks of
Razer Inc. The applet changes firmware settings such as fan speed and power limits; use it at your
own risk.

## License

[GPL-3.0-only](LICENSE)
