//! A later change of the configured password, or of the login window, applied
//! to a guest that is already installed.

use std::fs;
use std::path::Path;

use sha2::{Digest, Sha256};

/// Identity of a password. The password itself stays in `account.password`.
pub fn password_id(password: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(password.as_bytes());
    format!("{:x}", hasher.finalize())
}

pub fn encode_base64(data: &[u8]) -> String {
    const TABLE: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    let mut i = 0;
    while i + 3 <= data.len() {
        let n = (u32::from(data[i]) << 16) | (u32::from(data[i + 1]) << 8) | u32::from(data[i + 2]);
        out.push(TABLE[((n >> 18) & 63) as usize] as char);
        out.push(TABLE[((n >> 12) & 63) as usize] as char);
        out.push(TABLE[((n >> 6) & 63) as usize] as char);
        out.push(TABLE[(n & 63) as usize] as char);
        i += 3;
    }
    if i < data.len() {
        let mut n = u32::from(data[i]) << 16;
        out.push(TABLE[((n >> 18) & 63) as usize] as char);
        if i + 1 < data.len() {
            n |= u32::from(data[i + 1]) << 8;
            out.push(TABLE[((n >> 12) & 63) as usize] as char);
            out.push(TABLE[((n >> 6) & 63) as usize] as char);
            out.push('=');
        } else {
            out.push(TABLE[((n >> 12) & 63) as usize] as char);
            out.push('=');
            out.push('=');
        }
    }
    out
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Update {
    pub old: String,
    pub new: String,
    pub login_window: bool,
}

/// What to send the guest. `configured` is the module password, when one is set.
/// None means the guest already matches.
pub fn plan(
    file_password: &str,
    configured: Option<&str>,
    stored_id: &str,
    stored_login_window: Option<bool>,
    login_window: bool,
) -> Option<Update> {
    let new = configured.unwrap_or(file_password);
    let password_differs = password_id(new) != stored_id.trim() || new != file_password;
    let login_differs = stored_login_window != Some(login_window);
    if !password_differs && !login_differs {
        return None;
    }
    // The file was edited and the module has no password: the previous
    // password is not known, so the guest cannot be updated from here.
    if configured.is_none() && password_id(file_password) != stored_id.trim() && !login_differs {
        return None;
    }
    Some(Update {
        old: file_password.to_owned(),
        new: new.to_owned(),
        login_window,
    })
}

/// Reads the four lines the host sends on stdin: old password, new password,
/// whether the login window is shown, and the auto-login secret as base64.
pub fn guest_script(account: &str) -> String {
    format!(
        r#"set -e
read -r OLD
read -r NEW
read -r LOGIN
read -r KC
if [ "$OLD" != "$NEW" ]; then
  dscl . -passwd /Users/{account} "$OLD" "$NEW"
fi
if [ "$LOGIN" = true ]; then
  printf '%s\n' "$NEW" | sudo -S rm -f /etc/kcpassword
  printf '%s\n' "$NEW" | sudo -S defaults delete /Library/Preferences/com.apple.loginwindow autoLoginUser || true
else
  printf '%s' "$KC" | base64 -D > /tmp/macos-kvm-kcpassword
  printf '%s\n' "$NEW" | sudo -S install -m 600 /tmp/macos-kvm-kcpassword /etc/kcpassword
  rm -f /tmp/macos-kvm-kcpassword
  printf '%s\n' "$NEW" | sudo -S defaults write /Library/Preferences/com.apple.loginwindow autoLoginUser {account}
fi
"#
    )
}

pub fn record(dir: &Path, password: &str, login_window: bool) -> std::io::Result<()> {
    fs::write(
        dir.join("password.sha256"),
        format!("{}\n", password_id(password)),
    )?;
    fs::write(
        dir.join("login-window"),
        if login_window { "true\n" } else { "false\n" },
    )?;
    Ok(())
}

pub fn stored_login_window(dir: &Path) -> Option<bool> {
    match fs::read_to_string(dir.join("login-window")).ok()?.trim() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_password_and_login_window_need_nothing() {
        let id = password_id("secret");
        assert!(plan("secret", Some("secret"), &id, Some(false), false).is_none());
    }

    #[test]
    fn a_new_module_password_is_applied_with_the_old_one() {
        let update = plan(
            "secret",
            Some("changed"),
            &password_id("secret"),
            Some(false),
            false,
        )
        .unwrap();
        assert_eq!(update.old, "secret");
        assert_eq!(update.new, "changed");
        assert!(!update.login_window);
    }

    #[test]
    fn turning_the_login_window_on_is_applied() {
        let id = password_id("secret");
        let update = plan("secret", Some("secret"), &id, Some(false), true).unwrap();
        assert!(update.login_window);
        assert_eq!(update.old, update.new);
    }

    #[test]
    fn an_edited_password_file_is_not_a_guest_update() {
        assert!(plan("edited", None, &password_id("secret"), Some(false), false).is_none());
    }

    #[test]
    fn the_guest_script_changes_the_password_and_the_login_window() {
        let script = guest_script("alex");
        assert!(script.contains("dscl . -passwd /Users/alex"));
        assert!(script.contains("autoLoginUser alex"));
        assert!(script.contains("rm -f /etc/kcpassword"));
        assert!(script.contains("base64 -D"));
    }

    #[test]
    fn base64_matches_the_standard_alphabet() {
        assert_eq!(encode_base64(b"hello"), "aGVsbG8=");
        assert_eq!(encode_base64(b"hi"), "aGk=");
        assert_eq!(encode_base64(&[0x7d, 0x89]), "fYk=");
    }
}
