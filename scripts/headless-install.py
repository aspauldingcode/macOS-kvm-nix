#!/usr/bin/env python3
"""Install macOS without showing the Recovery installer.

Recovery's Terminal shortcut is Command-Shift-T. This types it once, on the
guest's USB keyboard through QEMU's QMP socket, without screenshots or OCR.
The FAT disk's script then runs startosinstall the way mist does: the
installer package is expanded onto the target, and startosinstall is
--agreetolicense --nointeraction --forcequitapps --volume. Nothing is typed
after that. Apple's image is ejected on the reboot so OpenCore boots the
internal disk instead of returning to the Recovery window.
"""

from __future__ import annotations

import argparse
import fcntl
import hashlib
import json
import os
import plistlib
import shutil
import socket
import subprocess
import time
import uuid
from pathlib import Path

HOLD_MS = 40
GAP_S = 0.08
LABEL = "MACOSKVM"
# sfdisk's default GPT alignment. mtools addresses the FAT partition here.
PART_OFFSET = 1024 * 1024
# The Apple installer image is about 20 GB. The guest disk has to be larger.
MIN_DISK_BYTES = 40 * 1024 * 1024 * 1024


def log(msg: str) -> None:
    print(f"macos-kvm: {msg}", flush=True)


def run(args: list[str]) -> None:
    log("+ " + " ".join(args))
    subprocess.run(args, check=True)


def capture(args: list[str]) -> str:
    return subprocess.run(args, check=True, text=True, capture_output=True).stdout


class Qmp:
    def __init__(self, path: Path):
        self.s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.s.connect(str(path))
        self.s.settimeout(10)
        self.buf = b""
        self.events: list[str] = []
        greet = self._read()
        if "QMP" not in greet:
            raise RuntimeError(f"not a QMP socket: {greet}")
        self.execute("qmp_capabilities")

    def _read(self) -> dict:
        while b"\n" not in self.buf:
            chunk = self.s.recv(1 << 20)
            if not chunk:
                raise RuntimeError("QMP closed")
            self.buf += chunk
        line, _, self.buf = self.buf.partition(b"\n")
        return json.loads(line)

    def execute(self, command: str, arguments: dict | None = None) -> dict:
        msg: dict = {"execute": command}
        if arguments is not None:
            msg["arguments"] = arguments
        self.s.sendall((json.dumps(msg) + "\n").encode())
        while True:
            reply = self._read()
            if "event" in reply:
                self.events.append(reply["event"])
                continue
            if "error" in reply:
                raise RuntimeError(f"QMP {command}: {reply['error']}")
            if "return" in reply:
                return reply["return"]

    def drain(self) -> None:
        self.s.settimeout(0.2)
        try:
            while True:
                reply = self._read()
                if "event" in reply:
                    self.events.append(reply["event"])
        except (TimeoutError, socket.timeout):
            pass
        finally:
            self.s.settimeout(10)

    def close(self) -> None:
        self.s.close()

    def chord(self, *names: str) -> None:
        self.execute(
            "send-key",
            {
                "hold-time": HOLD_MS,
                "keys": [{"type": "qcode", "data": n} for n in names],
            },
        )

    def type_text(self, text: str) -> None:
        special = {
            " ": "spc",
            "\n": "ret",
            "/": "slash",
            "-": "minus",
            ".": "dot",
            ";": "semicolon",
        }
        for ch in text:
            if ch in special:
                self.chord(special[ch])
            elif ch.isupper():
                self.chord("shift", ch.lower())
            elif ch.islower() or ch.isdigit():
                self.chord(ch)
            else:
                raise RuntimeError(f"cannot type {ch!r}")
            time.sleep(GAP_S)


def kcpassword(password: str) -> bytes:
    key = bytes([0x7D, 0x89, 0x52, 0x23, 0xD2, 0xBC, 0xDD, 0xEA, 0xA3, 0xB9, 0x1F])
    raw = password.encode() + b"\x00"
    raw += b"\x00" * ((11 - len(raw) % 11) % 11)
    return bytes(b ^ key[i % 11] for i, b in enumerate(raw))


def shadow_hash(password: str) -> bytes:
    salt = os.urandom(32)
    entropy = hashlib.pbkdf2_hmac("sha512", password.encode(), salt, 39999, dklen=128)
    return plistlib.dumps(
        {
            "SALTED-SHA512-PBKDF2": {
                "entropy": entropy,
                "iterations": 39999,
                "salt": salt,
            }
        },
        fmt=plistlib.FMT_BINARY,
    )


APPLE_OUIS = (
    "00:03:93",
    "00:0A:95",
    "00:11:24",
    "00:16:CB",
    "00:1C:B3",
    "00:1E:52",
    "00:23:DF",
    "00:25:4B",
    "00:26:BB",
    "28:CF:E9",
    "3C:07:54",
    "60:03:08",
    "7C:6D:62",
    "A4:B1:97",
    "F0:18:98",
)


