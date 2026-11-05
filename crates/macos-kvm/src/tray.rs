//! A StatusNotifier item. Waybar's tray (and any other host) shows it.
//! Open, start, stop, restart, and quit go through the user service, so a
//! restart does not race the process that is keeping the guest alive.

use std::path::PathBuf;
use std::process::Command;
use std::thread;

use ksni::menu::{CheckmarkItem, MenuItem, StandardItem};
use ksni::{Tray, TrayService};

use crate::session::Config;
use crate::{notify, vm_running};

struct Applet {
    reims: String,
    name: String,
    home: PathBuf,
}

impl Tray for Applet {
    fn id(&self) -> String {
        "macos-kvm".into()
    }

    fn title(&self) -> String {
        "macOS".into()
    }

    fn icon_name(&self) -> String {
        "computer".into()
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        vec![
            item("Open", |this| this.service("start")),
            item("Start", |this| this.service("start")),
            item("Stop", |this| this.service("stop")),
            item("Restart", |this| this.service("restart")),
            item("Show", |this| this.show()),
            MenuItem::Checkmark(CheckmarkItem {
                label: "Retina".into(),
                checked: self.retina(),
                activate: Box::new(|this| this.toggle_retina()),
                ..Default::default()
            }),
            item("Quit", |this| this.service("stop")),
        ]
    }
}

impl Applet {
    fn service(&self, action: &str) {
        let action = action.to_owned();
        thread::spawn(move || {
            let status = Command::new("systemctl")
                .args(["--user", &action, "macos-kvm"])
                .status();
            if !status.is_ok_and(|code| code.success()) {
                let _ = Command::new("systemctl")
                    .args(["--user", &action, "macos-kvm.service"])
                    .status();
            }
        });
    }

    fn show(&self) {
        let reims = self.reims.clone();
        let name = self.name.clone();
        thread::spawn(move || {
            let done = Command::new(&reims).args(["show", &name]).output();
            let text = match done {
                Ok(out) if out.status.success() => {
                    String::from_utf8_lossy(&out.stdout).into_owned()
                }
                Ok(out) => String::from_utf8_lossy(&out.stderr).into_owned(),
                Err(err) => err.to_string(),
            };
            let state = if vm_running(&text) {
                "running"
            } else {
                "stopped"
            };
            let body = text
                .lines()
                .find(|line| line.split_whitespace().next() == Some("state"))
                .map(str::trim)
                .unwrap_or(state);
            notify("macOS", body);
        });
    }

    fn vm_toml(&self) -> PathBuf {
        self.home
            .join(".local/share/reims-ctl/vms")
            .join(&self.name)
            .join("vm.toml")
    }

    fn retina(&self) -> bool {
        std::fs::read_to_string(self.vm_toml())
            .ok()
            .is_some_and(|text| hidpi_enabled(&text))
    }

    /// HiDPI is the density QEMU tells the guest. It is read at the next start,
    /// so a running guest is restarted once the setting is saved.
    fn toggle_retina(&mut self) {
        let next = if self.retina() { "false" } else { "true" };
        let saved = Command::new(&self.reims)
            .args(["set", &self.name, "--hidpi", next])
            .status()
            .is_ok_and(|status| status.success());
        if !saved {
            notify("macOS", "Could not change Retina.");
            return;
        }
        let reims = self.reims.clone();
        let name = self.name.clone();
        let installed = self
            .home
            .join(".local/share/reims-ctl/vms")
            .join(&self.name)
            .join("headless-installed")
            .is_file();
        let on = next == "true";
        thread::spawn(move || {
            let show = Command::new(&reims).args(["show", &name]).output();
            let running = show.ok().is_some_and(|out| {
                out.status.success() && vm_running(&String::from_utf8_lossy(&out.stdout))
            });
            if running && installed {
                let _ = Command::new("systemctl")
                    .args(["--user", "restart", "macos-kvm"])
                    .status();
                notify(
                    "macOS",
                    if on {
                        "Retina on. The guest is restarting."
                    } else {
                        "Retina off. The guest is restarting."
                    },
                );
            } else {
                notify(
                    "macOS",
                    if on {
                        "Retina on. It applies at the next start."
                    } else {
                        "Retina off. It applies at the next start."
                    },
                );
            }
        });
    }
}

/// `hidpi` under `[display]` in a reims-ctl `vm.toml`.
pub fn hidpi_enabled(text: &str) -> bool {
    let mut section = String::new();
    for raw in text.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            section = line
                .trim_start_matches('[')
                .trim_end_matches(']')
                .trim()
                .to_owned();
            continue;
        }
        if section == "display" {
            if let Some(rest) = line.strip_prefix("hidpi") {
                let rest = rest.trim().trim_start_matches('=').trim();
                return rest == "true";
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::hidpi_enabled;

    #[test]
    fn retina_is_the_display_hidpi_flag() {
        let on = "\
[display]
resolution = \"3840x2160\"
hidpi = true
[audio]
enabled = true
";
        let off = on.replace("hidpi = true", "hidpi = false");
        assert!(hidpi_enabled(on));
        assert!(!hidpi_enabled(&off));
        assert!(!hidpi_enabled("[audio]\nhidpi = true\n"));
    }
}

fn item(label: &str, activate: fn(&mut Applet)) -> MenuItem<Applet> {
    StandardItem {
        label: label.into(),
        activate: Box::new(activate),
        ..Default::default()
    }
    .into()
}

pub fn run(cfg: &Config) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let service = TrayService::new(Applet {
        reims: cfg.reims.display().to_string(),
        name: cfg.name.clone(),
        home: cfg.home.clone(),
    });
    let handle = service.spawn();
    // The watcher owns the menu. This thread just keeps the process up.
    loop {
        thread::park();
        let _ = &handle;
    }
}
