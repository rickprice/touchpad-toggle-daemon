//! Disables the laptop touchpad while at least one external mouse is
//! connected, and re-enables it once the last one is unplugged.
//!
//! Watches udev's "input" subsystem for hotplug events instead of polling,
//! and identifies mice via the `ID_INPUT_MOUSE` udev property rather than
//! name/vendor/product matching. All non-I/O logic lives in `lib.rs`, where
//! it's covered by unit tests.

use std::process::Command;

use clap::Parser;
use log::{error, info};
use nix::poll::{poll, PollFd, PollFlags};
use touchpad_toggle_daemon::{
    find_touchpad_name, is_mouse_event_device, xinput_action, MouseCounter,
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

fn run(touchpad_name: &str) -> std::io::Result<()> {
    let mut enumerator = udev::Enumerator::new()?;
    enumerator.match_subsystem("input")?;
    let initial = enumerator.scan_devices()?.filter(is_external_mouse).count();
    let mut mice = MouseCounter::new(initial);
    info!(
        "Startup: {} external mouse device(s) currently connected",
        mice.count()
    );
    if mice.count() > 0 {
        set_touchpad_enabled(touchpad_name, false);
    }

    let socket = udev::MonitorBuilder::new()?
        .match_subsystem("input")?
        .listen()?;

    loop {
        // Block until the monitor socket has an event ready, rather than polling in a busy loop.
        let mut poll_fds = [PollFd::new(&socket, PollFlags::POLLIN)];
        poll(&mut poll_fds, -1)?;

        for event in socket.iter() {
            let device = event.device();
            if !is_external_mouse(&device) {
                continue;
            }

            match event.event_type() {
                udev::EventType::Add => {
                    let should_disable = mice.connect();
                    info!(
                        "External mouse connected ({}); mouse count = {}",
                        device.sysname().to_string_lossy(),
                        mice.count()
                    );
                    if should_disable {
                        set_touchpad_enabled(touchpad_name, false);
                    }
                }
                udev::EventType::Remove => {
                    let should_enable = mice.disconnect();
                    info!(
                        "External mouse disconnected ({}); mouse count = {}",
                        device.sysname().to_string_lossy(),
                        mice.count()
                    );
                    if should_enable {
                        set_touchpad_enabled(touchpad_name, true);
                    }
                }
                _ => {}
            }
        }
    }
}
