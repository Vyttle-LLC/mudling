//! The release assets an update downloads, named as
//! `.github/workflows/release.yml` and the packaging scripts name them. Keep
//! the two in step: an asset this can't find is never an update.

use super::{Os, Platform};
use std::path::PathBuf;

/// The bundle inside the macOS zip, and the one an update replaces.
pub const MAC_APP: &str = "MUDling.app";

/// The asset this platform updates from, or `None` if no release builds for
/// it: `MUDling-<version>-macos-<arm64|x86_64>.zip`,
/// `MUDling-<version>-windows-<x64|arm64>.zip` (the plain zip, not the
/// installer) or `MUDling-<version>-linux-<x64|arm64>.AppImage`.
pub fn name(version: &str, platform: Platform) -> Option<String> {
    let (os, extension) = match platform.os {
        Os::Mac => ("macos", "zip"),
        Os::Windows => ("windows", "zip"),
        Os::Linux => ("linux", "AppImage"),
    };
    Some(format!(
        "MUDling-{version}-{os}-{}.{extension}",
        arch(platform)?
    ))
}

/// Where `MUDling.exe` sits in the Windows zip: in a folder named as the zip.
pub fn windows_exe(version: &str, platform: Platform) -> PathBuf {
    let folder = format!(
        "MUDling-{version}-windows-{}",
        arch(platform).unwrap_or_default()
    );
    PathBuf::from(folder).join("MUDling.exe")
}

/// macOS names Intel `x86_64`; Windows and Linux call it `x64`.
fn arch(platform: Platform) -> Option<&'static str> {
    match (platform.os, platform.arch) {
        (_, "aarch64") => Some("arm64"),
        (Os::Mac, "x86_64") => Some("x86_64"),
        (_, "x86_64") => Some("x64"),
        _ => None,
    }
}
