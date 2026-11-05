//! Headless macOS install. The window stays off. One command is typed into
//! Recovery over QMP; startosinstall then runs with the license already
//! accepted, and the first boot finds the account already planted.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use plist::Value;

use crate::qmp::Qmp;
use crate::{notify, Identity};

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

const LABEL: &str = "MACOSKVM";
const PART_OFFSET: u64 = 1024 * 1024;
const HOURS: u64 = 60 * 60;

pub struct Install {
    pub name: String,
    pub account: String,
    pub host_user: String,
    pub macos: String,
    pub ssh_port: u16,
    /// The module password, when `accountPassword` is set.
    pub password: Option<String>,
    pub password_file: Option<PathBuf>,
    /// True shows the login window. False boots into the desktop.
    pub login_window: bool,
    pub audio: bool,
    pub reims: PathBuf,
    pub util: PathBuf,
    pub macserial: PathBuf,
    pub home: PathBuf,
}

impl Install {
    pub fn vm_dir(&self) -> PathBuf {
        self.home
            .join(".local/share/reims-ctl/vms")
            .join(&self.name)
    }

    /// True when this call did the install. False when the disk was already done.
    pub fn run(&self) -> Result<bool> {
        let dir = self.vm_dir();
        fs::create_dir_all(&dir)?;
        if dir.join("headless-installed").is_file() {
            return Ok(false);
        }
        let _lock = Lock::acquire(&dir.join("install.lock"))?;
        if dir.join("headless-installed").is_file() {
            return Ok(false);
        }
        if dir.join("install-staged").is_file() {
            self.note(
                "Setting up macOS",
                "Finishing the install already on the disk",
            );
            self.continue_staged()?;
            return Ok(true);
        }
        self.note(
            "Setting up macOS",
            "Preparing the guest. The window stays hidden.",
        );
        self.apply_identity()?;
        let password = self.password()?;
        self.log(&format!("account {}", self.account));
        let outcome = self.install_disk(&password);
        if let Err(err) = outcome {
            if dir.join("install-staged").is_file() {
                self.log("staged install stays on the disk");
                notify("macOS", "Setup did not finish. The disk is kept.");
                return Err(err);
            }
            self.log(&format!("install did not finish: {err}"));
            notify("macOS", "Setup did not finish. Opening the window.");
            self.show_window();
            return Err(err);
        }
        Ok(true)
    }

    fn install_disk(&self, password: &str) -> Result<()> {
        let dir = self.vm_dir();
        self.ensure_offline()?;
        self.stop()?;
        // The install stays quiet: no window, and no boot chime.
        self.set_audio(false)?;
        let image = self.build_image(password)?;
        self.attach(Some(&image))?;
        self.set_window(false)?;
        self.start()?;
        std::thread::sleep(Duration::from_secs(45));
        let mut qmp = Qmp::connect(&dir.join("qmp.sock"))?;
        let command = format!("diskutil mount {LABEL}; sh /Volumes/{LABEL}/go.sh");
        let mut started = false;
        for attempt in 0..12 {
            self.log(&format!("opening Recovery Terminal ({})", attempt + 1));
            open_terminal(&mut qmp, &command, attempt > 0)?;
            if self
                .wait_status(&image, "TERMINAL", Duration::from_secs(30))
                .is_ok()
            {
                started = true;
                break;
            }
            self.log("the script has not started, trying the shortcut again");
        }
        if !started {
            return Err("could not start the install script".into());
        }
        drop(qmp);
        self.note("Setting up macOS", "Copying the installer onto the disk");
        self.wait_status(&image, "STARTOSINSTALL", Duration::from_secs(3 * HOURS))?;
        self.note("Setting up macOS", "Installing macOS");
        self.wait_reboot(&image, Duration::from_secs(3 * HOURS))?;
        fs::write(dir.join("install-staged"), "ok\n")?;
        self.finish_copy_and_plant()
    }

    fn continue_staged(&self) -> Result<()> {
        self.finish_copy_and_plant()
    }

    /// The first reboot only starts the copy. The next one is the new system.
    /// The account is then written from Recovery, because the package
    /// startosinstall accepts does not always land on the Data volume.
    fn finish_copy_and_plant(&self) -> Result<()> {
        let dir = self.vm_dir();
        let image = dir.join("bootstrap.img");
        if !dir.join("install-copied").is_file() {
            self.note("Setting up macOS", "Installing macOS");
            // The installer image would boot Recovery again. The staged
            // install continues from the internal disk.
            self.boot_installed_disk()?;
            self.wait_for_reset(Duration::from_secs(8 * HOURS))?;
            fs::write(dir.join("install-copied"), "ok\n")?;
        }
        if !dir.join("account-planted").is_file() {
            self.plant(&image)?;
        }
        self.boot_installed_disk()?;
        self.wait_for_ssh(&image)?;
        self.finish_installed()
    }

