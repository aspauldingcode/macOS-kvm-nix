#!/bin/sh
# Recovery writes the account onto the installed Data volume. The package
# startosinstall is asked to install does not always land, and the Recovery
# ramdisk ("macOS Base System") is not the target.
LOG=/Volumes/MACOSKVM/status
echo FINISH >>"$LOG"
sync
# Recovery leaves the installed Data volume unmounted.
diskutil mount "Macintosh HD - Data" >>"$LOG" 2>&1 || true
diskutil list | sed -n 's/.*\(disk[0-9][0-9]*s[0-9][0-9]*\).*/\1/p' | while read -r dev; do
  diskutil mount "$dev" >>"$LOG" 2>&1 || true
done
ls /Volumes >>"$LOG" 2>&1
sync
data=""
for v in /Volumes/*; do
  base=$(basename "$v")
  case "$base" in
    "macOS Base System"|MACOSKVM|RECOVERY|Installer) continue ;;
  esac
  case "$base" in
    *" - Data")
      # Users is created at first boot. The volume is the target before that.
      data=$v
      ;;
  esac
done
echo "DATA $data" >>"$LOG"
if [ -z "$data" ]; then
  echo "FAIL no data volume" >>"$LOG"
  sync
  exit 1
fi
if [ -d "$data/private/var/db" ]; then
  db="$data/private/var/db"
elif [ -d "$data/var/db" ]; then
  db="$data/var/db"
else
  db="$data/private/var/db"
  mkdir -p "$db/dslocal/nodes/Default/users"
fi
if [ -d "$data/private/etc" ]; then
  etc="$data/private/etc"
elif [ -d "$data/etc" ]; then
  etc="$data/etc"
else
  etc="$data/private/etc"
  mkdir -p "$etc"
fi
SRC=/Volumes/MACOSKVM
mkdir -p "$data/Library/macos-kvm" \
  "$data/Library/LaunchDaemons" \
  "$data/Library/LaunchAgents" \
  "$db/dslocal/nodes/Default/users" \
  "$data/Users/@ACCOUNT@/.ssh" \
  "$data/Users/@ACCOUNT@/SHARED_FOLDER" \
  "$data/Library/Preferences" \
  "$etc"
sys=""
for v in /Volumes/*; do
  base=$(basename "$v")
  case "$base" in
    "macOS Base System"|MACOSKVM|RECOVERY|Installer) continue ;;
  esac
  if [ -d "$v/System/Library/User Template/English.lproj" ]; then
    sys=$v
  fi
done
if [ ! -d "$data/Users/@ACCOUNT@" ]; then
  if [ -n "$sys" ]; then
    cp -R "$sys/System/Library/User Template/English.lproj" "$data/Users/@ACCOUNT@"
  else
    mkdir -p "$data/Users/@ACCOUNT@"
  fi
fi
cp "$SRC/user.plist" "$db/dslocal/nodes/Default/users/@ACCOUNT@.plist"
chmod 600 "$db/dslocal/nodes/Default/users/@ACCOUNT@.plist"
if [ -f "$SRC/kcpassword" ]; then
  cp "$SRC/kcpassword" "$etc/kcpassword"
  chmod 600 "$etc/kcpassword"
fi
if [ -f "$SRC/loginwindow.plist" ]; then
  cp "$SRC/loginwindow.plist" "$data/Library/Preferences/com.apple.loginwindow.plist"
fi
cp "$SRC/GlobalPreferences.plist" "$data/Library/Preferences/.GlobalPreferences.plist"
touch "$db/.AppleSetupDone"
rm -f "$db/.AppleSetupTermsOfService"
cp "$SRC/authorized_keys" "$data/Users/@ACCOUNT@/.ssh/authorized_keys"
cp "$SRC/guest_id_ed25519" "$data/Users/@ACCOUNT@/.ssh/id_ed25519"
cp "$SRC/guest_id_ed25519.pub" "$data/Users/@ACCOUNT@/.ssh/id_ed25519.pub"
cp "$SRC/guest_ssh_config" "$data/Users/@ACCOUNT@/.ssh/config"
chmod 700 "$data/Users/@ACCOUNT@/.ssh"
chmod 600 "$data/Users/@ACCOUNT@/.ssh/authorized_keys" \
  "$data/Users/@ACCOUNT@/.ssh/id_ed25519" "$data/Users/@ACCOUNT@/.ssh/config"
chown -R 501:20 "$data/Users/@ACCOUNT@" || true
cp "$SRC/nsmb.conf" "$etc/nsmb.conf"
chmod 644 "$etc/nsmb.conf"
cp "$SRC/bootstrap.sh" "$data/Library/macos-kvm/bootstrap.sh"
cp "$SRC/share.sh" "$data/Library/macos-kvm/share.sh"
cp "$SRC/com.macos-kvm.share.plist" "$data/Library/LaunchAgents/com.macos-kvm.share.plist"
cp "$SRC/com.macos-kvm.bootstrap.plist" "$data/Library/LaunchDaemons/com.macos-kvm.bootstrap.plist"
chmod 755 "$data/Library/macos-kvm/bootstrap.sh" "$data/Library/macos-kvm/share.sh"
chmod 644 "$data/Library/LaunchDaemons/com.macos-kvm.bootstrap.plist" \
  "$data/Library/LaunchAgents/com.macos-kvm.share.plist"
for f in user.plist kcpassword GlobalPreferences.plist loginwindow.plist authorized_keys \
  guest_id_ed25519 guest_id_ed25519.pub guest_ssh_config nsmb.conf share.sh \
  com.macos-kvm.share.plist bootstrap.sh; do
  if [ -f "$SRC/$f" ]; then
    cp "$SRC/$f" "$data/Library/macos-kvm/$f"
  fi
done
echo PLANTED >>"$LOG"
sync
