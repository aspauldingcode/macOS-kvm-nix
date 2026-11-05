# macOS-kvm-nix

x86-64 macOS guests on Linux KVM, with [Reims vGPU](https://reims-vgpu.com/): Metal commands from the guest's own `AppleParavirtGPU` driver run on the host's Vulkan GPU through metal2vulkan. Each display is a Reims window (Wayland or X11), not a virtio-gpu scanout. HiDPI when asked. Dragging the window, once it has settled, changes the guest's display mode.

macOS 26 Tahoe is the last Intel release. The guest is KVM, not emulation. The CPU model is reims-ctl's Skylake-Client (`+invtsc`, `vmware-cpuid-freq`), which Tahoe boots. vCPUs and RAM default to half the host (2–8 cores, 4–16 GiB).

Apple's license still applies to the guest. The installer is downloaded from Apple by reims-util. OpenCore presents the VM as an iMac20,1 with its own serial, MLB, UUID, and ROM.

## Host

- x86-64 Linux, KVM usable by the desktop user
- A Vulkan GPU. On an NVIDIA hybrid laptop set `primeOffload = true` so the guest renders on the dGPU and the compositor stays on the iGPU
- A Wayland or X11 session for the window
- `vm.max_map_count` at 2097152 and `/dev/udmabuf` (the module sets both)

## Use

```nix
{
  inputs.macos-kvm.url = "github:aspauldingcode/macOS-kvm-nix";

  outputs = { macos-kvm, ... }: {
    nixosConfigurations.myhost = nixpkgs.lib.nixosSystem {
      modules = [
        macos-kvm.nixosModules.default
        {
          macos-kvm.enable = true;
          macos-kvm.user = "alex";
          macos-kvm.accountName = "alex";       # guest short name
          macos-kvm.accountPassword = "change-me"; # world-readable in the Nix store
          macos-kvm.loginWindow = false;        # boot straight into the desktop
          macos-kvm.primeOffload = true; # hybrid NVIDIA
          macos-kvm.hidpi = true;        # 3840x2160 framebuffer, looks like 1920x1080
          macos-kvm.borderless = false;  # resizable window
        }
      ];
    };
  };
}
```

`nh os switch` installs `reims-ctl`, `reims-util`, and the QEMU linked against reims-vgpu. On the next graphical session a user service creates the VM if it is missing and installs macOS. The window stays hidden while that runs, and a desktop notification says that macOS is being set up. The disk, OpenCore image, and Apple installer stay in the user's XDG data directory (`~/.local/share/reims-ctl`).

The install is silent. The window stays hidden and the speaker stays off until the desktop is up. It downloads Apple's full installer (about 17 GiB) and types Command-Shift-T into Recovery once, over the QMP socket. The script on the small disk erases the guest disk, expands `InstallAssistant.pkg` the way mist does (`CM_BUILD=CM_BUILD installer -pkg`), and runs `startosinstall --agreetolicense --nointeraction --forcequitapps --volume` with one package. That package, and a Recovery script that writes the same files onto the Data volume when the package does not land, creates the account, turns on SSH, skips Setup Assistant, sets Metal's error mode, and mounts the shared folder. With `loginWindow = false` (the default) it also signs that account in, so the first window is the desktop. Nothing is typed after the script starts. When startosinstall reboots, Apple's image is ejected so OpenCore boots the internal disk instead of returning to the Recovery window. When SSH answers, the window opens and the speaker comes back.

`accountName` is the guest's short name (`macos-kvm.user` when it is left null). `accountPassword` sets the password. Changing it later, then starting the guest, changes the password of the account that was installed; it does not reinstall macOS. `loginWindow = true` shows the login window instead. Changing that later is applied the same way. `accountPassword` is stored in the NixOS system, which every user on the machine can read. `accountPasswordFile` is a runtime path for a password that must stay out of the store. Leaving both unset generates one at `~/.local/share/reims-ctl/vms/tahoe/account.password` (mode 0600).

A StatusNotifier item (a menubar applet, in Waybar's tray) can open, start, stop, restart, show, or quit the guest. virtio-net carries the shared folder and SSH. The display stays the Reims GPU: a window resize offers the new size through `rvg_display_set_modes`, and the guest switches to it. virtio-gpu is not the display.

OpenCore's serial, board serial, UUID, and ROM come from `macserial`, the same program [osx-serial-generator](https://github.com/sickcodes/osx-serial-generator) runs, for an iMac20,1. They are written into reims-util's OpenCore image, which keeps `ReimsVgpu.kext`. The generator's own boot disk is not used.

`~/SHARED_FOLDER` on the host is the guest's `~/SHARED_FOLDER`. QEMU exports it over the guest's virtual network (`smb://10.0.2.4/qemu`), and a login agent mounts it. `ssh tahoe` (and `macos-kvm ssh`) reaches the guest on `127.0.0.1:2222` with a key, and the guest reaches the host as `ssh host` at `10.0.2.2`.

```sh
macos-kvm            # install if needed, then open the window
macos-kvm ssh        # shell on the guest
macos-kvm account    # print the generated password
macos-kvm show tahoe
ssh tahoe            # same guest, from the host ssh config
```

`macos-kvm account` prints the password the guest is using. A password set with `accountPassword` is also in the Nix store.
