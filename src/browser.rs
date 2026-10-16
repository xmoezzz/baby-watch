use std::process::Command;

use crate::storage::BrowserChoice;

pub fn open_url(choice: BrowserChoice, url: &str) -> Result<(), String> {
    match choice {
        BrowserChoice::SystemDefault => webbrowser::open(url).map_err(|e| e.to_string()),
        BrowserChoice::Safari => open_named_browser("Safari", url),
        BrowserChoice::Chrome => open_named_browser("Google Chrome", url),
    }
}

#[cfg(target_os = "macos")]
fn open_named_browser(app: &str, url: &str) -> Result<(), String> {
    let status = Command::new("open")
        .args(["-a", app, url])
        .status()
        .map_err(|e| format!("Could not launch {app}: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{app} failed to launch with status {status}"))
    }
}

#[cfg(not(target_os = "macos"))]
fn open_named_browser(_app: &str, url: &str) -> Result<(), String> {
    webbrowser::open(url).map_err(|e| e.to_string())
}
