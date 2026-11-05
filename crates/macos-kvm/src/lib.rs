//! Silent setup of a macOS KVM guest, and the menu that runs it.
//!
//! The pieces that decide a password, an identity, an SSH path, or a display
//! mode are pure. The install that talks to QEMU sits behind them.

use std::fmt::Write as _;
use std::io::Read;
use std::net::{TcpStream, ToSocketAddrs};
use std::path::Path;
use std::time::Duration;

pub mod install;
pub mod qmp;
pub mod session;
pub mod sync;
pub mod tray;

const KCPASSWORD_KEY: [u8; 11] = [
    0x7D, 0x89, 0x52, 0x23, 0xD2, 0xBC, 0xDD, 0xEA, 0xA3, 0xB9, 0x1F,
];

/// Apple OUIs osx-serial-generator picks a ROM from.
const APPLE_OUIS: [&str; 15] = [
    "00:03:93", "00:0A:95", "00:11:24", "00:16:CB", "00:1C:B3", "00:1E:52", "00:23:DF", "00:25:4B",
    "00:26:BB", "28:CF:E9", "3C:07:54", "60:03:08", "7C:6D:62", "A4:B1:97", "F0:18:98",
];

/// macOS auto-login secret. The password, a NUL, then NUL padding out to a
/// multiple of the 11-byte key, each byte XOR that key.
pub fn kcpassword(password: &str) -> Vec<u8> {
    let mut raw = password.as_bytes().to_vec();
    raw.push(0);
    let pad = (11 - raw.len() % 11) % 11;
    raw.extend(std::iter::repeat(0).take(pad));
    raw.iter()
        .enumerate()
        .map(|(i, byte)| byte ^ KCPASSWORD_KEY[i % 11])
        .collect()
}

/// SALTED-SHA512-PBKDF2 blob macOS stores as a user's ShadowHashData.
pub fn shadow_hash(password: &str, salt: &[u8; 32]) -> Vec<u8> {
    let mut entropy = [0u8; 128];
    pbkdf2::pbkdf2_hmac::<sha2::Sha512>(password.as_bytes(), salt, 39999, &mut entropy);
    let root = plist::Value::Dictionary({
        let mut body = plist::Dictionary::new();
        body.insert("entropy".into(), plist::Value::Data(entropy.to_vec()));
        body.insert("iterations".into(), plist::Value::from(39999i64));
        body.insert("salt".into(), plist::Value::Data(salt.to_vec()));
        let mut root = plist::Dictionary::new();
        root.insert(
            "SALTED-SHA512-PBKDF2".into(),
            plist::Value::Dictionary(body),
        );
        root
    });
    let mut out = Vec::new();
    root.to_writer_binary(&mut out)
        .expect("a shadow hash plist is a fixed shape");
    out
}

/// `macserial --num 1 --model iMac20,1` prints `SERIAL | MLB`.
pub fn parse_macserial(line: &str) -> Result<(String, String), String> {
    let (serial, mlb) = line
        .trim()
        .split_once('|')
        .ok_or_else(|| format!("macserial returned {line:?}"))?;
    let serial = serial.trim();
    let mlb = mlb.trim();
    if serial.len() != 12 || mlb.len() != 17 {
        return Err(format!("macserial returned {line:?}"));
    }
    Ok((serial.to_owned(), mlb.to_owned()))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identity {
    pub serial: String,
    pub mlb: String,
    pub uuid: String,
    pub mac: String,
}

pub fn format_identity(id: &Identity) -> String {
    format!(
        "export DEVICE_MODEL=\"iMac20,1\"\nexport SERIAL=\"{}\"\nexport BOARD_SERIAL=\"{}\"\nexport UUID=\"{}\"\nexport MAC_ADDRESS=\"{}\"\n",
        id.serial, id.mlb, id.uuid, id.mac
    )
}

/// Reads an identity written by [`format_identity`]. Missing any field is none.
pub fn parse_identity(text: &str) -> Option<Identity> {
    let mut serial = None;
    let mut mlb = None;
    let mut uuid = None;
    let mut mac = None;
    for line in text.lines() {
        let rest = line.strip_prefix("export ")?;
        let (key, raw) = rest.split_once('=')?;
        let value = raw.trim().trim_matches('"').to_owned();
        if value.is_empty() {
            continue;
        }
        match key {
            "SERIAL" => serial = Some(value),
            "BOARD_SERIAL" => mlb = Some(value),
            "UUID" => uuid = Some(value),
            "MAC_ADDRESS" => mac = Some(value),
            _ => {}
        }
    }
    Some(Identity {
        serial: serial?,
        mlb: mlb?,
        uuid: uuid?,
        mac: mac?,
    })
}

/// UUID version 4 from 16 random bytes.
pub fn uuid_v4(mut bytes: [u8; 16]) -> String {
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    format!(
        "{:02X}{:02X}{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}",
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7], bytes[8],
        bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15]
    )
}

