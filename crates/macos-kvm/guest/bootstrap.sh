#!/bin/bash
# First boot of a disk this flake installed. One reboot lands on the desktop
# instead of Setup Assistant. A second boot finds the stamp and does nothing.
set -x
if [ -f /var/db/.macos-kvm-bootstrapped ]; then
  exit 0
fi
SRC=/Library/macos-kvm
cp "$SRC/GlobalPreferences.plist" /Library/Preferences/.GlobalPreferences.plist
chmod 644 /Library/Preferences/.GlobalPreferences.plist
if ! id @ACCOUNT@ >/dev/null 2>&1; then
  mkdir -p /private/var/db/dslocal/nodes/Default/users
  cp "$SRC/user.plist" /private/var/db/dslocal/nodes/Default/users/@ACCOUNT@.plist
  chmod 600 /private/var/db/dslocal/nodes/Default/users/@ACCOUNT@.plist
fi
for _ in 1 2 3 4 5 6 7 8 9 10; do
  id @ACCOUNT@ >/dev/null 2>&1 && break
  sleep 2
done
if [ ! -d /Users/@ACCOUNT@ ]; then
  template="/System/Library/User Template/English.lproj"
  if [ -d "$template" ]; then
    cp -R "$template" /Users/@ACCOUNT@
  else
    mkdir -p /Users/@ACCOUNT@
  fi
  chown -R 501:20 /Users/@ACCOUNT@ || true
fi
dseditgroup -o edit -a @ACCOUNT@ -t user admin || true
# Absent when the host asked for the login window.
if [ -f "$SRC/kcpassword" ]; then
  cp "$SRC/kcpassword" /etc/kcpassword
  chmod 600 /etc/kcpassword
fi
if [ -f "$SRC/loginwindow.plist" ]; then
  cp "$SRC/loginwindow.plist" /Library/Preferences/com.apple.loginwindow.plist
fi
touch /var/db/.AppleSetupDone
# This cookie is what makes the first login stop on the license window.
rm -f /var/db/.AppleSetupTermsOfService
mkdir -p /Users/@ACCOUNT@/.ssh /Users/@ACCOUNT@/SHARED_FOLDER
cp "$SRC/authorized_keys" /Users/@ACCOUNT@/.ssh/authorized_keys
cp "$SRC/guest_id_ed25519" /Users/@ACCOUNT@/.ssh/id_ed25519
cp "$SRC/guest_id_ed25519.pub" /Users/@ACCOUNT@/.ssh/id_ed25519.pub
cp "$SRC/guest_ssh_config" /Users/@ACCOUNT@/.ssh/config
chmod 700 /Users/@ACCOUNT@/.ssh
chmod 600 /Users/@ACCOUNT@/.ssh/authorized_keys /Users/@ACCOUNT@/.ssh/id_ed25519 /Users/@ACCOUNT@/.ssh/config
chown -R 501:20 /Users/@ACCOUNT@
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
if [ -d /Volumes/MACOSKVM ]; then
  echo BOOTSTRAP >>/Volumes/MACOSKVM/status
  sync
fi
touch /var/db/.macos-kvm-bootstrapped
reboot