    fn boot_installed_disk(&self) -> Result<()> {
        self.log("booting the internal disk");
        self.stop()?;
        let _ = self.reims(&["eject", &self.name]);
        self.restore_opencore()?;
        self.attach(None)?;
        self.set_window(false)?;
        self.set_audio(false)?;
        self.start()
    }

    fn wait_for_reset(&self, limit: Duration) -> Result<()> {
        let deadline = Instant::now() + limit;
        let mut qmp = None;
        let mut announced = Instant::now();
        while Instant::now() < deadline {
            if qmp.is_none() {
                match Qmp::connect(&self.vm_dir().join("qmp.sock")) {
                    Ok(conn) => qmp = Some(conn),
                    Err(_) => {
                        if !self.running() {
                            self.log("qemu exited while macOS was installing");
                            return Ok(());
                        }
                        std::thread::sleep(Duration::from_secs(2));
                        continue;
                    }
                }
            }
            let Some(conn) = qmp.as_mut() else { continue };
            match (|| -> Result<serde_json::Value> {
                conn.drain()?;
                conn.execute("query-status", None)
            })() {
                Err(err) => {
                    self.log(&format!("qemu connection closed ({err})"));
                    return Ok(());
                }
                Ok(status) => {
                    if conn
                        .events
                        .iter()
                        .any(|event| matches!(event.as_str(), "RESET" | "SHUTDOWN" | "POWERDOWN"))
                    {
                        self.log("the installer rebooted into the new system");
                        return Ok(());
                    }
                    let word = status
                        .get("status")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("");
                    if word == "shutdown" || word == "guest-panicked" {
                        self.log(&format!("guest status {word}"));
                        return Ok(());
                    }
                }
            }
            if announced.elapsed() > Duration::from_secs(60) {
                self.log("waiting for the installer to finish");
                announced = Instant::now();
            }
            std::thread::sleep(Duration::from_secs(5));
        }
        Err("the installer never rebooted into the new system".into())
    }

    fn plant(&self, image: &Path) -> Result<()> {
        let dir = self.vm_dir();
        self.note("Setting up macOS", "Creating the account");
        self.stop()?;
        self.picker_opencore()?;
        self.ensure_offline()?;
        self.attach(Some(image))?;
        self.set_window(false)?;
        self.set_audio(false)?;
        self.start()?;
        // OpenCore's text picker. With the installer attached, entry 3 is
        // the installer's Recovery, which can see the Data volume.
        std::thread::sleep(Duration::from_secs(25));
        {
            let mut qmp = Qmp::connect(&dir.join("qmp.sock"))?;
            qmp.type_text("3")?;
        }
        std::thread::sleep(Duration::from_secs(70));
        let command = format!("diskutil mount {LABEL}; sh /Volumes/{LABEL}/plant.sh");
        let mut qmp = Qmp::connect(&dir.join("qmp.sock"))?;
        let mut planted = false;
        let mut failure = None;
        for attempt in 0..12 {
            self.log(&format!("planting the account ({})", attempt + 1));
            let before = self.status(image).len();
            open_terminal(&mut qmp, &command, attempt > 0)?;
            let status = self.latest_plant(image, before, Duration::from_secs(45));
            if status.contains("PLANTED") {
                planted = true;
                break;
            }
            if status.contains("FAIL ") && !status.contains("FAIL no data volume") {
                failure = Some(status.trim().to_owned());
                break;
            }
            self.log("the account is not on the disk yet");
        }
        drop(qmp);
        self.stop()?;
        self.restore_opencore()?;
        let _ = self.reims(&["eject", &self.name]);
        self.attach(None)?;
        if let Some(failure) = failure {
            return Err(failure.into());
        }
        if !planted {
            return Err("could not plant the account".into());
        }
        fs::write(dir.join("account-planted"), "ok\n")?;
        Ok(())
    }

    /// plant.sh appends. Only the lines after the latest FINISH belong to
    /// this attempt; an earlier FAIL must not end the retry.
    fn latest_plant(&self, image: &Path, before: usize, limit: Duration) -> String {
        let deadline = Instant::now() + limit;
        while Instant::now() < deadline {
            let text = self.status(image);
            let fresh = if text.len() > before {
                &text[before..]
            } else {
                ""
            };
            if let Some(at) = fresh.rfind("FINISH") {
                let latest = &fresh[at..];
                if latest.contains("PLANTED") || latest.contains("FAIL ") {
                    return latest.to_owned();
                }
            }
            std::thread::sleep(Duration::from_secs(5));
        }
        String::new()
    }

