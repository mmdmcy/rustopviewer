#[cfg(target_os = "windows")]
pub fn is_process_elevated() -> bool {
    use std::mem::size_of;
    use windows::Win32::{
        Foundation::CloseHandle,
        Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation},
        System::Threading::{GetCurrentProcess, OpenProcessToken},
    };

    unsafe {
        let mut token = Default::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }

        let mut elevation = TOKEN_ELEVATION::default();
        let mut returned = 0u32;
        let success = GetTokenInformation(
            token,
            TokenElevation,
            Some((&mut elevation as *mut TOKEN_ELEVATION).cast()),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut returned,
        )
        .is_ok();

        let _ = CloseHandle(token);
        success && elevation.TokenIsElevated != 0
    }
}

#[cfg(unix)]
pub fn is_process_elevated() -> bool {
    unsafe { libc::geteuid() == 0 }
}

pub fn username() -> Option<String> {
    first_non_empty_env(&["USER", "USERNAME"])
}

pub fn hostname() -> Option<String> {
    first_non_empty_env(&["HOSTNAME", "COMPUTERNAME"])
        .or_else(read_hostname_file)
        .map(|hostname| hostname.trim_end_matches('.').to_string())
        .filter(|hostname| !hostname.is_empty())
}

pub fn os_label() -> String {
    if cfg!(target_os = "windows") {
        "Windows".to_string()
    } else if cfg!(target_os = "linux") {
        linux_pretty_name().unwrap_or_else(|| "Linux".to_string())
    } else if cfg!(target_os = "macos") {
        "macOS".to_string()
    } else {
        std::env::consts::OS.to_string()
    }
}

pub fn os_family() -> String {
    std::env::consts::OS.to_string()
}

fn first_non_empty_env(names: &[&str]) -> Option<String> {
    names.iter().find_map(|name| {
        std::env::var(name)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    })
}

#[cfg(unix)]
fn read_hostname_file() -> Option<String> {
    std::fs::read_to_string("/etc/hostname")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

#[cfg(not(unix))]
fn read_hostname_file() -> Option<String> {
    None
}

#[cfg(target_os = "linux")]
fn linux_pretty_name() -> Option<String> {
    let content = std::fs::read_to_string("/etc/os-release").ok()?;
    content.lines().find_map(|line| {
        line.strip_prefix("PRETTY_NAME=")
            .or_else(|| line.strip_prefix("NAME="))
            .map(unquote_os_release_value)
            .filter(|value| !value.is_empty())
    })
}

#[cfg(not(target_os = "linux"))]
fn linux_pretty_name() -> Option<String> {
    None
}

fn unquote_os_release_value(value: &str) -> String {
    value
        .trim()
        .trim_matches('"')
        .trim_matches('\'')
        .to_string()
}
