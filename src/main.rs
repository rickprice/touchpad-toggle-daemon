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
use log::{debug, error, info, trace};
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
///
/// `touchpad_ancestor` is the sysfs `inputN` directory that parents all of
/// the physical touchpad's sibling nodes (event, mouse, …). Any input node
/// whose syspath falls under that prefix is excluded so that the touchpad's
/// own relative-mouse node (mouseN) — which carries `ID_INPUT_MOUSE=1` but
/// not necessarily `ID_INPUT_TOUCHPAD=1` — is never counted as an external
/// mouse.
fn is_external_mouse(device: &udev::Device, touchpad_ancestor: Option<&Path>) -> bool {
    let sysname = device.sysname().to_string_lossy();
    let has_devnode = device.devnode().is_some();
    let id_input_mouse = device
        .property_value("ID_INPUT_MOUSE")
        .and_then(|v| v.to_str());
    let id_input_touchpad = device
        .property_value("ID_INPUT_TOUCHPAD")
        .and_then(|v| v.to_str());
    // Devices whose sysfs path sits under /devices/virtual/ are created by
    // software (uinput), not physical hardware — keyd, XTEST, etc.  They must
    // not be counted as external mice even when udev sets ID_INPUT_MOUSE=1.
    let is_virtual = device
        .devpath()
        .to_string_lossy()
        .starts_with("/devices/virtual/");
    // Sibling nodes of the physical touchpad share the same inputN ancestor.
    let is_touchpad_sibling = touchpad_ancestor
        .map(|a| device.syspath().starts_with(a))
        .unwrap_or(false);
    let result = !is_touchpad_sibling
        && is_mouse_event_device(has_devnode, id_input_mouse, id_input_touchpad, is_virtual);
    debug!(
        "is_external_mouse({sysname}): has_devnode={has_devnode}, \
         ID_INPUT_MOUSE={id_input_mouse:?}, ID_INPUT_TOUCHPAD={id_input_touchpad:?}, \
         is_virtual={is_virtual}, is_touchpad_sibling={is_touchpad_sibling} → {result}"
    );
    result
}

/// Walks up the sysfs hierarchy from the device's own path looking for a
/// sibling `power_supply/` directory. On Logitech HID++ receivers the layout
/// is `…/<hid-device>/power_supply/<name>/status`, reached by going up three
/// levels from the event node. Returns `Some(true)` when the battery status is
/// active, `Some(false)` when it exists but is inactive (mouse off / out of
/// range), or `None` when no battery directory is found at all (wired mouse or
/// a receiver that does not expose battery status — treated as always active).
fn read_device_battery_status(syspath: &Path) -> Option<bool> {
    debug!("read_device_battery_status: searching from {:?}", syspath);
    let mut path = syspath.parent()?;
    for depth in 0..6 {
        let ps_dir = path.join("power_supply");
        trace!("  depth={depth}: checking for power_supply at {:?}", ps_dir);
        if ps_dir.is_dir() {
            debug!("  found power_supply directory at {:?}", ps_dir);
            if let Ok(entries) = std::fs::read_dir(&ps_dir) {
                for entry in entries.flatten() {
                    let status_path = entry.path().join("status");
                    match std::fs::read_to_string(&status_path) {
                        Ok(status) => {
                            let active = is_battery_active(&status);
                            debug!(
                                "  battery status file {:?}: {:?} → active={active}",
                                status_path,
                                status.trim()
                            );
                            return Some(active);
                        }
                        Err(e) => {
                            debug!("  could not read {:?}: {e}", status_path);
                        }
                    }
                }
            }
            debug!("  power_supply directory found but no readable status; treating as inactive");
            return Some(false);
        }
        path = path.parent()?;
    }
    debug!("  no power_supply directory found within 6 ancestor levels; assuming wired/unsupported (always active)");
    None
}