/// A ROM in an Apple OUI, `prefix` selecting which one.
pub fn apple_mac(prefix: u8, tail: [u8; 3]) -> String {
    format!(
        "{}:{:02X}:{:02X}:{:02X}",
        APPLE_OUIS[prefix as usize % APPLE_OUIS.len()],
        tail[0],
        tail[1],
        tail[2]
    )
}

/// Guest pixels that fill `points` of window at `scale` guest pixels per point.
/// A multiple of 8, at least `min`, at most 7680.
pub fn guest_pixels(points: f64, scale: u8, min: u16) -> Option<u16> {
    let px = points * f64::from(scale);
    if !px.is_finite() || px <= 0.0 {
        return None;
    }
    let rounded = (px / 8.0).round() * 8.0;
    let value = (rounded as u32).clamp(u32::from(min), 7680);
    u16::try_from(value).ok()
}

/// SSH config for the guest. `%C` is left for ssh to expand.
pub fn ssh_config(
    name: &str,
    account: &str,
    port: u16,
    key: &Path,
    known_hosts: &Path,
    runtime: &str,
) -> String {
    let key = key.display();
    let known_hosts = known_hosts.display();
    format!(
        "\
Host {name} macos macos-kvm
  HostName 127.0.0.1
  Port {port}
  User {account}
  IdentityFile {key}
  IdentitiesOnly yes
  ControlMaster auto
  ControlPath {runtime}/macos-kvm-%C
  ControlPersist 10m
  Compression no
  Ciphers aes128-gcm@openssh.com
  IPQoS none
  StrictHostKeyChecking accept-new
  UserKnownHostsFile {known_hosts}
"
    )
}

/// A control socket path must fit in a Unix socket name: 108 bytes, NUL included.
/// `%C` expands to 40 hex characters.
pub fn control_socket_fits(runtime: &str) -> bool {
    let path = format!("{runtime}/macos-kvm-{}", "f".repeat(40));
    path.len() < 108
}

/// `show` text from reims-ctl. The state word is the second field.
pub fn vm_running(show: &str) -> bool {
    show.lines().any(|line| {
        let mut words = line.split_whitespace();
        words.next() == Some("state") && words.next() == Some("running")
    })
}

pub fn installer_attached(show: &str) -> bool {
    show.lines().any(|line| {
        let mut words = line.split_whitespace();
        words.next() == Some("installer") && words.next().is_some_and(|word| word != "none")
    })
}

