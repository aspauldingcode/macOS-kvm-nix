#!/bin/sh
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