    fn picker_opencore(&self) -> Result<()> {
        let dir = self.vm_dir();
        let dest = dir.join("opencore.qcow2");
        let saved = dir.join("opencore.auto.qcow2");
        if !saved.is_file() && dest.is_file() {
            fs::copy(&dest, &saved)?;
        }
        let id = self.identity()?;
        let fresh = dir.join("opencore.picker.qcow2");
        let _ = fs::remove_file(&fresh);
        run(
            &self.util,
            &[
                "opencore",
                "-o",
                fresh.to_str().unwrap_or("opencore.picker.qcow2"),
                "--model",
                "iMac20,1",
                "--serial",
                &id.serial,
                "--mlb",
                &id.mlb,
                "--uuid",
                &id.uuid,
                "--rom",
                &id.mac,
                "--timeout",
                "0",
                "--text-picker",
                "--show-picker",
                "--no-ovmf",
            ],
        )?;
        let _ = fs::remove_file(&dest);
        fs::rename(&fresh, &dest)?;
        Ok(())
    }

    fn restore_opencore(&self) -> Result<()> {
        let dir = self.vm_dir();
        let saved = dir.join("opencore.auto.qcow2");
        let dest = dir.join("opencore.qcow2");
        if saved.is_file() {
            let _ = fs::remove_file(&dest);
            fs::copy(&saved, &dest)?;
        }
        Ok(())
    }

    fn finish_installed(&self) -> Result<()> {
        let dir = self.vm_dir();
        let password = self.password()?;
        self.stop()?;
        self.set_window(true)?;
        self.set_audio(self.audio)?;
        self.attach(None)?;
        self.start()?;
        self.write_password_file(&password)?;
        fs::write(dir.join("account.name"), format!("{}\n", self.account))?;
        crate::sync::record(&dir, &password, self.login_window)?;
        fs::write(dir.join("headless-installed"), "ok\n")?;
        self.note("macOS", "The guest is ready.");
        Ok(())
    }

    fn wait_for_ssh(&self, image: &Path) -> Result<()> {
        self.note("Setting up macOS", "Waiting for the first boot");
        let deadline = Instant::now() + Duration::from_secs(8 * HOURS);
        while Instant::now() < deadline {
            if image.is_file() && self.status(image).contains("BOOTSTRAP") {
                self.log("first-boot bootstrap ran");
            }
            if crate::ssh_banner_up(self.ssh_port) {
                std::thread::sleep(Duration::from_secs(45));
                if crate::ssh_banner_up(self.ssh_port) {
                    return Ok(());
                }
            }
            std::thread::sleep(Duration::from_secs(15));
        }
        Err("SSH did not stay up".into())
    }

    fn apply_identity(&self) -> Result<()> {
        let dir = self.vm_dir();
        if dir.join("identity.applied").is_file() {
            return Ok(());
        }
        let id = self.identity()?;
        self.log(&format!("OpenCore identity {} ({})", id.serial, id.mac));
        self.stop()?;
        self.reims(&["set", &self.name, "--mac", &id.mac])?;
        let fresh = dir.join("opencore.new.qcow2");
        let _ = fs::remove_file(&fresh);
        run(
            &self.util,
            &[
                "opencore",
                "-o",
                fresh.to_str().unwrap_or("opencore.new.qcow2"),
                "--model",
                "iMac20,1",
                "--serial",
                &id.serial,
                "--mlb",
                &id.mlb,
                "--uuid",
                &id.uuid,
                "--rom",
                &id.mac,
                "--no-ovmf",
            ],
        )?;
        let dest = dir.join("opencore.qcow2");
        let _ = fs::remove_file(&dest);
        fs::rename(&fresh, &dest)?;
        fs::write(dir.join("identity.applied"), "ok\n")?;
        Ok(())
    }

    fn identity(&self) -> Result<Identity> {
        let path = self.vm_dir().join("identity.env");
        if let Ok(text) = fs::read_to_string(&path) {
            if let Some(id) = crate::parse_identity(&text) {
                return Ok(id);
            }
        }
        let line = output(&self.macserial, &["--num", "1", "--model", "iMac20,1"])?;
        let (serial, mlb) = crate::parse_macserial(&line)?;
        let mut random = [0u8; 16];
        random_bytes(&mut random)?;
        let uuid = crate::uuid_v4(random);
        let mut tail = [0u8; 3];
        random_bytes(&mut tail)?;
        let mac = crate::apple_mac(random[0], tail);
        let id = Identity {
            serial,
            mlb,
            uuid,
            mac,
        };
        fs::write(&path, crate::format_identity(&id))?;
        let mut perms = fs::metadata(&path)?.permissions();
        perms.set_mode(0o600);
        fs::set_permissions(&path, perms)?;
        Ok(id)
    }

    pub fn password(&self) -> Result<String> {
        if let Some(password) = &self.password {
            if !password.is_empty() {
                return Ok(password.clone());
            }
        }
        if let Some(path) = &self.password_file {
            return Ok(fs::read_to_string(path)?.trim_end_matches('\n').to_owned());
        }
        let path = self.vm_dir().join("account.password");
        if path.is_file() {
            return Ok(fs::read_to_string(&path)?.trim_end_matches('\n').to_owned());
        }
        let password = crate::random_password();
        self.write_password_file(&password)?;
        Ok(password)
    }

