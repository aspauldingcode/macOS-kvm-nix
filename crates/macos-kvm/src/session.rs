//! The `macos-kvm` command. A graphical session runs `ensure`, which installs
//! once and then stays up for as long as the guest does.

use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use crate::install::Install;

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

pub struct Config {
    pub name: String,
    pub account: String,
    pub host_user: String,
    pub macos: String,
    pub disk_size: String,
    pub resolution: String,
    pub hidpi: bool,
    pub borderless: bool,
    pub audio: bool,
    pub ssh_port: u16,
    pub cpus: Option<String>,
    pub memory: Option<String>,
    pub offline: bool,
    pub gpu: String,
    pub password: Option<String>,
    pub password_file: Option<PathBuf>,
    pub login_window: bool,
    pub reims: PathBuf,
    pub util: PathBuf,
    pub macserial: PathBuf,
    pub share: PathBuf,
    pub home: PathBuf,
    pub runtime: PathBuf,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or("HOME is not set")?;
        let runtime = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(format!("/run/user/{}", uid())));
        Ok(Self {
            name: env_or("MACOS_KVM_NAME", "tahoe"),
            account: env_or("MACOS_KVM_ACCOUNT", "alex"),
            host_user: env_or("MACOS_KVM_HOST_USER", &env_or("MACOS_KVM_ACCOUNT", "alex")),
            macos: env_or("MACOS_KVM_MACOS", "26"),
            disk_size: env_or("MACOS_KVM_DISK", "128G"),
            resolution: env_or("MACOS_KVM_RESOLUTION", "3840x2160"),
            hidpi: env_flag("MACOS_KVM_HIDPI", true),
            borderless: env_flag("MACOS_KVM_BORDERLESS", false),
            audio: env_flag("MACOS_KVM_AUDIO", true),
            ssh_port: env_or("MACOS_KVM_SSH_PORT", "2222").parse().unwrap_or(2222),
            cpus: std::env::var("MACOS_KVM_CPUS")
                .ok()
                .filter(|v| !v.is_empty()),
            memory: std::env::var("MACOS_KVM_MEMORY")
                .ok()
                .filter(|v| !v.is_empty()),
            offline: env_flag("MACOS_KVM_OFFLINE", true),
            gpu: env_or("MACOS_KVM_GPU", "auto"),
            password: std::env::var("MACOS_KVM_PASSWORD")
                .ok()
                .filter(|v| !v.is_empty()),
            password_file: std::env::var("MACOS_KVM_PASSWORD_FILE")
                .ok()
                .filter(|v| !v.is_empty())
                .map(PathBuf::from),
            login_window: env_flag("MACOS_KVM_LOGIN_WINDOW", false),
            reims: require("MACOS_KVM_REIMS")?,
            util: require("MACOS_KVM_UTIL")?,
            macserial: require("MACOS_KVM_MACSERIAL")?,
            share: std::env::var("MACOS_KVM_SHARE")
                .map(PathBuf::from)
                .unwrap_or_else(|_| home.join("SHARED_FOLDER")),
            home,
            runtime,
        })
    }

    fn install(&self) -> Install {
        Install {
            name: self.name.clone(),
            account: self.account.clone(),
            host_user: self.host_user.clone(),
            macos: self.macos.clone(),
            ssh_port: self.ssh_port,
            password: self.password.clone(),
            password_file: self.password_file.clone(),
            login_window: self.login_window,
            audio: self.audio,
            reims: self.reims.clone(),
            util: self.util.clone(),
            macserial: self.macserial.clone(),
            home: self.home.clone(),
        }
    }
}

pub fn cli() -> Result<()> {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        None | Some("open") | Some("ensure") => ensure(&Config::from_env()?),
        Some("ssh") => ssh(&Config::from_env()?, args),
        Some("account") => account(&Config::from_env()?),
        Some("tray") => crate::tray::run(&Config::from_env()?),
        Some("install") => {
            Config::from_env()?.install().run()?;
            Ok(())
        }
        Some("sync") => sync_account(&Config::from_env()?),
        Some(_) => passthrough(),
    }
}

fn ensure(cfg: &Config) -> Result<()> {
    create(cfg)?;
    publish(cfg)?;
    cfg.install().run()?;
    sync_account(cfg)?;
    if !running(cfg) {
        run(&cfg.reims, &["start", &cfg.name])?;
    }
    // Type=simple runs ExecStop when this process exits, and ExecStop powers
    // the guest off. Stay alive for as long as the guest does.
    while running(cfg) {
        std::thread::sleep(Duration::from_secs(5));
    }
    Ok(())
}