def generate_identity(directory: Path, macserial: str) -> dict[str, str]:
    """One iMac20,1 identity, the way osx-serial-generator asks macserial for it.

    The values are written into reims-util's OpenCore. That image keeps
    ReimsVgpu.kext. The generator's own boot disk does not, so it is not used.
    """
    env_path = directory / "identity.env"
    if env_path.exists():
        values = {}
        for line in env_path.read_text().splitlines():
            if not line.startswith("export "):
                continue
            key, _, raw = line.removeprefix("export ").partition("=")
            values[key] = raw.strip().strip('"')
        needed = ("SERIAL", "BOARD_SERIAL", "UUID", "MAC_ADDRESS")
        if all(values.get(k) for k in needed):
            return {
                "serial": values["SERIAL"],
                "mlb": values["BOARD_SERIAL"],
                "uuid": values["UUID"],
                "mac": values["MAC_ADDRESS"],
            }
    line = capture([macserial, "--num", "1", "--model", "iMac20,1"]).strip()
    serial, mlb = [part.strip() for part in line.split("|", 1)]
    if len(serial) != 12 or len(mlb) != 17:
        raise RuntimeError(f"macserial returned {line!r}")
    system_uuid = str(uuid.uuid4()).upper()
    prefix = APPLE_OUIS[int.from_bytes(os.urandom(1), "big") % len(APPLE_OUIS)]
    tail = ":".join(f"{b:02X}" for b in os.urandom(3))
    mac = f"{prefix}:{tail}"
    env_path.write_text(
        "\n".join(
            [
                "export DEVICE_MODEL=\"iMac20,1\"",
                f"export SERIAL=\"{serial}\"",
                f"export BOARD_SERIAL=\"{mlb}\"",
                f"export UUID=\"{system_uuid}\"",
                f"export MAC_ADDRESS=\"{mac}\"",
                "",
            ]
        )
    )
    os.chmod(env_path, 0o600)
    return {"serial": serial, "mlb": mlb, "uuid": system_uuid, "mac": mac}


def apply_identity(reims: str, util: str, name: str, macserial: str) -> None:
    directory = vm_dir(name)
    if (directory / "identity.applied").exists():
        return
    ident = generate_identity(directory, macserial)
    log(f"OpenCore identity {ident['serial']} ({ident['mac']})")
    stop_vm(reims, name)
    run([reims, "set", name, "--mac", ident["mac"]])
    fresh = directory / "opencore.new.qcow2"
    if fresh.exists():
        fresh.unlink()
    run(
        [
            util,
            "opencore",
            "-o",
            str(fresh),
            "--model",
            "iMac20,1",
            "--serial",
            ident["serial"],
            "--mlb",
            ident["mlb"],
            "--uuid",
            ident["uuid"],
            "--rom",
            ident["mac"],
            "--no-ovmf",
        ]
    )
    dest = directory / "opencore.qcow2"
    dest.unlink(missing_ok=True)
    fresh.rename(dest)
    (directory / "identity.applied").write_text("ok\n")


def ensure_ssh_keys(directory: Path) -> Path:
    ssh_dir = directory / "ssh"
    ssh_dir.mkdir(parents=True, exist_ok=True)
    os.chmod(ssh_dir, 0o700)
    for name, comment in (
        ("id_ed25519", "macos-kvm"),
        ("guest_id_ed25519", "macos-kvm-guest"),
    ):
        private = ssh_dir / name
        if not private.exists():
            run(
                [
                    "ssh-keygen",
                    "-t",
                    "ed25519",
                    "-N",
                    "",
                    "-C",
                    comment,
                    "-f",
                    str(private),
                ]
            )
        os.chmod(private, 0o600)
    return ssh_dir


def authorized_keys(ssh_dir: Path) -> str:
    keys = [(ssh_dir / "id_ed25519.pub").read_text().strip()]
    user_key = Path.home() / ".ssh" / "id_ed25519.pub"
    if user_key.exists():
        text = user_key.read_text().strip()
        if text and text not in keys:
            keys.append(text)
    return "\n".join(keys) + "\n"