    pub fn write_password_file(&self, password: &str) -> Result<()> {
        let path = self.vm_dir().join("account.password");
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&path, format!("{password}\n"))?;
        let mut perms = fs::metadata(&path)?.permissions();
        perms.set_mode(0o600);
        fs::set_permissions(&path, perms)?;
        Ok(())
    }

    /// The password currently on disk, before a module password replaces it.
    pub fn stored_password(&self) -> Option<String> {
        fs::read_to_string(self.vm_dir().join("account.password"))
            .ok()
            .map(|text| text.trim_end_matches('\n').to_owned())
            .filter(|text| !text.is_empty())
    }

    fn ensure_offline(&self) -> Result<()> {
        let cache = self.home.join(".local/share/reims-ctl/installers");
        fs::create_dir_all(&cache)?;
        loop {
            let mut images: Vec<_> = fs::read_dir(&cache)?
                .filter_map(|entry| entry.ok())
                .map(|entry| entry.path())
                .filter(|path| {
                    path.file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| name.ends_with("offline-installer.img"))
                })
                .collect();
            images.sort();
            let busy = download_running();
            if !images.is_empty() && !busy {
                let image = images.last().unwrap().to_string_lossy().into_owned();
                return self.reims(&["insert", &self.name, &image]);
            }
            if images.is_empty() && !busy {
                let dest = cache.to_string_lossy().into_owned();
                run(&self.util, &["offline-installer", &self.macos, "-o", &dest])?;
                continue;
            }
            self.log("waiting for the offline installer download to finish");
            std::thread::sleep(Duration::from_secs(30));
        }
    }

    fn build_image(&self, password: &str) -> Result<PathBuf> {
        let dir = self.vm_dir();
        let stage = dir.join("bootstrap-src");
        let _ = fs::remove_dir_all(&stage);
        fs::create_dir_all(&stage)?;
        let ssh = self.ensure_keys()?;
        self.account_files(password, &stage, &ssh)?;
        let component = stage.join("component.pkg");
        let pkg = stage.join("bootstrap.pkg");
        build_pkg(&stage, &component)?;
        build_product(&stage, &component, &pkg)?;
        fs::write(stage.join("go.sh"), include_str!("../guest/go.sh"))?;
        fs::set_permissions(stage.join("go.sh"), mode(0o755))?;
        fs::write(
            stage.join("plant.sh"),
            include_str!("../guest/plant.sh").replace("@ACCOUNT@", &self.account),
        )?;
        fs::set_permissions(stage.join("plant.sh"), mode(0o755))?;
        fs::write(stage.join("status"), "")?;
        let image = dir.join("bootstrap.img");
        let _ = fs::remove_file(&image);
        let file = File::create(&image)?;
        file.set_len(64 * 1024 * 1024)?;
        drop(file);
        let script =
            "label: gpt\nstart=2048, type=EBD0A0A2-B9E5-4433-87C0-68B6B72699C7, name=MACOSKVM\n";
        let mut child = Command::new("sfdisk")
            .args(["--wipe", "always"])
            .arg(&image)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()?;
        child.stdin.take().unwrap().write_all(script.as_bytes())?;
        let done = child.wait_with_output()?;
        if !done.status.success() {
            return Err(format!("sfdisk: {}", String::from_utf8_lossy(&done.stderr)).into());
        }
        let spec = format!("{}@@{PART_OFFSET}", image.display());
        run_at("mformat", &["-i", &spec, "-v", LABEL, "-F", "::"], None)?;
        for name in [
            "go.sh",
            "plant.sh",
            "bootstrap.pkg",
            "bootstrap.sh",
            "status",
            "user.plist",
            "kcpassword",
            "GlobalPreferences.plist",
            "loginwindow.plist",
            "authorized_keys",
            "guest_id_ed25519",
            "guest_id_ed25519.pub",
            "guest_ssh_config",
            "nsmb.conf",
            "share.sh",
            "com.macos-kvm.share.plist",
            "com.macos-kvm.bootstrap.plist",
        ] {
            let source = stage.join(name);
            if !source.is_file() {
                continue;
            }
            run_at(
                "mcopy",
                &[
                    "-pm",
                    "-i",
                    &spec,
                    source.to_str().unwrap_or(name),
                    &format!("::{name}"),
                ],
                None,
            )?;
        }
        fs::set_permissions(&image, mode(0o600))?;
        Ok(image)
    }

    fn account_files(&self, password: &str, dest: &Path, ssh: &Path) -> Result<()> {
        let mut salt = [0u8; 32];
        random_bytes(&mut salt)?;
        let mut uid = [0u8; 16];
        random_bytes(&mut uid)?;
        write_bytes(
            dest.join("user.plist"),
            &user_plist(&self.account, password, &salt, uid)?,
        )?;
        write_xml(
            dest.join("GlobalPreferences.plist"),
            &plist_dict(&[("METAL_ERROR_MODE", Value::from(5i64))]),
        )?;
        if !self.login_window {
            write_bytes(dest.join("kcpassword"), &crate::kcpassword(password))?;
            fs::set_permissions(dest.join("kcpassword"), mode(0o600))?;
            write_xml(
                dest.join("loginwindow.plist"),
                &plist_dict(&[("autoLoginUser", Value::String(self.account.clone()))]),
            )?;
        }
        let boot = include_str!("../guest/bootstrap.sh").replace("@ACCOUNT@", &self.account);
        fs::write(dest.join("bootstrap.sh"), boot)?;
        fs::set_permissions(dest.join("bootstrap.sh"), mode(0o755))?;
        fs::write(dest.join("authorized_keys"), authorized_keys(ssh)?)?;
        fs::copy(ssh.join("guest_id_ed25519"), dest.join("guest_id_ed25519"))?;
        fs::copy(
            ssh.join("guest_id_ed25519.pub"),
            dest.join("guest_id_ed25519.pub"),
        )?;
        fs::write(
            dest.join("guest_ssh_config"),
            format!(
                "\
Host host linux
  HostName 10.0.2.2
  User {user}
  IdentityFile ~/.ssh/id_ed25519
  IdentitiesOnly yes
  StrictHostKeyChecking accept-new
  Compression no
  Ciphers aes128-gcm@openssh.com
",
                user = self.host_user
            ),
        )?;
        fs::write(
            dest.join("nsmb.conf"),
            "[default]\nsigning_required=no\nvalidate_neg_off=yes\n",
        )?;
        fs::write(
            dest.join("share.sh"),
            "#!/bin/bash\ndest=\"$HOME/SHARED_FOLDER\"\nmkdir -p \"$dest\"\nif mount | grep -F -q \" on $dest \"; then\n  exit 0\nfi\nexec mount_smbfs -N '//guest:@10.0.2.4/qemu' \"$dest\"\n",
        )?;
        fs::set_permissions(dest.join("share.sh"), mode(0o755))?;
        write_xml(
            dest.join("com.macos-kvm.share.plist"),
            &agent_plist(
                "com.macos-kvm.share",
                &["/bin/bash", "/Library/macos-kvm/share.sh"],
                true,
            ),
        )?;
        write_xml(
            dest.join("com.macos-kvm.bootstrap.plist"),
            &agent_plist(
                "com.macos-kvm.bootstrap",
                &["/bin/bash", "/Library/macos-kvm/bootstrap.sh"],
                false,
            ),
        )?;
        Ok(())
    }

    pub fn ensure_keys(&self) -> Result<PathBuf> {
        let ssh = self.vm_dir().join("ssh");
        fs::create_dir_all(&ssh)?;
        fs::set_permissions(&ssh, mode(0o700))?;
        for (name, comment) in [
            ("id_ed25519", format!("macos-kvm-{}", self.name)),
            ("guest_id_ed25519", format!("macos-kvm-{}-guest", self.name)),
        ] {
            let private = ssh.join(name);
            if !private.is_file() {
                run(
                    "ssh-keygen",
                    &[
                        "-t",
                        "ed25519",
                        "-N",
                        "",
                        "-C",
                        &comment,
                        "-f",
                        &private.to_string_lossy(),
                    ],
                )?;
            }
            fs::set_permissions(&private, mode(0o600))?;
        }
        Ok(ssh)
    }

    fn show(&self) -> Result<String> {
        output(&self.reims, &["show", &self.name])
    }

    fn running(&self) -> bool {
        self.show().is_ok_and(|text| crate::vm_running(&text))
    }

    fn start(&self) -> Result<()> {
        if !self.running() {
            self.reims(&["start", &self.name])?;
        }
        Ok(())
    }

    fn stop(&self) -> Result<()> {
        if self.running() {
            self.reims(&["stop", &self.name, "--timeout", "20"])?;
        }
        Ok(())
    }

    fn set_window(&self, visible: bool) -> Result<()> {
        let value = if visible {
            "REIMS_VGPU_WINDOW="
        } else {
            "REIMS_VGPU_WINDOW=0"
        };
        self.reims(&["set", &self.name, "--env", value])
    }

    fn set_audio(&self, on: bool) -> Result<()> {
        self.reims(&[
            "set",
            &self.name,
            "--audio",
            if on { "true" } else { "false" },
        ])
    }

    fn attach(&self, image: Option<&Path>) -> Result<()> {
        let extra = match image {
            None => String::new(),
            Some(image) => format!(
                "-drive id=bootstrap,if=none,format=raw,file={},cache=writethrough -device ide-hd,bus=sata.5,drive=bootstrap",
                image.display()
            ),
        };
        self.reims(&["set", &self.name, "--extra-args", &extra])
    }

    fn show_window(&self) {
        let _ = self.stop();
        let _ = self.set_window(true);
        let _ = self.start();
    }

    fn reims(&self, args: &[&str]) -> Result<()> {
        run(&self.reims, args)
    }

    fn status(&self, image: &Path) -> String {
        let spec = format!("{}@@{PART_OFFSET}", image.display());
        output("mtype", &["-i", &spec, "::status"]).unwrap_or_default()
    }

    fn wait_status(&self, image: &Path, needle: &str, limit: Duration) -> Result<String> {
        let deadline = Instant::now() + limit;
        let mut last = String::new();
        let mut announced = Instant::now();
        while Instant::now() < deadline {
            let text = self.status(image);
            if text != last {
                let fresh = text.strip_prefix(&last).unwrap_or(&text);
                for line in fresh.lines().filter(|line| !line.trim().is_empty()) {
                    self.log(&format!("guest: {line}"));
                }
                last = text.clone();
                announced = Instant::now();
            }
            if text.contains("FAIL ") && !text.contains(needle) {
                return Err(text.trim().to_owned().into());
            }
            if text.contains(needle) {
                return Ok(text);
            }
            if announced.elapsed() > Duration::from_secs(60) {
                self.log(&format!("waiting for {needle}"));
                announced = Instant::now();
            }
            std::thread::sleep(Duration::from_secs(5));
        }
        Err(format!("timed out waiting for {needle}").into())
    }

    fn wait_reboot(&self, image: &Path, limit: Duration) -> Result<()> {
        let deadline = Instant::now() + limit;
        let mut qmp = None;
        let mut last = String::new();
        let mut announced = false;
        while Instant::now() < deadline {
            let text = self.status(image);
            if text != last {
                let fresh = text.strip_prefix(&last).unwrap_or(&text);
                for line in fresh.lines().filter(|line| !line.trim().is_empty()) {
                    self.log(&format!("guest: {line}"));
                }
                last = text.clone();
            }
            if text.contains("FAIL ") && !text.contains("FAIL startosinstall exited 0") {
                return Err(text.trim().to_owned().into());
            }
            if !announced && (text.contains("REBOOTING") || text.contains("Restarting")) {
                self.log("startosinstall asked the guest to reboot");
                announced = true;
            }
            if qmp.is_none() {
                match Qmp::connect(&self.vm_dir().join("qmp.sock")) {
                    Ok(conn) => qmp = Some(conn),
                    Err(_) => {
                        if !self.running() {
                            self.log("qemu exited");
                            return Ok(());
                        }
                        std::thread::sleep(Duration::from_secs(2));
                        continue;
                    }
                }
            }
            let Some(conn) = qmp.as_mut() else { continue };
            match (|| -> Result<serde_json::Value> {
                conn.drain()?;
                conn.execute("query-status", None)
            })() {
                Err(err) => {
                    self.log(&format!("qemu connection closed ({err})"));
                    return Ok(());
                }
                Ok(status) => {
                    if conn
                        .events
                        .iter()
                        .any(|event| matches!(event.as_str(), "RESET" | "SHUTDOWN" | "POWERDOWN"))
                    {
                        self.log("guest is rebooting");
                        return Ok(());
                    }
                    let word = status
                        .get("status")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("");
                    if word == "shutdown" || word == "guest-panicked" {
                        self.log(&format!("guest status {word}"));
                        return Ok(());
                    }
                }
            }
            std::thread::sleep(Duration::from_secs(5));
        }
        Err("startosinstall never rebooted the guest".into())
    }

    fn log(&self, msg: &str) {
        let path = self.vm_dir().join("install.log");
        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(file, "{msg}");
        }
    }

    fn note(&self, summary: &str, body: &str) {
        self.log(body);
        notify(summary, body);
    }
}