/// The guest sshd is up only when the banner arrives. slirp accepts the
/// forward while sshd is still down.
pub fn ssh_banner_up(port: u16) -> bool {
    let address = match ("127.0.0.1", port).to_socket_addrs() {
        Ok(mut addrs) => match addrs.next() {
            Some(addr) => addr,
            None => return false,
        },
        Err(_) => return false,
    };
    let mut stream = match TcpStream::connect_timeout(&address, Duration::from_secs(2)) {
        Ok(stream) => stream,
        Err(_) => return false,
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let mut buf = [0u8; 32];
    match stream.read(&mut buf) {
        Ok(n) if n >= 4 => buf.starts_with(b"SSH-"),
        _ => false,
    }
}

/// One replaced notification, so a long install does not stack banners.
pub fn notify(summary: &str, body: &str) {
    let _ = std::process::Command::new("notify-send")
        .args(["-a", "macos-kvm", "-r", "26026", summary, body])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

/// 16 hex characters. The account password, when the user did not choose one.
pub fn random_password() -> String {
    let mut bytes = [0u8; 8];
    getrandom::getrandom(&mut bytes).expect("the system random source");
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kcpassword_matches_the_known_vector() {
        assert_eq!(kcpassword("secret"), hex("0eec3151b7c8ddeaa3b91f"));
        assert_eq!(kcpassword("secret").len() % 11, 0);
        let longer = kcpassword("0123456789abcdef");
        assert_eq!(longer.len() % 11, 0);
        assert!(longer.len() > 16);
    }

    #[test]
    fn shadow_hash_is_the_salted_sha512_macos_stores() {
        let salt = hex("00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff");
        let salt: [u8; 32] = salt.try_into().unwrap();
        let bytes = shadow_hash("secret", &salt);
        let value = plist::Value::from_reader(std::io::Cursor::new(bytes)).unwrap();
        let body = value
            .as_dictionary()
            .unwrap()
            .get("SALTED-SHA512-PBKDF2")
            .unwrap()
            .as_dictionary()
            .unwrap();
        assert_eq!(
            body.get("iterations").unwrap().as_signed_integer(),
            Some(39999)
        );
        assert_eq!(
            body.get("entropy").unwrap().as_data().unwrap(),
            hex("4a7eb3eef6b5be6da7dc418f0ff6b297e9b3ef9d992251d67935462a2873737f9d6fb680742a3e1fc6224b20067fb0c9c3cbc77c23fff7120eecd3cf5fe9c3a032f052365491ecd1cb81ae818ff5b2301960b679838f91dea619a1bccfc6902fc0e810777547e347a09686bb6c1e261174445655370a63a79a2617d2c651e21f").as_slice()
        );
    }

    #[test]
    fn macserial_output_is_a_serial_and_a_board_serial() {
        let (serial, mlb) = parse_macserial("C02GP5YNPN5T | C02146207QXPHC1JC\n").unwrap();
        assert_eq!(serial, "C02GP5YNPN5T");
        assert_eq!(mlb, "C02146207QXPHC1JC");
        assert!(parse_macserial("short | also-short").is_err());
        assert!(parse_macserial("no bar").is_err());
    }

    #[test]
    fn an_identity_file_round_trips() {
        let id = Identity {
            serial: "C02GP5YNPN5T".into(),
            mlb: "C02146207QXPHC1JC".into(),
            uuid: "49C95E9E-6BBA-4FB4-94A1-1E99AE475C4E".into(),
            mac: "28:CF:E9:5F:72:2E".into(),
        };
        let parsed = parse_identity(&format_identity(&id)).unwrap();
        assert_eq!(parsed, id);
        assert!(parse_identity("export SERIAL=\"only\"\n").is_none());
    }

    #[test]
    fn uuid_v4_sets_the_version_and_variant() {
        let id = uuid_v4([0xff; 16]);
        assert_eq!(id, "FFFFFFFF-FFFF-4FFF-BFFF-FFFFFFFFFFFF");
        assert_eq!(id.len(), 36);
    }

    #[test]
    fn a_rom_uses_an_apple_oui() {
        let mac = apple_mac(9, [0x5F, 0x72, 0x2E]);
        assert_eq!(mac, "28:CF:E9:5F:72:2E");
        assert!(APPLE_OUIS.iter().any(|oui| mac.starts_with(oui)));
    }

    #[test]
    fn a_window_resize_keeps_the_guest_scale() {
        assert_eq!(guest_pixels(1920.0, 2, 640), Some(3840));
        assert_eq!(guest_pixels(1600.0, 2, 640), Some(3200));
        assert_eq!(guest_pixels(900.0, 2, 480), Some(1800));
        assert_eq!(guest_pixels(1920.4, 2, 640), Some(3840));
        assert_eq!(guest_pixels(100.0, 2, 640), Some(640));
        assert_eq!(guest_pixels(f64::NAN, 1, 640), None);
    }

    #[test]
    fn the_control_socket_lives_in_the_runtime_dir() {
        let runtime = "/run/user/1000";
        let config = ssh_config(
            "tahoe",
            "alex",
            2222,
            Path::new("/tmp/id_ed25519"),
            Path::new("/tmp/known_hosts"),
            runtime,
        );
        assert!(config.contains("ControlPath /run/user/1000/macos-kvm-%C"));
        assert!(control_socket_fits(runtime));
        // 108 bytes including the terminating NUL. A long home directory plus
        // the VM path crosses it; the runtime dir does not.
        assert!(!control_socket_fits(&format!(
            "/home/{}/.local/share/reims-ctl/vms/tahoe/ssh",
            "n".repeat(40)
        )));
    }

    #[test]
    fn show_text_says_whether_the_guest_and_the_installer_are_up() {
        let show = "  state    running (pid 1)\n  installer none\n";
        assert!(vm_running(show));
        assert!(!installer_attached(show));
        assert!(!vm_running("  state    stopped\n"));
        assert!(installer_attached("installer /tmp/macos.img\n"));
    }

    #[test]
    fn an_ssh_banner_is_the_only_sign_sshd_is_up() {
        use std::io::Write;
        use std::net::TcpListener;
        use std::thread;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let _ = sock.write_all(b"SSH-2.0-test\r\n");
        });
        assert!(ssh_banner_up(port));
        let closed = TcpListener::bind("127.0.0.1:0").unwrap();
        let closed_port = closed.local_addr().unwrap().port();
        drop(closed);
        assert!(!ssh_banner_up(closed_port));
    }

    #[test]
    fn the_guest_scripts_skip_setup_and_agree_to_the_license() {
        let boot = include_str!("../guest/bootstrap.sh");
        assert!(boot.contains("rm -f /var/db/.AppleSetupTermsOfService"));
        assert!(boot.contains("touch /var/db/.AppleSetupDone"));
        assert!(boot.contains("SHARED_FOLDER"));
        assert!(boot.contains("if [ -f \"$SRC/kcpassword\" ]"));
        let plant = include_str!("../guest/plant.sh");
        assert!(plant.contains("macOS Base System"));
        assert!(plant.contains("FAIL no data volume"));
        assert!(plant.contains("PLANTED"));
        assert!(plant.contains("@ACCOUNT@"));
        assert!(plant.contains(".AppleSetupTermsOfService"));
        let go = include_str!("../guest/go.sh");
        assert!(go.contains("--agreetolicense --nointeraction"));
        assert!(go.contains("--forcequitapps"));
        assert!(go.contains("CM_BUILD=CM_BUILD installer -pkg"));
    }

    fn hex(text: &str) -> Vec<u8> {
        (0..text.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
            .collect()
    }
}
