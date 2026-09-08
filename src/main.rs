//! Disables the laptop touchpad while at least one external mouse is
//! connected, and re-enables it once the last one is unplugged.
//!
//! Watches udev's "input" subsystem for hotplug events instead of polling,
//! and identifies mice via the `ID_INPUT_MOUSE` udev property rather than
//! name/vendor/product matching. All non-I/O logic lives in `lib.rs`, where
//! it's covered by unit tests.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use clap::Parser;
use log::{error, info};
use nix::errno::Errno;
use nix::poll::{poll, PollFd, PollFlags};
use touchpad_toggle_daemon::{
    find_touchpad_name, is_battery_active, is_mouse_event_device, xinput_action, MouseCounter,
};

/// Disable the touchpad when an external mouse is plugged in, re-enable it when unplugged.
#[derive(Parser, Debug)]
struct Args {
    /// Exact xinput device name of the touchpad to toggle (as shown by `xinput list`).
    /// If omitted, autodetected as the first device whose name contains
    /// "touchpad" (case-insensitive).
    #[arg(long)]
    touchpad_name: Option<String>,
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let args = Args::parse();

    let touchpad_name = args.touchpad_name.or_else(autodetect_touchpad_name).unwrap_or_else(|| {
        error!("Could not autodetect a touchpad via `xinput list`; pass --touchpad-name explicitly");
        std::process::exit(1);
    });
    info!("Using touchpad device: \"{touchpad_name}\"");

    if let Err(e) = run(&touchpad_name) {
        error!("Fatal error: {e}");
        std::process::exit(1);
    }
}

/// Runs `xinput list --name-only` and delegates parsing to `find_touchpad_name`.
fn autodetect_touchpad_name() -> Option<String> {
    let output = match Command::new("xinput")
        .args(["list", "--name-only"])
        .output()
    {
        Ok(o) => o,
        Err(e) => {
            error!("Failed to run `xinput list`: {e}");
            return None;
        }
    };
    if !output.status.success() {
        error!(
            "`xinput list` exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
        return None;
    }
    find_touchpad_name(&String::from_utf8_lossy(&output.stdout))
}

/// Shells out to `xinput enable`/`disable`, logging but not panicking on failure.
fn set_touchpad_enabled(touchpad_name: &str, enabled: bool) {
    let action = xinput_action(enabled);
    match Command::new("xinput")
        .args([action, touchpad_name])
        .status()
    {
        Ok(status) if status.success() => {
            info!(
                "{} touchpad \"{touchpad_name}\"",
                if enabled { "Enabled" } else { "Disabled" }
            );
        }
        Ok(status) => {
            error!("`xinput {action} \"{touchpad_name}\"` exited with {status}");
        }
        Err(e) => {
            error!("Failed to run `xinput {action} \"{touchpad_name}\"` (is xinput on PATH?): {e}");
        }
    }
}

/// Wraps `is_mouse_event_device` with the actual udev property lookup.
fn is_external_mouse(device: &udev::Device) -> bool {
    let id_input_mouse = device
        .property_value("ID_INPUT_MOUSE")
        .and_then(|v| v.to_str());
    is_mouse_event_device(device.devnode().is_some(), id_input_mouse)
}

/// Walks up the sysfs hierarchy from the device's own path looking for a
/// sibling `power_supply/` directory. On Logitech HID++ receivers the layout
/// is `…/<hid-device>/power_supply/<name>/status`, reached by going up three
/// levels from the event node. Returns `Some(true)` when the battery status is
/// active, `Some(false)` when it exists but is inactive (mouse off / out of
/// range), or `None` when no battery directory is found at all (wired mouse or
/// a receiver that does not expose battery status — treated as always active).
fn read_device_battery_status(syspath: &Path) -> Option<bool> {
    let mut path = syspath.parent()?;
    for _ in 0..6 {
        let ps_dir = path.join("power_supply");
        if ps_dir.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&ps_dir) {
                for entry in entries.flatten() {
                    if let Ok(status) = std::fs::read_to_string(entry.path().join("status")) {
                        return Some(is_battery_active(&status));
                    }
                }
            }
            return Some(false);
        }
        path = path.parent()?;
    }
    None
}

/// Returns `true` if the device is battery-active or has no battery at all
/// (wired or unsupported receiver). Returns `false` only when a battery is
/// present and explicitly reports an inactive status.
fn device_battery_active_or_absent(device: &udev::Device) -> bool {
    read_device_battery_status(device.syspath()).unwrap_or(true)
}

/// Strips the `power_supply/<name>` suffix from the power supply's sysfs path
/// to get the HID device directory that is the common ancestor of both the
/// power supply and its associated input event nodes.
fn hid_syspath_for_power_supply(ps_device: &udev::Device) -> Option<PathBuf> {
    ps_device.syspath().parent()?.parent().map(Path::to_path_buf)
}

