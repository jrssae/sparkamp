//! Manual on-host check for external-device detection.
//!
//! Builds against the real crate and queries udisks2 over the system bus, so
//! it must run on the host (the distrobox can't reach the system bus). Plug in
//! a USB stick / SD card and run:
//!
//!     cargo run --example list_devices
//!
//! It prints every detected external device, or the diagnosis kind when the
//! udisks2 service can't be reached.

#[cfg(target_os = "linux")]
fn main() {
    match sparkamp::devices::detect::list_devices() {
        Ok(devices) => {
            println!("detected {} external device(s):", devices.len());
            for d in devices {
                println!(
                    "- label={:?} id={} fs={} free={}/{} bytes ro={} ejectable={}\n  mount={}\n  backend={}",
                    d.label,
                    d.id,
                    d.fs_type,
                    d.free_bytes,
                    d.total_bytes,
                    d.read_only,
                    d.ejectable,
                    d.mount_path.display(),
                    d.backend_id,
                );
            }
        }
        Err(e) => {
            eprintln!("list_devices failed: {e}");
            eprintln!(
                "diagnosis: {:?}",
                sparkamp::devices::detect::classify_error(&e)
            );
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("list_devices example is Linux-only (udisks2).");
}