fn plist_dict(pairs: &[(&str, Value)]) -> Value {
    let mut dict = plist::Dictionary::new();
    for (key, value) in pairs {
        dict.insert((*key).to_owned(), value.clone());
    }
    Value::Dictionary(dict)
}

fn user_plist(account: &str, password: &str, salt: &[u8; 32], uid: [u8; 16]) -> Result<Vec<u8>> {
    let hash = crate::shadow_hash(password, salt);
    let strings = |value: &str| Value::Array(vec![Value::String(value.to_owned())]);
    let mut dict = plist::Dictionary::new();
    dict.insert("name".into(), strings(account));
    dict.insert("realname".into(), strings(account));
    dict.insert("uid".into(), strings("501"));
    dict.insert("gid".into(), strings("20"));
    dict.insert("home".into(), strings(&format!("/Users/{account}")));
    dict.insert("shell".into(), strings("/bin/zsh"));
    dict.insert("generateduid".into(), strings(&crate::uuid_v4(uid)));
    dict.insert("passwd".into(), strings("********"));
    dict.insert("authentication_authority".into(), strings(";ShadowHash;"));
    dict.insert(
        "ShadowHashData".into(),
        Value::Array(vec![Value::Data(hash)]),
    );
    let mut out = Vec::new();
    Value::Dictionary(dict).to_writer_binary(&mut out)?;
    Ok(out)
}