fn create(cfg: &Config) -> Result<()> {
    if output(&cfg.reims, &["show", &cfg.name]).is_ok() {
        return Ok(());
    }
    let dir = cfg.home.join(".local/share/reims-ctl/vms").join(&cfg.name);
    if dir.is_dir() && !dir.join("vm.toml").is_file() {
        fs::remove_dir_all(&dir)?;
    }
    let _ = Command::new(&cfg.reims)
        .args(["delete", &cfg.name, "--yes"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    let mut args = vec![
        "create".to_owned(),
        cfg.name.clone(),
        "--macos".into(),
        cfg.macos.clone(),
        "--disk-size".into(),
        cfg.disk_size.clone(),
        "--resolution".into(),
        cfg.resolution.clone(),
        "--hidpi".into(),
        bool_word(cfg.hidpi),
        "--borderless".into(),
        bool_word(cfg.borderless),
        "--audio".into(),
        bool_word(cfg.audio),
        "--logs".into(),
        "--ssh-port".into(),
        cfg.ssh_port.to_string(),
        "--gpu".into(),
        cfg.gpu.clone(),
    ];
    if let Some(cpus) = &cfg.cpus {
        args.push("--cpus".into());
        args.push(cpus.clone());
    }
    if let Some(memory) = &cfg.memory {
        args.push("--memory".into());
        args.push(memory.clone());
    }
    if cfg.offline {
        args.push("--offline".into());
    }
    let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
    run(&cfg.reims, &borrowed)
}

pub fn publish(cfg: &Config) -> Result<()> {
    let install = cfg.install();
    let ssh = install.ensure_keys()?;
    fs::create_dir_all(&cfg.share)?;
    fs::set_permissions(&cfg.share, std::fs::Permissions::from_mode(0o755))?;
    let ssh_dir = cfg.home.join(".ssh");
    fs::create_dir_all(&ssh_dir)?;
    fs::set_permissions(&ssh_dir, std::fs::Permissions::from_mode(0o700))?;
    fs::create_dir_all(&cfg.runtime)?;
    let config = crate::ssh_config(
        &cfg.name,
        &guest_account(cfg),
        cfg.ssh_port,
        &ssh.join("id_ed25519"),
        &ssh.join("known_hosts"),
        &cfg.runtime.to_string_lossy(),
    );
    if !crate::control_socket_fits(&cfg.runtime.to_string_lossy()) {
        return Err("the SSH control socket path does not fit a Unix socket name".into());
    }
    let path = ssh.join("config");
    fs::write(&path, config)?;
    fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    let keys = ssh_dir.join("authorized_keys");
    if keys.is_symlink() {
        return Ok(());
    }
    let public = fs::read_to_string(ssh.join("guest_id_ed25519.pub"))?;
    let public = public.trim();
    let mut existing = String::new();
    if keys.is_file() {
        fs::File::open(&keys)?.read_to_string(&mut existing)?;
    }
    if !existing.lines().any(|line| line == public) {
        let mut file = OpenOptions::new().create(true).append(true).open(&keys)?;
        writeln!(file, "{public}")?;
        fs::set_permissions(&keys, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// The short name the guest was installed with. A later change of
/// `accountName` does not rename that account.
fn guest_account(cfg: &Config) -> String {
    let path = cfg
        .home
        .join(".local/share/reims-ctl/vms")
        .join(&cfg.name)
        .join("account.name");
    fs::read_to_string(path)
        .ok()
        .map(|text| text.trim().to_owned())
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| cfg.account.clone())
}

fn sync_account(cfg: &Config) -> Result<()> {
    let install = cfg.install();
    let dir = install.vm_dir();
    if !dir.join("headless-installed").is_file() {
        return Ok(());
    }
    let Some(old) = install.stored_password() else {
        return Ok(());
    };
    let stored_id = fs::read_to_string(dir.join("password.sha256")).unwrap_or_default();
    let Some(update) = crate::sync::plan(
        &old,
        cfg.password.as_deref(),
        &stored_id,
        crate::sync::stored_login_window(&dir),
        cfg.login_window,
    ) else {
        return Ok(());
    };
    if !running(cfg) {
        run(
            &cfg.reims,
            &["set", &cfg.name, "--audio", &bool_word(cfg.audio)],
        )?;
        run(&cfg.reims, &["start", &cfg.name])?;
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(180);
    while !crate::ssh_banner_up(cfg.ssh_port) {
        if std::time::Instant::now() > deadline {
            return Err("the guest SSH server did not come up to apply the new password".into());
        }
        std::thread::sleep(Duration::from_secs(2));
    }
    let account = guest_account(cfg);
    let script = crate::sync::guest_script(&account);
    let kc = crate::sync::encode_base64(&crate::kcpassword(&update.new));
    let config = dir.join("ssh/config");
    // One remote command. Separate arguments are joined by the guest's zsh,
    // and that shell then never passes stdin on to bash.
    let remote = format!("bash -c {}", zsh_quote(&script));
    let mut child = Command::new("ssh")
        .args([
            "-F",
            &config.to_string_lossy(),
            "-o",
            "BatchMode=yes",
            "-o",
            "ControlMaster=no",
            "-o",
            "ControlPath=none",
            &cfg.name,
            &remote,
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    {
        use std::io::Write;
        let mut stdin = child.stdin.take().unwrap();
        writeln!(stdin, "{}", update.old)?;
        writeln!(stdin, "{}", update.new)?;
        writeln!(
            stdin,
            "{}",
            if update.login_window { "true" } else { "false" }
        )?;
        writeln!(stdin, "{kc}")?;
    }
    let done = child.wait_with_output()?;
    if !done.status.success() {
        return Err(format!(
            "the guest rejected the password change: {}",
            String::from_utf8_lossy(&done.stderr)
        )
        .into());
    }
    install.write_password_file(&update.new)?;
    crate::sync::record(&dir, &update.new, update.login_window)?;
    crate::notify("macOS", "The guest account was updated.");
    Ok(())
}

fn ssh(cfg: &Config, rest: impl Iterator<Item = String>) -> Result<()> {
    publish(cfg)?;
    let config = cfg
        .home
        .join(".local/share/reims-ctl/vms")
        .join(&cfg.name)
        .join("ssh/config");
    let mut cmd = Command::new("ssh");
    cmd.arg("-F").arg(config).arg(&cfg.name);
    cmd.args(rest);
    let err = cmd.exec();
    Err(err.into())
}

fn account(cfg: &Config) -> Result<()> {
    let path = cfg
        .home
        .join(".local/share/reims-ctl/vms")
        .join(&cfg.name)
        .join("account.password");
    let mut text = String::new();
    fs::File::open(path)?.read_to_string(&mut text)?;
    print!("{text}");
    if !text.ends_with('\n') {
        println!();
    }
    Ok(())
}

fn passthrough() -> Result<()> {
    let cfg = Config::from_env()?;
    let err = Command::new(&cfg.reims)
        .args(std::env::args_os().skip(1))
        .exec();
    Err(err.into())
}

fn running(cfg: &Config) -> bool {
    output(&cfg.reims, &["show", &cfg.name]).is_ok_and(|text| crate::vm_running(&text))
}

fn run(program: &Path, args: &[&str]) -> Result<()> {
    let done = Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()?;
    if done.status.success() {
        Ok(())
    } else {
        let err = String::from_utf8_lossy(&done.stderr);
        Err(format!("{}: {err}", program.display()).into())
    }
}

fn output(program: &Path, args: &[&str]) -> Result<String> {
    let done = Command::new(program).args(args).output()?;
    if done.status.success() {
        Ok(String::from_utf8_lossy(&done.stdout).into_owned())
    } else {
        Err(format!("{}", String::from_utf8_lossy(&done.stderr)).into())
    }
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key)
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| default.to_owned())
}

fn env_flag(key: &str, default: bool) -> bool {
    match std::env::var(key).ok().as_deref() {
        Some("true" | "1") => true,
        Some("false" | "0") => false,
        _ => default,
    }
}

fn require(key: &str) -> Result<PathBuf> {
    std::env::var(key)
        .ok()
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| format!("{key} is not set").into())
}

fn zsh_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

fn bool_word(value: bool) -> String {
    if value {
        "true".into()
    } else {
        "false".into()
    }
}

fn uid() -> u32 {
    unsafe { libc::geteuid() }
}
