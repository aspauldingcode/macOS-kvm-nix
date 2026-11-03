# macOS-kvm-nix

x86-64 macOS guests on Linux KVM, with [Reims vGPU](https://reims-vgpu.com/): Metal commands from the guest's own `AppleParavirtGPU` driver run on the host's Vulkan GPU through metal2vulkan. Each display is a window (Wayland or X11), HiDPI when asked, and resizable unless the window is borderless.

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
          macos-kvm.primeOffload = true; # hybrid NVIDIA
          macos-kvm.hidpi = true;        # 3840x2160 framebuffer, looks like 1920x1080
          macos-kvm.borderless = false;  # resizable window
        }
      ];
    };
  };
}
```

`nh os switch` installs `reims-ctl`, `reims-util`, and the QEMU linked against reims-vgpu. On the next graphical session a user service creates the VM if it is missing and starts it. The disk, OpenCore image, and Apple installer stay in the user's XDG data directory (`~/.local/share/reims-ctl`).

The first window is macOS Recovery. Erase the QEMU disk as APFS, reinstall macOS, then from Recovery set Metal's error mode or the desktop comes up without a menu bar:

```sh
cd /Volumes/*Data/Library/Preferences && defaults write "$PWD/.GlobalPreferences" METAL_ERROR_MODE -int 5 && chmod 644 .GlobalPreferences.plist && reboot
```

Then `macos-kvm eject tahoe`. Later logins start the installed guest.

`macos-kvm` is `reims-ctl` with this host's Vulkan environment. `macos-kvm show tahoe` prints the Mac serial OpenCore generated.