fn agent_plist(label: &str, args: &[&str], interval: bool) -> Value {
    let mut dict = plist::Dictionary::new();
    dict.insert("Label".into(), Value::String(label.to_owned()));
    dict.insert("RunAtLoad".into(), Value::Boolean(true));
    if interval {
        dict.insert("StartInterval".into(), Value::from(20i64));
    }
    dict.insert(
        "ProgramArguments".into(),
        Value::Array(
            args.iter()
                .map(|arg| Value::String((*arg).to_owned()))
                .collect(),
        ),
    );
    Value::Dictionary(dict)
}

fn write_xml(path: PathBuf, value: &Value) -> Result<()> {
    let mut file = File::create(path)?;
    value.to_writer_xml(&mut file)?;
    Ok(())
}

fn write_bytes(path: PathBuf, bytes: &[u8]) -> Result<()> {
    fs::write(path, bytes)?;
    Ok(())
}

fn authorized_keys(ssh: &Path) -> Result<String> {
    let mut keys = vec![fs::read_to_string(ssh.join("id_ed25519.pub"))?
        .trim()
        .to_owned()];
    let user = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
        .join(".ssh/id_ed25519.pub");
    if user.is_file() {
        let text = fs::read_to_string(user)?.trim().to_owned();
        if !text.is_empty() && !keys.contains(&text) {
            keys.push(text);
        }
    }
    Ok(keys.join("\n") + "\n")
}