def account_files(account: str, password: str, dest: Path, ssh_dir: Path, host_user: str) -> None:
    dest.mkdir(parents=True, exist_ok=True)
    user = {
        "name": [account],
        "realname": [account],
        "uid": ["501"],
        "gid": ["20"],
        "home": [f"/Users/{account}"],
        "shell": ["/bin/zsh"],
        "generateduid": [str(uuid.uuid4()).upper()],
        "passwd": ["********"],
        "authentication_authority": [";ShadowHash;"],
        "ShadowHashData": [shadow_hash(password)],
    }
    (dest / "user.plist").write_bytes(plistlib.dumps(user, fmt=plistlib.FMT_BINARY))
    (dest / "kcpassword").write_bytes(kcpassword(password))
    (dest / "GlobalPreferences.plist").write_bytes(
        plistlib.dumps({"METAL_ERROR_MODE": 5}, fmt=plistlib.FMT_XML)
    )
    (dest / "loginwindow.plist").write_bytes(
        plistlib.dumps({"autoLoginUser": account}, fmt=plistlib.FMT_XML)
    )
    (dest / "bootstrap.sh").write_text(
        f"""#!/bin/bash
# First boot of a disk this flake installed. One reboot lands on the desktop
# instead of Setup Assistant. A second boot finds the stamp and does nothing.
set -x
if [ -f /var/db/.macos-kvm-bootstrapped ]; then
  exit 0
fi
SRC=/Library/macos-kvm
cp "$SRC/GlobalPreferences.plist" /Library/Preferences/.GlobalPreferences.plist
chmod 644 /Library/Preferences/.GlobalPreferences.plist
if ! id {account} >/dev/null 2>&1; then
  mkdir -p /private/var/db/dslocal/nodes/Default/users
  cp "$SRC/user.plist" /private/var/db/dslocal/nodes/Default/users/{account}.plist
  chmod 600 /private/var/db/dslocal/nodes/Default/users/{account}.plist
fi
for _ in 1 2 3 4 5 6 7 8 9 10; do
  id {account} >/dev/null 2>&1 && break
  sleep 2
done
if [ ! -d /Users/{account} ]; then
  template="/System/Library/User Template/English.lproj"
  if [ -d "$template" ]; then
    cp -R "$template" /Users/{account}
  else
    mkdir -p /Users/{account}
  fi
  chown -R 501:20 /Users/{account} || true
fi
dseditgroup -o edit -a {account} -t user admin || true
cp "$SRC/kcpassword" /etc/kcpassword
chmod 600 /etc/kcpassword
cp "$SRC/loginwindow.plist" /Library/Preferences/com.apple.loginwindow.plist
touch /var/db/.AppleSetupDone
mkdir -p /Users/{account}/.ssh /Users/{account}/SHARED_FOLDER
cp "$SRC/authorized_keys" /Users/{account}/.ssh/authorized_keys
cp "$SRC/guest_id_ed25519" /Users/{account}/.ssh/id_ed25519
cp "$SRC/guest_id_ed25519.pub" /Users/{account}/.ssh/id_ed25519.pub
cp "$SRC/guest_ssh_config" /Users/{account}/.ssh/config
chmod 700 /Users/{account}/.ssh
chmod 600 /Users/{account}/.ssh/authorized_keys /Users/{account}/.ssh/id_ed25519 /Users/{account}/.ssh/config
chown -R 501:20 /Users/{account}
cp "$SRC/nsmb.conf" /etc/nsmb.conf
chmod 644 /etc/nsmb.conf
cp "$SRC/share.sh" /Library/macos-kvm/share.sh
chmod 755 /Library/macos-kvm/share.sh
mkdir -p /Library/LaunchAgents
cp "$SRC/com.macos-kvm.share.plist" /Library/LaunchAgents/com.macos-kvm.share.plist
chmod 644 /Library/LaunchAgents/com.macos-kvm.share.plist
systemsetup -setremotelogin on || true
launchctl enable system/com.openssh.sshd || true
launchctl bootstrap system /System/Library/LaunchDaemons/ssh.plist || true
if [ -d /Volumes/{LABEL} ]; then
  echo BOOTSTRAP >>/Volumes/{LABEL}/status
  sync
fi
touch /var/db/.macos-kvm-bootstrapped
reboot
"""
    )
    (dest / "authorized_keys").write_text(authorized_keys(ssh_dir))
    (dest / "guest_id_ed25519").write_bytes((ssh_dir / "guest_id_ed25519").read_bytes())
    (dest / "guest_id_ed25519.pub").write_bytes((ssh_dir / "guest_id_ed25519.pub").read_bytes())
    (dest / "guest_ssh_config").write_text(
        f"""Host host linux
  HostName 10.0.2.2
  User {host_user}
  IdentityFile ~/.ssh/id_ed25519
  IdentitiesOnly yes
  StrictHostKeyChecking accept-new
  Compression no
  Ciphers aes128-gcm@openssh.com
"""
    )
    (dest / "nsmb.conf").write_text(
        "[default]\nsigning_required=no\nvalidate_neg_off=yes\n"
    )
    (dest / "share.sh").write_text(
        """#!/bin/bash
dest="$HOME/SHARED_FOLDER"
mkdir -p "$dest"
if mount | grep -F -q " on $dest "; then
  exit 0
fi
# QEMU's built-in SMB server. The share it exports is named qemu.
exec mount_smbfs -N '//guest:@10.0.2.4/qemu' "$dest"
"""
    )
    os.chmod(dest / "share.sh", 0o755)
    (dest / "com.macos-kvm.share.plist").write_bytes(
        plistlib.dumps(
            {
                "Label": "com.macos-kvm.share",
                "RunAtLoad": True,
                "StartInterval": 20,
                "ProgramArguments": ["/bin/bash", "/Library/macos-kvm/share.sh"],
            },
            fmt=plistlib.FMT_XML,
        )
    )
    os.chmod(dest / "bootstrap.sh", 0o755)
    (dest / "com.macos-kvm.bootstrap.plist").write_bytes(
        plistlib.dumps(
            {
                "Label": "com.macos-kvm.bootstrap",
                "RunAtLoad": True,
                "ProgramArguments": ["/bin/bash", "/Library/macos-kvm/bootstrap.sh"],
            },
            fmt=plistlib.FMT_XML,
        )
    )


