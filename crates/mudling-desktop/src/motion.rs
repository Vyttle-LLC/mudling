//! Follows the OS's reduced-motion setting into gpui's own flag, which gpui
//! doesn't set on any platform. Windows have nothing to subscribe to here, so
//! the flag is refreshed at startup and whenever a panel is about to move.

use gpui::App;

#[cfg(target_os = "macos")]
#[cfg_attr(test, allow(dead_code))]
pub fn follow_os(cx: &mut App) {
    cx.set_reduce_motion(crate::macos::reduce_motion());
}

/// Windows calls this "Animation effects".
#[cfg(target_os = "windows")]
#[cfg_attr(test, allow(dead_code))]
pub fn follow_os(cx: &mut App) {
    cx.set_reduce_motion(!client_area_animates());
}

#[cfg(target_os = "windows")]
#[allow(unsafe_code)]
#[cfg_attr(test, allow(dead_code))]
fn client_area_animates() -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        SPI_GETCLIENTAREAANIMATION, SystemParametersInfoW,
    };
    let mut animates = 1i32;
    // SAFETY: this action writes one BOOL, an i32, through the pointer.
    let read = unsafe {
        SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION, 0, (&raw mut animates).cast(), 0)
    };
    read == 0 || animates != 0
}

/// There's no one setting on Linux; GNOME's is the one toolkits share. It takes
/// a process to read, which can't happen on the UI thread, so a change made
/// mid-session counts from the panel's next move. Without `gsettings` panels
/// slide.
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
#[cfg_attr(test, allow(dead_code))]
pub fn follow_os(cx: &mut App) {
    cx.spawn(async move |cx| {
        let read = cx
            .background_executor()
            .spawn(async { animations_are_off() });
        let reduced = read.await;
        cx.update(|cx| cx.set_reduce_motion(reduced));
    })
    .detach();
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
#[cfg_attr(test, allow(dead_code))]
fn animations_are_off() -> bool {
    std::process::Command::new("gsettings")
        .args(["get", "org.gnome.desktop.interface", "enable-animations"])
        .output()
        .is_ok_and(|output| String::from_utf8_lossy(&output.stdout).trim() == "false")
}