fn build_pkg(stage: &Path, pkg: &Path) -> Result<()> {
    let root = stage.join("root");
    let scripts = stage.join("scripts");
    let flat = stage.join("flat");
    for path in [&root, &scripts, &flat] {
        let _ = fs::remove_dir_all(path);
        fs::create_dir_all(path)?;
    }
    let lib = root.join("Library/macos-kvm");
    let daemons = root.join("Library/LaunchDaemons");
    fs::create_dir_all(&lib)?;
    fs::create_dir_all(&daemons)?;
    for name in [
        "user.plist",
        "kcpassword",
        "GlobalPreferences.plist",
        "loginwindow.plist",
        "bootstrap.sh",
        "authorized_keys",
        "guest_id_ed25519",
        "guest_id_ed25519.pub",
        "guest_ssh_config",
        "nsmb.conf",
        "share.sh",
        "com.macos-kvm.share.plist",
    ] {
        let source = stage.join(name);
        if source.is_file() {
            fs::copy(&source, lib.join(name))?;
        }
    }
    fs::set_permissions(lib.join("bootstrap.sh"), mode(0o755))?;
    let kcpassword = lib.join("kcpassword");
    if kcpassword.is_file() {
        fs::set_permissions(kcpassword, mode(0o600))?;
    }
    fs::copy(
        stage.join("com.macos-kvm.bootstrap.plist"),
        daemons.join("com.macos-kvm.bootstrap.plist"),
    )?;
    fs::write(
        scripts.join("postinstall"),
        "#!/bin/bash\nROOT=\"${3:-/}\"\nchmod 755 \"$ROOT/Library/macos-kvm/bootstrap.sh\" 2>/dev/null || true\nchmod 644 \"$ROOT/Library/LaunchDaemons/com.macos-kvm.bootstrap.plist\" 2>/dev/null || true\nexit 0\n",
    )?;
    fs::set_permissions(scripts.join("postinstall"), mode(0o755))?;
    run_at(
        "mkbom",
        &[
            "-u",
            "0",
            "-g",
            "0",
            root.to_str().unwrap(),
            flat.join("Bom").to_str().unwrap(),
        ],
        None,
    )?;
    cpio_gzip(&root, &flat.join("Payload"))?;
    cpio_gzip(&scripts, &flat.join("Scripts"))?;
    let files = walk_files(&root)?;
    let kbytes = std::cmp::max(1, fs::metadata(flat.join("Payload"))?.len() / 1024);
    fs::write(
        flat.join("PackageInfo"),
        format!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<pkg-info format-version=\"2\" identifier=\"com.macos-kvm.bootstrap\" version=\"1.0\" install-location=\"/\" auth=\"root\">\n  <payload numberOfFiles=\"{files}\" installKBytes=\"{kbytes}\"/>\n  <scripts>\n    <postinstall file=\"./postinstall\"/>\n  </scripts>\n</pkg-info>\n"
        ),
    )?;
    let _ = fs::remove_file(pkg);
    run_at(
        "xar",
        &[
            "--compression",
            "none",
            "-cf",
            pkg.to_str().unwrap(),
            "PackageInfo",
            "Bom",
            "Payload",
            "Scripts",
        ],
        Some(&flat),
    )?;
    let listing = output("xar", &["-tf", pkg.to_str().unwrap()])?;
    for name in ["PackageInfo", "Bom", "Payload", "Scripts"] {
        if !listing.split_whitespace().any(|word| word == name) {
            return Err(format!("package is missing {name}: {listing}").into());
        }
    }
    Ok(())
}

