# touchpad-toggle-daemon

Rust daemon that disables the laptop touchpad while an external mouse is
connected, and re-enables it once the last one is unplugged.

It watches udev's `input` subsystem for hotplug events (no polling),
identifies mice via the `ID_INPUT_MOUSE` udev property (no vendor/product ID
or name-substring matching), tracks a running count so multiple mice plugged
in at once behave correctly, and toggles the touchpad by shelling out to
`xinput enable`/`xinput disable`.

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

Logs go to stderr with timestamps (`RUST_LOG` controls verbosity, default
`info`), so `journalctl --user -u touchpad-toggle-daemon` is useful when run
as a systemd unit.

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