/// Handles a `power_supply` Change event (mouse turned on or off while the
/// receiver remains plugged in). Re-reads the battery status from sysfs, finds
/// every tracked input device whose sysfs path lives under the same HID device,
/// and adjusts the mouse count when the active/inactive state changes.
fn handle_battery_change(
    ps_device: &udev::Device,
    tracked: &mut HashMap<PathBuf, bool>,
    mice: &mut MouseCounter,
    touchpad_name: &str,
) {
    let is_active = std::fs::read_to_string(ps_device.syspath().join("status"))
        .map(|s| is_battery_active(&s))
        .unwrap_or(false);

    let hid_path = match hid_syspath_for_power_supply(ps_device) {
        Some(p) => p,
        None => return,
    };

    let associated: Vec<PathBuf> = tracked
        .keys()
        .filter(|p| p.starts_with(&hid_path))
        .cloned()
        .collect();

    for mouse_path in associated {
        let was_active = tracked[&mouse_path];
        if was_active == is_active {
            continue;
        }
        tracked.insert(mouse_path.clone(), is_active);
        if is_active {
            let should_disable = mice.connect();
            info!(
                "Mouse battery active ({}); count = {}",
                mouse_path.file_name().unwrap_or_default().to_string_lossy(),
                mice.count()
            );
            if should_disable {
                set_touchpad_enabled(touchpad_name, false);
            }
        } else {
            let should_enable = mice.disconnect();
            info!(
                "Mouse battery inactive ({}); count = {}",
                mouse_path.file_name().unwrap_or_default().to_string_lossy(),
                mice.count()
            );
            if should_enable {
                set_touchpad_enabled(touchpad_name, true);
            }
        }
    }
}

fn run(touchpad_name: &str) -> std::io::Result<()> {
    // Create the monitor socket before enumerating so that any hotplug events
    // that arrive during enumeration are buffered in the socket and processed
    // in the main loop, rather than silently dropped.
    let socket = udev::MonitorBuilder::new()?
        .match_subsystem("input")?
        .match_subsystem("power_supply")?
        .listen()?;

    let mut enumerator = udev::Enumerator::new()?;
    enumerator.match_subsystem("input")?;

    // Track which mouse input devices are currently counted as active.
    // Keyed by syspath so Remove events can be matched without re-querying
    // udev properties, which may be absent during device removal.
    let mut tracked: HashMap<PathBuf, bool> = HashMap::new();
    let mut mice = MouseCounter::new(0);

    for dev in enumerator.scan_devices()? {
        if !is_external_mouse(&dev) {
            continue;
        }
        let active = device_battery_active_or_absent(&dev);
        if active {
            mice.connect();
        }
        tracked.insert(dev.syspath().to_path_buf(), active);
    }

    info!(
        "Startup: {} external mouse device(s) currently active",
        mice.count()
    );
    if mice.count() > 0 {
        set_touchpad_enabled(touchpad_name, false);
    }

    loop {
        let mut poll_fds = [PollFd::new(&socket, PollFlags::POLLIN)];
        loop {
            match poll(&mut poll_fds, -1) {
                Ok(_) => break,
                Err(Errno::EINTR) => {}
                Err(e) => return Err(e.into()),
            }
        }

        for event in socket.iter() {
            let device = event.device();
            match event.event_type() {
                udev::EventType::Add if is_external_mouse(&device) => {
                    let active = device_battery_active_or_absent(&device);
                    let syspath = device.syspath().to_path_buf();
                    tracked.insert(syspath, active);
                    let should_disable = active && mice.connect();
                    info!(
                        "External mouse detected ({}); battery_active={}, count={}",
                        device.sysname().to_string_lossy(),
                        active,
                        mice.count()
                    );
                    if should_disable {
                        set_touchpad_enabled(touchpad_name, false);
                    }
                }
                udev::EventType::Remove => {
                    // Use the tracked map rather than re-checking udev properties,
                    // which may be gone by the time the Remove event is processed.
                    let syspath = device.syspath().to_path_buf();
                    if let Some(was_active) = tracked.remove(&syspath) {
                        let should_enable = was_active && mice.disconnect();
                        info!(
                            "External mouse removed ({}); was_active={}, count={}",
                            device.sysname().to_string_lossy(),
                            was_active,
                            mice.count()
                        );
                        if should_enable {
                            set_touchpad_enabled(touchpad_name, true);
                        }
                    }
                }
                udev::EventType::Change
                    if device.subsystem().and_then(|s| s.to_str()) == Some("power_supply") =>
                {
                    handle_battery_change(&device, &mut tracked, &mut mice, touchpad_name);
                }
                _ => {}
            }
        }
    }
}
