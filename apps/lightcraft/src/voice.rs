//! Native voice transport. The UI owns consent, screenshots and validated edits.

#[cfg(any(windows, test))]
mod audio;
#[cfg(any(windows, test))]
mod config;
#[cfg(any(windows, test))]
mod protocol;
#[cfg(windows)]
mod windows;

use lightcraft_ui_egui::assistant::VoiceConnection;

pub fn start(ctx: egui::Context) -> Result<VoiceConnection, String> {
    #[cfg(windows)]
    {
        windows::start(ctx)
    }
    #[cfg(not(windows))]
    {
        let _ = ctx;
        Err("Native voice is currently available on Windows.".into())
    }
}
