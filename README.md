# touchpad-toggle-daemon

Rust daemon that disables the laptop touchpad while an external mouse is
connected and active, and re-enables it once the last active mouse is gone.

It watches udev's `input` subsystem for hotplug events (no polling),
identifies mice via the `ID_INPUT_MOUSE` udev property (no vendor/product ID
or name-substring matching), tracks a running count so multiple mice plugged
in at once behave correctly, and toggles the touchpad by shelling out to
`xinput enable`/`xinput disable`.

## Battery-aware detection

Wireless mice connected via a USB receiver (e.g. Logitech Unifying or Bolt)
appear in udev as soon as the dongle is plugged in, regardless of whether the
mouse itself is switched on. The daemon handles this by reading the
`power_supply/*/status` file the kernel exposes for HID++ devices:

- `Discharging` / `Charging` / `Full` → mouse is on, touchpad disabled
- `Unknown` → mouse is off or out of range, touchpad left enabled

The daemon also subscribes to `power_supply` change events, so turning the
mouse on or off while the dongle remains plugged in updates the touchpad state
in real time — no need to re-plug the receiver.

Devices with no battery entry (wired mice, or receivers that don't expose
battery status) are always treated as active, preserving the original
behaviour.

## Usage

```
touchpad-toggle-daemon [--touchpad-name <NAME>]
```

`--touchpad-name` must match a device name exactly as `xinput list` reports
it, e.g.:

```
$ xinput list --name-only | grep -i touchpad
SynPS/2 Synaptics TouchPad
```

If omitted, the daemon autodetects the first `xinput` device whose name
contains "touchpad" (case-insensitive).

### Logging

Logs go to stderr with timestamps; `journalctl --user -u touchpad-toggle-daemon`
is useful when run as a systemd unit. `RUST_LOG` controls verbosity:

| Level | What you see |
|-------|-------------|
| `info` (default) | Startup summary, each mouse plug/unplug/battery change, and every touchpad enable/disable action |
| `debug` | Per-device udev property inspection during startup scan and on every event (`has_devnode`, `ID_INPUT_MOUSE`, `ID_INPUT_TOUCHPAD`, battery status file path and raw value, HID ancestor path resolution) |
| `trace` | Every device examined during startup, every sysfs ancestor level checked for `power_supply/`, and every unhandled udev event type |

Set the level at launch:

```
RUST_LOG=debug touchpad-toggle-daemon
```

or persistently in the systemd unit's `[Service]` section:

```ini
Environment=RUST_LOG=debug
```

## Virtual pointer devices (keyd, XTEST)

Software keyboard remappers such as [keyd](https://github.com/rvaiya/keyd) and
the X server's own XTEST infrastructure create virtual pointer devices via
`uinput`. The kernel places all `uinput` devices under
`/sys/devices/virtual/input/`, and udev stamps them with `ID_INPUT_MOUSE=1`
just like real hardware mice.

The daemon filters these out by checking whether a device's sysfs path begins
with `/devices/virtual/` before counting it. Any device under that subtree is
ignored, so keyd's virtual pointer (or any other software-generated pointer)
never triggers a touchpad disable.

## Known limitations

When the daemon is stopped while a mouse is connected, the touchpad remains
disabled until it is restarted or `xinput enable` is run manually. The
`Restart = "on-failure"` systemd setting means the daemon re-evaluates the
connected mice on restart and re-enables the touchpad if none are present.

## Building

```
nix build
```

or plain `cargo build --release` (requires `libudev` headers/pkg-config and
`xinput` on `PATH` at runtime).

## NixOS / Home Manager

This repo is a flake exposing `packages.default`, built with
`buildRustPackage` and wrapped (via `makeWrapper`) so `xinput` is on `PATH`
regardless of the caller's environment.

See [`nix/home-manager-module.nix.example`](nix/home-manager-module.nix.example)
for a `systemd.user.services.touchpad-toggle-daemon` unit to drop into a
Home Manager config: `After`/`PartOf`/`WantedBy` on
`graphical-session.target`, `Restart = "on-failure"`.