/// Returns `true` if the device is battery-active or has no battery at all
/// (wired or unsupported receiver). Returns `false` only when a battery is
/// present and explicitly reports an inactive status.
fn device_battery_active_or_absent(device: &udev::Device) -> bool {
    let result = read_device_battery_status(device.syspath()).unwrap_or(true);
    debug!(
        "device_battery_active_or_absent({}): {result}",
        device.sysname().to_string_lossy()
    );
    result
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
    let status_path = ps_device.syspath().join("status");
    let is_active = match std::fs::read_to_string(&status_path) {
        Ok(s) => {
            let active = is_battery_active(&s);
            debug!(
                "handle_battery_change: battery status at {:?} = {:?} → active={active}",
                status_path,
                s.trim()
            );
            active
        }
        Err(e) => {
            debug!("handle_battery_change: could not read {:?}: {e}; treating as inactive", status_path);
            false
        }
    };

    let hid_path = match hid_syspath_for_power_supply(ps_device) {
        Some(p) => p,
        None => {
            debug!("handle_battery_change: could not derive HID syspath for {:?}; skipping", ps_device.syspath());
            return;
        }
    };
    debug!("handle_battery_change: HID ancestor path = {:?}", hid_path);

    let associated: Vec<PathBuf> = tracked
        .keys()
        .filter(|p| p.starts_with(&hid_path))
        .cloned()
        .collect();

    debug!(
        "handle_battery_change: {} tracked mouse device(s) under this HID receiver",
        associated.len()
    );

    for mouse_path in associated {
        let was_active = tracked[&mouse_path];
        debug!(
            "  mouse {:?}: was_active={was_active}, now is_active={is_active}",
            mouse_path.file_name().unwrap_or_default()
        );
        if was_active == is_active {
            debug!("  no change; skipping");
            continue;
        }
        tracked.insert(mouse_path.clone(), is_active);
        if is_active {
            let should_disable = mice.connect();
            info!(
                "Mouse battery became active ({}); mouse_count={}{}",
                mouse_path.file_name().unwrap_or_default().to_string_lossy(),
                mice.count(),
                if should_disable { "; disabling touchpad" } else { "" }
            );
            if should_disable {
                set_touchpad_enabled(touchpad_name, false);
            }
        } else {
            let should_enable = mice.disconnect();
            info!(
                "Mouse battery became inactive ({}); mouse_count={}{}",
                mouse_path.file_name().unwrap_or_default().to_string_lossy(),
                mice.count(),
                if should_enable { "; re-enabling touchpad" } else { "" }
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

    // Find the sysfs inputN directory that parents all of the touchpad's input
    // nodes (eventN, mouseN, …) so we can exclude them from mouse detection.
    // I2C-HID touchpads set ID_INPUT_TOUCHPAD=1 on their event node, so a
    // property-filtered enumeration reliably locates the right ancestor.
    let touchpad_input_ancestor: Option<PathBuf> = {
        let mut tp_enum = udev::Enumerator::new()?;
        tp_enum.match_subsystem("input")?;
        tp_enum.match_property("ID_INPUT_TOUCHPAD", "1")?;
        tp_enum
            .scan_devices()?
            .filter_map(|dev| dev.syspath().parent().map(Path::to_path_buf))
            .next()
    };
    match touchpad_input_ancestor {
        Some(ref a) => info!(
            "Touchpad input ancestor: {:?}; sibling nodes excluded from mouse count",
            a
        ),
        None => info!(
            "No ID_INPUT_TOUCHPAD=1 device found; touchpad sibling filtering disabled"
        ),
    }

    let mut enumerator = udev::Enumerator::new()?;
    enumerator.match_subsystem("input")?;

    // Track which mouse input devices are currently counted as active.
    // Keyed by syspath so Remove events can be matched without re-querying
    // udev properties, which may be absent during device removal.
    let mut tracked: HashMap<PathBuf, bool> = HashMap::new();
    let mut mice = MouseCounter::new(0);

    info!("Startup: scanning existing input devices via udev enumeration");
    let mut enumerated = 0usize;
    for dev in enumerator.scan_devices()? {
        enumerated += 1;
        trace!(
            "Startup scan: examining {} (syspath={:?})",
            dev.sysname().to_string_lossy(),
            dev.syspath()
        );
        if !is_external_mouse(&dev, touchpad_input_ancestor.as_deref()) {
            continue;
        }
        let active = device_battery_active_or_absent(&dev);
        tracked.insert(dev.syspath().to_path_buf(), active);
        if active {
            mice.connect();
            info!(
                "Startup: external mouse {} is active (syspath={:?}); mouse_count={}",
                dev.sysname().to_string_lossy(),
                dev.syspath(),
                mice.count()
            );
        } else {
            info!(
                "Startup: external mouse {} found but battery inactive (syspath={:?}); not counting",
                dev.sysname().to_string_lossy(),
                dev.syspath()
            );
        }
    }
    info!(
        "Startup scan complete: examined {enumerated} input device(s), \
         {} external mouse device(s) tracked, {} currently active",
        tracked.len(),
        mice.count()
    );
    if mice.count() > 0 {
        info!("Startup: mouse(s) present; disabling touchpad");
        set_touchpad_enabled(touchpad_name, false);
    } else {
        info!("Startup: no active mice; touchpad left enabled");
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
            let event_type = event.event_type();
            let sysname = device.sysname().to_string_lossy().into_owned();
            let subsystem = device
                .subsystem()
                .and_then(|s| s.to_str())
                .unwrap_or("<none>")
                .to_owned();
            debug!(
                "udev event: {:?} subsystem={subsystem} device={sysname}",
                event_type
            );
            match event_type {
                udev::EventType::Add if is_external_mouse(&device, touchpad_input_ancestor.as_deref()) => {
                    let active = device_battery_active_or_absent(&device);
                    let syspath = device.syspath().to_path_buf();
                    tracked.insert(syspath, active);
                    let should_disable = active && mice.connect();
                    info!(
                        "External mouse plugged in: {sysname} (battery_active={active}, mouse_count={}){}",
                        mice.count(),
                        if should_disable { "; disabling touchpad" } else { "" }
                    );
                    if should_disable {
                        set_touchpad_enabled(touchpad_name, false);
                    }
                }
                udev::EventType::Add => {
                    debug!("Add event for non-mouse device {sysname} (subsystem={subsystem}); ignoring");
                }
                udev::EventType::Remove => {
                    // Use the tracked map rather than re-checking udev properties,
                    // which may be gone by the time the Remove event is processed.
                    let syspath = device.syspath().to_path_buf();
                    if let Some(was_active) = tracked.remove(&syspath) {
                        let should_enable = was_active && mice.disconnect();
                        info!(
                            "External mouse unplugged: {sysname} (was_active={was_active}, mouse_count={}){}",
                            mice.count(),
                            if should_enable { "; re-enabling touchpad" } else { "" }
                        );
                        if should_enable {
                            set_touchpad_enabled(touchpad_name, true);
                        }
                    } else {
                        debug!("Remove event for untracked device {sysname} (subsystem={subsystem}); ignoring");
                    }
                }
                udev::EventType::Change
                    if subsystem == "power_supply" =>
                {
                    info!("Battery change event for {sysname}; re-evaluating mouse activity");
                    handle_battery_change(&device, &mut tracked, &mut mice, touchpad_name);
                }
                udev::EventType::Change => {
                    debug!("Change event for non-power_supply device {sysname} (subsystem={subsystem}); ignoring");
                }
                other => {
                    trace!("Unhandled udev event type {other:?} for {sysname}; ignoring");
                }
            }
        }
    }
}
