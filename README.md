# cosmic-ext-applet-razer

A COSMIC panel applet for Razer laptops — the same controls as
[RazerControl / rzrctrl.com](https://github.com/Razer-Linux/RazerControl), without opening Chrome.

It talks to the laptop exactly the way the web app does (HID feature reports over
`/dev/hidraw*`), so no daemon or kernel module is needed.

**Controls**
- Performance: Balanced or Custom, with CPU (Low/Medium/High/Boost) and GPU (Low/Medium/High) levels
- Manual fan speed (2000–5400 rpm)
- Keyboard lighting: Off / Static / Spectrum / Wave / Reactive / Starlight, colour swatches, speed, wave direction
- Logo light on/off, Static/Breathing
- Battery charge limit (50–80 %)

Sections your model doesn't support are hidden automatically.

## Install (Pop!_OS 24.04 / COSMIC)

```bash
sudo apt install cargo just libxkbcommon-dev pkg-config   # or use rustup for a newer Rust
cd cosmic-ext-applet-razer
just install-udev   # once: lets your user open the Razer HID device
just install        # builds and installs to /usr/local
```

Then **Settings → Desktop → Panel → Configure panel applets → Add applet → "Razer Control"**.

If the applet isn't listed, log out and back in (the panel reads applet entries at startup).

> First build takes a while — libcosmic is a big dependency.
> If `cargo` complains the Rust version is too old, install rustup:
> `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`

## Troubleshooting

- **"No permission to open /dev/hidrawN"** — run `just install-udev`, then log out/in.
- **"No supported Razer laptop found"** — your model's USB product ID isn't in the list.
  Check with `lsusb | grep 1532`, add the ID to `LAPTOP_PIDS` in `src/razer.rs`, rebuild.
- **Force a specific device:** `RAZER_HIDRAW=/dev/hidraw3 cosmic-ext-applet-razer`
- **Logs:** `just run` from a terminal (close the panel instance first, or just read the output).

## Notes

- Like the web app, settings are written to the laptop's firmware; Razer laptops keep most of them across reboots.
- Changing CPU/GPU level switches the laptop into Custom mode, as on Windows Synapse.
- Unlike the web app, switching performance level does **not** silently reset a manual fan speed back to auto.