fn build_product(stage: &Path, component: &Path, product: &Path) -> Result<()> {
    let dist = stage.join("product");
    let _ = fs::remove_dir_all(&dist);
    fs::create_dir_all(&dist)?;
    fs::copy(component, dist.join("bootstrap-component.pkg"))?;
    fs::write(
        dist.join("Distribution"),
        "<?xml version=\"1.0\" encoding=\"utf-8\" standalone=\"no\"?>\n<installer-gui-script minSpecVersion=\"1\">\n    <title>macos-kvm</title>\n    <options customize=\"never\" require-scripts=\"false\" hostArchitectures=\"x86_64\"/>\n    <choices-outline>\n        <line choice=\"default\">\n            <line choice=\"com.macos-kvm.bootstrap\"/>\n        </line>\n    </choices-outline>\n    <choice id=\"default\"/>\n    <choice id=\"com.macos-kvm.bootstrap\" visible=\"false\" title=\"bootstrap\">\n        <pkg-ref id=\"com.macos-kvm.bootstrap\"/>\n    </choice>\n    <pkg-ref id=\"com.macos-kvm.bootstrap\" version=\"1.0\" onConclusion=\"none\">#bootstrap-component.pkg</pkg-ref>\n</installer-gui-script>\n",
    )?;
    let _ = fs::remove_file(product);
    run_at(
        "xar",
        &[
            "--compression",
            "none",
            "-cf",
            product.to_str().unwrap(),
            "Distribution",
            "bootstrap-component.pkg",
        ],
        Some(&dist),
    )?;
    let listing = output("xar", &["-tf", product.to_str().unwrap()])?;
    for name in ["Distribution", "bootstrap-component.pkg"] {
        if !listing.split_whitespace().any(|word| word == name) {
            return Err(format!("product archive is missing {name}: {listing}").into());
        }
    }
    Ok(())
}

fn walk_files(root: &Path) -> Result<u64> {
    let mut count = 0u64;
    for entry in fs::read_dir(root)? {
        let path = entry?.path();
        if path.is_dir() {
            count += walk_files(&path)?;
        } else if path.is_file() {
            count += 1;
        }
    }
    Ok(count)
}

fn cpio_gzip(src: &Path, dest: &Path) -> Result<()> {
    let mut find = Command::new("find")
        .args([".", "-print"])
        .current_dir(src)
        .stdout(Stdio::piped())
        .spawn()?;
    let mut cpio = Command::new("cpio")
        .args(["-o", "--format", "odc", "--owner", "0:0"])
        .current_dir(src)
        .stdin(find.stdout.take().unwrap())
        .stdout(Stdio::piped())
        .spawn()?;
    let mut gzip = Command::new("gzip")
        .arg("-c")
        .stdin(cpio.stdout.take().unwrap())
        .stdout(File::create(dest)?)
        .spawn()?;
    let codes = [
        find.wait()?.success(),
        cpio.wait()?.success(),
        gzip.wait()?.success(),
    ];
    if codes != [true, true, true] {
        return Err(format!("cpio pipeline failed: {codes:?}").into());
    }
    Ok(())
}

fn open_terminal(qmp: &mut Qmp, command: &str, close_front: bool) -> Result<()> {
    qmp.chord(&["esc"])?;
    std::thread::sleep(Duration::from_millis(300));
    if close_front {
        qmp.chord(&["meta_l", "q"])?;
        std::thread::sleep(Duration::from_secs(1));
    }
    qmp.chord(&["meta_l", "shift", "t"])?;
    std::thread::sleep(Duration::from_millis(1500));
    qmp.type_text(&format!("{command}\n"))?;
    Ok(())
}

fn download_running() -> bool {
    Command::new("pgrep")
        .args(["-f", "reims-util offline-installer"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn run(program: impl AsRef<std::ffi::OsStr>, args: &[&str]) -> Result<()> {
    run_at(program, args, None)
}

fn run_at(program: impl AsRef<std::ffi::OsStr>, args: &[&str], dir: Option<&Path>) -> Result<()> {
    let mut cmd = Command::new(program);
    cmd.args(args).stdout(Stdio::null());
    if let Some(dir) = dir {
        cmd.current_dir(dir);
    }
    let done = cmd.output()?;
    if done.status.success() {
        Ok(())
    } else {
        Err(format!("{}", String::from_utf8_lossy(&done.stderr)).into())
    }
}

fn output(program: impl AsRef<std::ffi::OsStr>, args: &[&str]) -> Result<String> {
    let done = Command::new(program).args(args).output()?;
    if done.status.success() {
        Ok(String::from_utf8_lossy(&done.stdout).into_owned())
    } else {
        Err(format!("{}", String::from_utf8_lossy(&done.stderr)).into())
    }
}

fn random_bytes(buf: &mut [u8]) -> Result<()> {
    getrandom::getrandom(buf).map_err(|err| err.to_string())?;
    Ok(())
}

fn mode(bits: u32) -> std::fs::Permissions {
    std::fs::Permissions::from_mode(bits)
}

struct Lock {
    _file: File,
}

impl Lock {
    fn acquire(path: &Path) -> Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(path)?;
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
        if rc != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(Self { _file: file })
    }
}
