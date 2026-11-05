//! A StatusNotifier item. Waybar's tray (and any other host) shows it.
//! Open, start, stop, restart, and quit go through the user service, so a
//! restart does not race the process that is keeping the guest alive.

use std::process::Command;
use std::thread;

use ksni::menu::{MenuItem, StandardItem};
use ksni::{Tray, TrayService};

use crate::session::Config;
use crate::{notify, vm_running};

struct Applet {
    reims: String,
    name: String,
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
    });
    let handle = service.spawn();
    // The watcher owns the menu. This thread just keeps the process up.
    loop {
        thread::park();
        let _ = &handle;
    }
}