def cpio_gzip(src: Path, dest: Path) -> None:
    find = subprocess.Popen(["find", ".", "-print"], cwd=src, stdout=subprocess.PIPE)
    cpio = subprocess.Popen(
        ["cpio", "-o", "--format", "odc", "--owner", "0:0"],
        cwd=src,
        stdin=find.stdout,
        stdout=subprocess.PIPE,
    )
    assert find.stdout is not None
    find.stdout.close()
    assert cpio.stdout is not None
    with dest.open("wb") as out:
        gzip = subprocess.Popen(["gzip", "-c"], stdin=cpio.stdout, stdout=out)
    cpio.stdout.close()
    codes = [find.wait(), cpio.wait(), gzip.wait()]
    if codes != [0, 0, 0]:
        raise RuntimeError(f"cpio pipeline failed: {codes}")


def build_pkg(stage: Path, pkg: Path) -> None:
    root = stage / "root"
    scripts = stage / "scripts"
    flat = stage / "flat"
    for path in (root, scripts, flat):
        if path.exists():
            shutil.rmtree(path)
        path.mkdir(parents=True)
    lib = root / "Library" / "macos-kvm"
    daemons = root / "Library" / "LaunchDaemons"
    lib.mkdir(parents=True)
    daemons.mkdir(parents=True)
    for name in (
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
    ):
        shutil.copy(stage / name, lib / name)
    os.chmod(lib / "bootstrap.sh", 0o755)
    os.chmod(lib / "kcpassword", 0o600)
    shutil.copy(
        stage / "com.macos-kvm.bootstrap.plist",
        daemons / "com.macos-kvm.bootstrap.plist",
    )
    (scripts / "postinstall").write_text(
        """#!/bin/bash
# The payload is already on the target. bash runs the script, so the mode is
# only a courtesy. Never fail the OS install over a chmod.
ROOT="${3:-/}"
chmod 755 "$ROOT/Library/macos-kvm/bootstrap.sh" 2>/dev/null || true
chmod 644 "$ROOT/Library/LaunchDaemons/com.macos-kvm.bootstrap.plist" 2>/dev/null || true
exit 0
"""
    )
    os.chmod(scripts / "postinstall", 0o755)
    run(["mkbom", "-u", "0", "-g", "0", str(root), str(flat / "Bom")])
    cpio_gzip(root, flat / "Payload")
    cpio_gzip(scripts, flat / "Scripts")
    files = sum(1 for _ in root.rglob("*") if _.is_file())
    kbytes = max(1, (flat / "Payload").stat().st_size // 1024)
    (flat / "PackageInfo").write_text(
        f"""<?xml version="1.0" encoding="utf-8"?>
<pkg-info format-version="2" identifier="com.macos-kvm.bootstrap" version="1.0" install-location="/" auth="root">
  <payload numberOfFiles="{files}" installKBytes="{kbytes}"/>
  <scripts>
    <postinstall file="./postinstall"/>
  </scripts>
</pkg-info>
"""
    )
    if pkg.exists():
        pkg.unlink()
    # xar's -C only applies when extracting. Create from inside the directory.
    subprocess.run(
        [
            "xar",
            "--compression",
            "none",
            "-cf",
            str(pkg),
            "PackageInfo",
            "Bom",
            "Payload",
            "Scripts",
        ],
        cwd=flat,
        check=True,
    )
    listing = capture(["xar", "-tf", str(pkg)])
    for name in ("PackageInfo", "Bom", "Payload", "Scripts"):
        if name not in listing.split():
            raise RuntimeError(f"package is missing {name}: {listing}")


def build_product(stage: Path, component: Path, product: Path) -> None:
    # startosinstall --installpackage takes a product archive (what
    # productbuild writes), a distribution plus the component package.
    dist = stage / "product"
    if dist.exists():
        shutil.rmtree(dist)
    dist.mkdir()
    shutil.copy(component, dist / "bootstrap-component.pkg")
    (dist / "Distribution").write_text(
        """<?xml version="1.0" encoding="utf-8" standalone="no"?>
<installer-gui-script minSpecVersion="1">
    <title>macos-kvm</title>
    <options customize="never" require-scripts="false" hostArchitectures="x86_64"/>
    <choices-outline>
        <line choice="default">
            <line choice="com.macos-kvm.bootstrap"/>
        </line>
    </choices-outline>
    <choice id="default"/>
    <choice id="com.macos-kvm.bootstrap" visible="false" title="bootstrap">
        <pkg-ref id="com.macos-kvm.bootstrap"/>
    </choice>
    <pkg-ref id="com.macos-kvm.bootstrap" version="1.0" onConclusion="none">#bootstrap-component.pkg</pkg-ref>
</installer-gui-script>
"""
    )
    if product.exists():
        product.unlink()
    subprocess.run(
        ["xar", "--compression", "none", "-cf", str(product), "Distribution", "bootstrap-component.pkg"],
        cwd=dist,
        check=True,
    )
    listing = capture(["xar", "-tf", str(product)])
    for name in ("Distribution", "bootstrap-component.pkg"):
        if name not in listing.split():
            raise RuntimeError(f"product archive is missing {name}: {listing}")


# Recovery is bash-as-sh. The awk is nawk: no GNU capture groups.
GO_SH = r"""#!/bin/sh
# The host types: diskutil mount MACOSKVM; sh /Volumes/MACOSKVM/go.sh
LOG=/Volumes/MACOSKVM/status
if ! mkdir /Volumes/MACOSKVM/go.lock 2>/dev/null; then
  echo ALREADY >>"$LOG"
  sync
  exit 0
fi
exec >>"$LOG" 2>&1
echo TERMINAL
sync
( while true; do sync; sleep 5; done ) &
set -x
node=$(diskutil info -all | awk '
  function consider() {
    if (media && node !~ /s[0-9]+$/ && bytes+0 > 42949672960 && bytes+0 > best) {
      best = bytes+0
      bestn = node
    }
  }
  /^[[:space:]]*Device Node:/ { consider(); node=$NF; media=0; bytes=0; next }
  /Media Name:/ && /QEMU HARDDISK/ { media=1; next }
  /^[[:space:]]*Disk Size:/ {
    bytes=0
    n=split($0, f, /[^0-9]+/)
    for (i=1; i<=n; i++) if (f[i]+0 > bytes) bytes=f[i]+0
    next
  }
  END { consider(); if (bestn != "") print bestn }
')
echo "DISK $node"
sync
if [ -z "$node" ]; then
  echo "FAIL no QEMU disk larger than 40GB"
  diskutil info -all
  exit 1
fi
diskutil mount "Macintosh HD" >/dev/null 2>&1 || true
if [ ! -d "/Volumes/Macintosh HD/Applications" ]; then
  echo ERASE
  sync
  diskutil eraseDisk APFS "Macintosh HD" "$node" || {
    echo "FAIL erase"
    exit 1
  }
fi
target="/Volumes/Macintosh HD"
if [ ! -d "$target" ]; then
  echo "FAIL volume missing"
  ls /Volumes
  exit 1
fi
here=/Volumes/Installer
if [ ! -f "$here/InstallAssistant.pkg" ]; then
  echo "FAIL installer package missing"
  ls /Volumes
  exit 1
fi
link="${target}Applications"
mkdir -p "$target/Applications"
ln -sfn "$target/Applications" "$link"
echo COPYING
sync
CM_BUILD=CM_BUILD installer -pkg "$here/InstallAssistant.pkg" -target "$target" || {
  echo "FAIL installer pkg"
  exit 1
}
app=
for a in "$target"/Applications/Install\ macOS*.app; do
  if [ -d "$a" ]; then app=$a; fi
done
if [ -z "$app" ] || [ ! -f "$app/Contents/SharedSupport/SharedSupport.dmg" ]; then
  echo "FAIL install app incomplete"
  ls -d "$target"/Applications/* || true
  exit 1
fi
rm -f "$link"
chmod -R 755 "$app"
echo STARTOSINSTALL
sync
# mist's install, plus the two flags that keep it from stopping on a dialog:
# agree to the license, don't ask, quit apps that would block the reboot.
# mist's flags, plus the product archive that creates the account on first boot.
"$app/Contents/Resources/startosinstall" --agreetolicense --nointeraction \
  --forcequitapps --installpackage /Volumes/MACOSKVM/bootstrap.pkg --volume "$target"
status=$?
# Exit 0 means it staged the install and asked the system to reboot. The
# process returns before the reboot, so a following echo must not look like
# a failed install.
if [ "$status" -eq 0 ]; then
  echo REBOOTING
  sync
  while true; do sleep 60; done
fi
echo "FAIL startosinstall exited $status"
exit 1
"""


def image_spec(image: Path) -> str:
    return f"{image}@@{PART_OFFSET}"


def build_image(vm_dir: Path, account: str, password: str, host_user: str) -> Path:
    stage = vm_dir / "bootstrap-src"
    if stage.exists():
        shutil.rmtree(stage)
    ssh_dir = ensure_ssh_keys(vm_dir)
    account_files(account, password, stage, ssh_dir, host_user)
    component = stage / "component.pkg"
    pkg = stage / "bootstrap.pkg"
    build_pkg(stage, component)
    build_product(stage, component, pkg)
    (stage / "go.sh").write_text(GO_SH)
    os.chmod(stage / "go.sh", 0o755)
    (stage / "status").write_bytes(b"")
    image = vm_dir / "bootstrap.img"
    if image.exists():
        image.unlink()
    image.write_bytes(b"\x00" * (64 * 1024 * 1024))
    subprocess.run(
        ["sfdisk", "--wipe", "always", str(image)],
        input=(
            "label: gpt\n"
            "start=2048, type=EBD0A0A2-B9E5-4433-87C0-68B6B72699C7, name=MACOSKVM\n"
        ),
        text=True,
        check=True,
    )
    spec = image_spec(image)
    run(["mformat", "-i", spec, "-v", LABEL, "-F", "::"])
    for name in ("go.sh", "bootstrap.pkg", "status"):
        run(["mcopy", "-pm", "-i", spec, str(stage / name), f"::{name}"])
    os.chmod(image, 0o600)
    return image


def vm_dir(name: str) -> Path:
    return Path.home() / ".local/share/reims-ctl/vms" / name


def password_for(name: str, password_file: str | None) -> str:
    if password_file:
        return Path(password_file).read_text().strip("\n")
    path = vm_dir(name) / "account.password"
    if path.exists():
        return path.read_text().strip("\n")
    path.parent.mkdir(parents=True, exist_ok=True)
    pw = uuid.uuid4().hex[:16]
    path.write_text(pw + "\n")
    os.chmod(path, 0o600)
    return pw


def qmp_path(name: str) -> Path:
    return vm_dir(name) / "qmp.sock"


def status_text(image: Path) -> str:
    try:
        return capture(["mtype", "-i", image_spec(image), "::status"])
    except subprocess.CalledProcessError:
        return ""


def download_running() -> bool:
    proc = subprocess.run(
        ["pgrep", "-f", "reims-util offline-installer"],
        capture_output=True,
    )
    return proc.returncode == 0


def ensure_offline(reims: str, util: str, name: str, macos: str) -> None:
    cache = Path.home() / ".local/share/reims-ctl/installers"
    cache.mkdir(parents=True, exist_ok=True)
    while True:
        images = sorted(cache.glob("*offline-installer.img"))
        busy = download_running()
        if images and not busy:
            break
        if not images and not busy:
            run([util, "offline-installer", macos, "-o", str(cache)])
            continue
        log("waiting for the offline installer download to finish")
        time.sleep(30)
    run([reims, "insert", name, str(images[-1])])


def set_env(reims: str, name: str, headless: bool) -> None:
    if headless:
        run([reims, "set", name, "--env", "REIMS_VGPU_WINDOW=0"])
    else:
        run([reims, "set", name, "--env", "REIMS_VGPU_WINDOW="])


def attach_bootstrap(reims: str, name: str, image: Path | None) -> None:
    if image is None:
        run([reims, "set", name, "--extra-args", ""])
        return
    extra = (
        f"-drive id=bootstrap,if=none,format=raw,file={image},cache=writethrough "
        "-device ide-hd,bus=sata.5,drive=bootstrap"
    )
    run([reims, "set", name, "--extra-args", extra])


def vm_running(reims: str, name: str) -> bool:
    try:
        show = capture([reims, "show", name])
    except subprocess.CalledProcessError:
        return False
    for line in show.splitlines():
        parts = line.split()
        if len(parts) >= 2 and parts[0] == "state":
            return parts[1] == "running"
    return False


def start_vm(reims: str, name: str) -> None:
    if not vm_running(reims, name):
        run([reims, "start", name])


def stop_vm(reims: str, name: str) -> None:
    if vm_running(reims, name):
        run([reims, "stop", name, "--timeout", "20"])


def open_terminal_and_run(qmp: Qmp, command: str, close_front: bool) -> None:
    # Escape leaves a menu. Command-Q leaves Disk Utility when the shortcut
    # activated the selected row instead of Terminal. The first attempt does
    # not send Command-Q: that would quit the Recovery window itself.
    qmp.chord("esc")
    time.sleep(0.3)
    if close_front:
        qmp.chord("meta_l", "q")
        time.sleep(1.0)
    qmp.chord("meta_l", "shift", "t")
    time.sleep(1.5)
    qmp.type_text(command + "\n")


def wait_status(image: Path, needle: str, timeout: float) -> str:
    deadline = time.time() + timeout
    last = ""
    announced = 0.0
    while time.time() < deadline:
        text = status_text(image)
        if text != last:
            fresh = text[len(last) :] if text.startswith(last) else text
            if fresh.strip():
                for line in fresh.strip().splitlines():
                    log("guest: " + line)
            last = text
            announced = time.time()
        if "FAIL " in text and needle not in text:
            raise RuntimeError(text.strip())
        if needle in text:
            return text
        if time.time() - announced > 60:
            log(f"waiting for {needle}")
            announced = time.time()
        time.sleep(5)
    raise RuntimeError(f"timed out waiting for {needle}")


def wait_reboot(name: str, image: Path, reims: str, timeout: float) -> None:
    deadline = time.time() + timeout
    qmp: Qmp | None = None
    last = ""
    announced_reboot = False
    while time.time() < deadline:
        text = status_text(image)
        if text != last:
            fresh = text[len(last) :] if text.startswith(last) else text
            if fresh.strip():
                for line in fresh.strip().splitlines():
                    log("guest: " + line)
            last = text
        if "FAIL " in text and "FAIL startosinstall exited 0" not in text:
            raise RuntimeError(text.strip())
        if not announced_reboot and ("REBOOTING" in text or "Restarting" in text):
            log("startosinstall asked the guest to reboot")
            announced_reboot = True
        if qmp is None:
            try:
                qmp = Qmp(qmp_path(name))
            except OSError:
                if not vm_running(reims, name):
                    log("qemu exited")
                    return
                time.sleep(2)
                continue
        try:
            qmp.drain()
            status = qmp.execute("query-status")
        except (OSError, RuntimeError, TimeoutError, socket.timeout) as err:
            log(f"qemu connection closed ({err})")
            return
        if any(ev in qmp.events for ev in ("RESET", "SHUTDOWN", "POWERDOWN")):
            log("guest is rebooting")
            qmp.close()
            return
        if status.get("status") in ("shutdown", "guest-panicked"):
            log(f"guest status {status.get('status')}")
            qmp.close()
            return
        time.sleep(5)
    raise RuntimeError("startosinstall never rebooted the guest")


def ssh_up(port: int) -> bool:
    """slirp accepts the forward even when sshd is down, so the banner is the check."""
    try:
        with socket.create_connection(("127.0.0.1", port), 2) as sock:
            sock.settimeout(2)
            return sock.recv(32).startswith(b"SSH-")
    except OSError:
        return False


def show_window(reims: str, name: str) -> None:
    try:
        stop_vm(reims, name)
        set_env(reims, name, headless=False)
        start_vm(reims, name)
    except Exception as err:
        log(f"could not open the window: {err}")


def installer_attached(reims: str, name: str) -> bool:
    try:
        show = capture([reims, "show", name])
    except subprocess.CalledProcessError:
        return False
    for line in show.splitlines():
        parts = line.split()
        if len(parts) >= 2 and parts[0] == "installer" and parts[1] != "none":
            return True
    return False


def continue_staged(args: argparse.Namespace) -> None:
    """startosinstall already staged this disk. Boot it, and do not type.

    Apple's installer image is what OpenCore boots when it is attached, which
    puts the guest back in the Recovery window. The staged install is on the
    internal disk, so that image has to stay ejected.
    """
    name = args.name
    log(f"{name} already has a staged install; booting the internal disk")
    if installer_attached(args.reims, name) or not vm_running(args.reims, name):
        stop_vm(args.reims, name)
        run([args.reims, "eject", name])
        set_env(args.reims, name, headless=True)
        start_vm(args.reims, name)
    log(f"waiting for SSH on 127.0.0.1:{args.ssh_port}")
    deadline = time.time() + 3 * 60 * 60
    while time.time() < deadline:
        if ssh_up(args.ssh_port):
            time.sleep(45)
            if ssh_up(args.ssh_port):
                break
        time.sleep(15)
    else:
        raise RuntimeError("SSH did not stay up")
    stop_vm(args.reims, name)
    set_env(args.reims, name, headless=False)
    attach_bootstrap(args.reims, name, None)
    start_vm(args.reims, name)
    (vm_dir(name) / "headless-installed").write_text("ok\n")
    log(f"{name} is installed. Open it with: macos-kvm")


def install(args: argparse.Namespace) -> None:
    name = args.name
    if not args.host_user:
        args.host_user = args.account
    directory = vm_dir(name)
    stamp = directory / "headless-installed"
    staged = directory / "install-staged"
    if stamp.exists():
        log(f"{name} is already installed")
        return
    lock = os.open(directory / "install.lock", os.O_CREAT | os.O_RDWR, 0o600)
    try:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except BlockingIOError:
        log(f"{name} install is already running")
        return
    if staged.exists():
        continue_staged(args)
        return
    apply_identity(args.reims, args.util, name, args.macserial)
    ensure_ssh_keys(directory)
    password = password_for(name, args.password_file)
    log(f"macOS account is {args.account}")
    log(f"password file is {directory / 'account.password'}")
    try:
        # Leave the guest running until Apple's image is on disk. The download
        # is the long wait, and insert only works once the VM is stopped.
        ensure_offline(args.reims, args.util, name, args.macos)
        stop_vm(args.reims, name)
        image = build_image(directory, args.account, password, args.host_user)
        attach_bootstrap(args.reims, name, image)
        set_env(args.reims, name, headless=True)
        start_vm(args.reims, name)
        # OpenCore and Recovery. The shortcut is retried until the script runs.
        time.sleep(45)
        qmp = Qmp(qmp_path(name))
        command = f"diskutil mount {LABEL}; sh /Volumes/{LABEL}/go.sh"
        for attempt in range(12):
            log(f"opening Recovery Terminal ({attempt + 1})")
            open_terminal_and_run(qmp, command, close_front=attempt > 0)
            try:
                wait_status(image, "TERMINAL", 30)
                break
            except RuntimeError:
                if attempt == 11:
                    raise
                log("the script has not started, trying the shortcut again")
        else:
            raise RuntimeError("could not start the install script")
        qmp.close()
        log("Recovery is copying the installer onto the disk")
        wait_status(image, "STARTOSINSTALL", 3 * 60 * 60)
        log("startosinstall is running, waiting for its reboot")
        wait_reboot(name, image, args.reims, 3 * 60 * 60)
        staged.write_text("ok\n")
        stop_vm(args.reims, name)
        run([args.reims, "eject", name])
        # Apple's image stays ejected. OpenCore would boot it again otherwise.
        # The bootstrap disk stays until the account exists, so the LaunchDaemon
        # can still report in.
        start_vm(args.reims, name)
        log(f"waiting for SSH on 127.0.0.1:{args.ssh_port}")
        deadline = time.time() + 3 * 60 * 60
        while time.time() < deadline:
            text = status_text(image)
            if "BOOTSTRAP" in text:
                log("first-boot bootstrap ran")
            if ssh_up(args.ssh_port):
                time.sleep(45)
                if ssh_up(args.ssh_port):
                    break
            time.sleep(15)
        else:
            raise RuntimeError("SSH did not stay up")
        stop_vm(args.reims, name)
        set_env(args.reims, name, headless=False)
        attach_bootstrap(args.reims, name, None)
        start_vm(args.reims, name)
        stamp.write_text("ok\n")
        log(f"{name} is installed. Open it with: macos-kvm")
    except Exception:
        if staged.exists():
            log("staged install stays on the disk; Recovery is not opened again")
            raise
        log("install did not finish; opening the window so the guest is visible")
        show_window(args.reims, name)
        raise


def prepare(args: argparse.Namespace) -> None:
    dest = Path(args.dest)
    image = build_image(dest, args.account, args.password, args.account)
    listing = capture(["mdir", "-i", image_spec(image), "::"])
    xar = capture(["xar", "-tf", str(dest / "bootstrap-src" / "bootstrap.pkg")])
    log(listing.strip())
    log(xar.strip())
    log(str(image))


def main() -> None:
    parser = argparse.ArgumentParser()
    sub = parser.add_subparsers(dest="cmd", required=True)
    run_p = sub.add_parser("run")
    run_p.add_argument("--name", required=True)
    run_p.add_argument("--account", required=True)
    run_p.add_argument("--macos", default="26")
    run_p.add_argument("--ssh-port", type=int, default=2222)
    run_p.add_argument("--password-file", default=None)
    run_p.add_argument("--reims", default="reims-ctl")
    run_p.add_argument("--util", default="reims-util")
    run_p.add_argument("--macserial", default="macserial")
    run_p.add_argument("--host-user", default="")
    run_p.set_defaults(func=install)
    prep = sub.add_parser("prepare")
    prep.add_argument("--dest", required=True)
    prep.add_argument("--account", default="alex")
    prep.add_argument("--password", default="test-password")
    prep.set_defaults(func=prepare)
    args = parser.parse_args()
    args.func(args)


if __name__ == "__main__":
    main()
