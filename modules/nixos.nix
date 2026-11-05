# macOS 26 on Linux KVM, drawn by Reims vGPU.
#
# The guest is x86-64 macOS. QEMU's reims-vgpu-pci device is the paravirtual
# GPU macOS already drives (AppleParavirtGPU). The host runs those Metal
# commands on Vulkan through metal2vulkan, and each display is a window:
# Wayland or X11, HiDPI, and resizable when the window is not borderless.
#
# `nh os switch` installs the toolchain. At the next graphical session the
# user service creates the VM if it is missing and installs macOS with no
# clicks. OpenCore gets a macserial identity (the generator
# osx-serial-generator runs). A package applied by startosinstall creates
# the account, turns on auto-login and SSH, and mounts ~/SHARED_FOLDER.
# The disk lives under the user's XDG data directory, not in the Nix store.
{ self }:
{
  config,
  lib,
  pkgs,
  options,
  ...
}:
let
  cfg = config.macos-kvm;
  reims = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
  account = if cfg.accountName == null then cfg.user else cfg.accountName;
  macserial = pkgs.callPackage ../pkgs/macserial.nix { };

  nvidiaIcd = "/run/opengl-driver/share/vulkan/icd.d/nvidia_icd.json";

  # reims-ctl's wrapper pins REIMS_CTL_QEMU. The unwrapped binary honours the
  # variable, and it looks for qemu-img in the same directory as that binary.
  # The share directory is added to the user netdev at start.
  qemuShare = pkgs.runCommand "macos-kvm-qemu" { } ''
    mkdir -p $out/bin
    cat > $out/bin/qemu-system-x86_64 <<'EOF'
    #!${pkgs.bash}/bin/bash
    set -euo pipefail
    real=@qemu@
    share="''${MACOS_KVM_SHARE:-}"
    if [[ -z "$share" ]]; then
      exec "$real" "$@"
    fi
    args=()
    while [[ $# -gt 0 ]]; do
      if [[ "$1" == -netdev && "''${2:-}" == user,* && "''${2:-}" != *",smb="* ]]; then
        args+=(-netdev "$2,smb=$share")
        shift 2
        continue
      fi
      args+=("$1")
      shift
    done
    exec "$real" "''${args[@]}"
EOF
    sed -i 's/^    //' $out/bin/qemu-system-x86_64
    substituteInPlace $out/bin/qemu-system-x86_64 --replace-fail @qemu@ ${reims}/bin/qemu-system-x86_64
    chmod +x $out/bin/qemu-system-x86_64
    ln -s ${reims}/bin/qemu-img $out/bin/qemu-img
  '';

  # mkbom writes the file name into a one-byte char array that is allocated
  # to the real length. glibc's fortify check aborts that copy.
  bomutils = pkgs.bomutils.overrideAttrs (old: {
    hardeningDisable = (old.hardeningDisable or [ ]) ++ [ "fortify" ];
  });

  tool = self.packages.${pkgs.stdenv.hostPlatform.system}.macos-kvm;

  primeEnv = lib.optionals cfg.primeOffload [
    "__NV_PRIME_RENDER_OFFLOAD=1"
    "__NV_PRIME_RENDER_OFFLOAD_PROVIDER=NVIDIA-G0"
    "__GLX_VENDOR_LIBRARY_NAME=nvidia"
    "__VK_LAYER_NV_optimus=NVIDIA_only"
    "VK_DRIVER_FILES=${nvidiaIcd}"
    "VK_ICD_FILENAMES=${nvidiaIcd}"
  ];

  session = pkgs.writeShellApplication {
    name = "macos-kvm";
    runtimeInputs = [
      tool
      reims
      macserial
      bomutils
      pkgs.mtools
      pkgs.util-linux
      pkgs.xar
      pkgs.cpio
      pkgs.gzip
      pkgs.procps
      pkgs.coreutils
      pkgs.openssh
      pkgs.libnotify
      pkgs.systemd
    ];
    text = ''
      set -euo pipefail

      export XDG_RUNTIME_DIR="''${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
      export REIMS_CTL_QEMU=${qemuShare}/bin/qemu-system-x86_64
      export REIMS_CTL_UTIL=${reims}/bin/reims-util
      export MACOS_KVM_SHARE="$HOME/SHARED_FOLDER"
      # NixOS publishes Vulkan ICDs here. A user service does not always
      # inherit the desktop's XDG_DATA_DIRS, and reims-ctl only searches those.
      case ":''${XDG_DATA_DIRS:-}:" in
        *:/run/opengl-driver/share:*) ;;
        *) export XDG_DATA_DIRS="/run/opengl-driver/share''${XDG_DATA_DIRS:+:$XDG_DATA_DIRS}" ;;
      esac
      if [[ -z "''${WAYLAND_DISPLAY:-}" && -z "''${DISPLAY:-}" ]]; then
        for sock in "$XDG_RUNTIME_DIR"/wayland-*; do
          [[ -S "$sock" ]] || continue
          WAYLAND_DISPLAY="$(basename "$sock")"
          export WAYLAND_DISPLAY
          break
        done
      fi

      ${lib.concatMapStrings (line: "export ${line}\n") primeEnv}

      export MACOS_KVM_REIMS=${reims}/bin/.reims-ctl-wrapped
      export MACOS_KVM_UTIL=${reims}/bin/reims-util
      export MACOS_KVM_MACSERIAL=${macserial}/bin/macserial
      export MACOS_KVM_NAME=${lib.escapeShellArg cfg.name}
      export MACOS_KVM_ACCOUNT=${lib.escapeShellArg account}
      export MACOS_KVM_HOST_USER=${lib.escapeShellArg cfg.user}
      export MACOS_KVM_MACOS=${lib.escapeShellArg cfg.macos}
      export MACOS_KVM_DISK=${lib.escapeShellArg cfg.diskSize}
      export MACOS_KVM_RESOLUTION=${lib.escapeShellArg cfg.resolution}
      export MACOS_KVM_HIDPI=${if cfg.hidpi then "true" else "false"}
      export MACOS_KVM_BORDERLESS=${if cfg.borderless then "true" else "false"}
      export MACOS_KVM_AUDIO=${if cfg.audio then "true" else "false"}
      export MACOS_KVM_SSH_PORT=${toString (if cfg.sshPort == null then 2222 else cfg.sshPort)}
      export MACOS_KVM_OFFLINE=${if cfg.offline then "true" else "false"}
      export MACOS_KVM_GPU=${if cfg.primeOffload then lib.escapeShellArg nvidiaIcd else "auto"}
      export MACOS_KVM_CPUS=${lib.escapeShellArg (if cfg.cpus == null then "" else toString cfg.cpus)}
      export MACOS_KVM_MEMORY=${lib.escapeShellArg (if cfg.memory == null then "" else cfg.memory)}
      export MACOS_KVM_PASSWORD_FILE=${lib.escapeShellArg (if cfg.accountPasswordFile == null then "" else cfg.accountPasswordFile)}
      export MACOS_KVM_PASSWORD=${lib.escapeShellArg (if cfg.accountPassword == null then "" else cfg.accountPassword)}
      export MACOS_KVM_LOGIN_WINDOW=${if cfg.loginWindow then "true" else "false"}
      exec ${tool}/bin/macos-kvm "$@"
    '';
  };
in
{
  options.macos-kvm = {
    enable = lib.mkEnableOption ''
      an x86-64 macOS guest on KVM with Reims vGPU (Metal on host Vulkan,
      one resizable window per display)
    '';

    user = lib.mkOption {
      type = lib.types.str;
      default = "alex";
      description = "Desktop user that owns the VM and the session its window opens in.";
    };

    name = lib.mkOption {
      type = lib.types.str;
      default = "tahoe";
      description = "reims-ctl VM name.";
    };

    macos = lib.mkOption {
      type = lib.types.str;
      default = "26";
      description = ''
        macOS release reims-util asks Apple for. 26 (Tahoe) is the last
        Intel macOS. A name (tahoe) or latest also works.
      '';
    };

    accountName = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      description = ''
        Short name of the account created on the guest. Null uses
        macos-kvm.user. This name is fixed at install; a later change does
        not rename the account. A later change of accountPassword does
        change the password of the account that was installed.
      '';
    };

    accountPassword = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      description = ''
        Password of the guest account. When this changes after the guest is
        installed, the next start applies it over SSH. The value is part of
        the NixOS system and is readable by every user on the machine. Use
        accountPasswordFile instead when the password must stay out of the
        store. Null generates a password into the VM directory.
      '';
    };

    accountPasswordFile = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      description = ''
        Runtime path of a file containing the guest account password. A nix
        path would be copied into the world-readable store, so this is a
        string read when the install runs. Used when accountPassword is
        null. Null generates a password at
        ~/.local/share/reims-ctl/vms/<name>/account.password (mode 0600).
      '';
    };

    loginWindow = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = ''
        Show the login window. The default, false, signs the account in
        when the guest boots and opens the desktop. A later change is
        applied to an installed guest on the next start.
      '';
    };

    offline = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = ''
        Download the full installer (about 17 GiB) when the VM is created.
        The headless install uses that image either way: Recovery's small
        image does not contain startosinstall.
      '';
    };

    cpus = lib.mkOption {
      type = lib.types.nullOr lib.types.ints.positive;
      default = null;
      description = "vCPUs. Null uses reims-ctl's default: half the host, clamped to 2–8.";
    };

    memory = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      example = "8G";
      description = "Guest RAM. Null uses half the host, clamped to 4–16 GiB.";
    };

    diskSize = lib.mkOption {
      type = lib.types.str;
      default = "128G";
      description = "Sparse qcow2 size for a new disk.";
    };

    resolution = lib.mkOption {
      type = lib.types.str;
      default = "3840x2160";
      description = ''
        The mode the guest starts in. With hidpi, reims-ctl halves each
        edge, so 3840x2160 looks like 1920x1080. After the window settles
        on a new size, the Reims display offers that size and the guest
        switches to it.
      '';
    };

    hidpi = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = "Retina: the guest framebuffer is twice the mode on each edge.";
    };

    borderless = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = ''
        Borderless windows fill a screen and do not follow a drag.
        Leave this off so the guest display tracks the window.
      '';
    };

    audio = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = "USB audio on the host. Safari's media players need an output device.";
    };

    sshPort = lib.mkOption {
      type = lib.types.nullOr lib.types.port;
      default = 2222;
      description = "Forward this localhost port to the guest's SSH, once Remote Login is on.";
    };

    primeOffload = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = ''
        Render the guest on the discrete NVIDIA GPU while the compositor
        stays on the iGPU. Sets the PRIME variables and the NVIDIA Vulkan
        ICD for the VM process only.
      '';
    };

    autostart = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = ''
        On a graphical session: create the VM if it is missing, install macOS
        if the disk is empty, then start the window.
      '';
    };

    maxMapCount = lib.mkOption {
      type = lib.types.ints.positive;
      default = 2097152;
      description = ''
        vm.max_map_count. Reims maps guest pages for the GPU. 2097152 covers
        a guest up to about 30 GiB; on NVIDIA it also avoids copying guest
        memory once the mapping budget is spent.
      '';
    };

    stopTimeout = lib.mkOption {
      type = lib.types.ints.positive;
      default = 90;
      description = "Seconds to wait for macOS to shut down before powering the VM off.";
    };
  };

  config = lib.mkMerge [
    (lib.mkIf cfg.enable {
    assertions = [
      {
        assertion = pkgs.stdenv.hostPlatform.isx86_64 && pkgs.stdenv.hostPlatform.isLinux;
        message = "macos-kvm runs an x86-64 macOS guest and is built for x86_64-linux.";
      }
      {
        assertion = cfg.sshPort != null;
        message = "macos-kvm.sshPort is how the headless install knows the guest booted.";
      }
    ];

    boot.kernelModules = [ "udmabuf" ];
    boot.extraModprobeConfig = ''
      options kvm ignore_msrs=1 report_ignored_msrs=0
    '';
    boot.kernel.sysctl."vm.max_map_count" = cfg.maxMapCount;

    # The active seat gets an ACL. A 0660 kvm node stays closed for a session
    # that logged in before the account joined kvm.
    services.udev.extraRules = ''
      KERNEL=="udmabuf", GROUP="kvm", MODE="0660", TAG+="uaccess"
    '';

    users.users.${cfg.user}.extraGroups = [ "kvm" ];

    # QEMU's slirp SMB server runs the smbd path it was built with, which is
    # /usr/sbin/smbd, and that process has to bind port 445. The wrapper is
    # the only thing on the host that listens, and only on 127.0.0.1, which
    # is where the guest's 10.0.2.4 is forwarded.
    security.wrappers.smbd = {
      owner = "root";
      group = "root";
      capabilities = "cap_net_bind_service+ep";
      source = "${pkgs.samba}/bin/smbd";
    };
    systemd.tmpfiles.rules = [
      "d /usr/sbin 0755 root root -"
      "L+ /usr/sbin/smbd - - - - /run/wrappers/bin/smbd"
      "d /home/${cfg.user}/SHARED_FOLDER 0755 ${cfg.user} users -"
    ];

    environment.systemPackages = [
      reims
      session
    ];

    systemd.user.services.macos-kvm = lib.mkIf cfg.autostart {
      description = "macOS ${cfg.macos} guest (${cfg.name}, Reims vGPU)";
      wantedBy = [ "graphical-session.target" ];
      after = [ "graphical-session.target" ];
      partOf = [ "graphical-session.target" ];
      serviceConfig = {
        # simple, not oneshot: activation starts the install and returns.
        # The install itself runs for as long as Apple's installer takes.
        Type = "simple";
        TimeoutStartSec = "infinity";
        ExecStart = "${session}/bin/macos-kvm ensure";
        ExecStop = "${session}/bin/macos-kvm stop ${cfg.name} --timeout ${toString cfg.stopTimeout}";
      };
    };

    systemd.user.services.macos-kvm-tray = {
      description = "macOS guest menu";
      wantedBy = [ "graphical-session.target" ];
      after = [ "graphical-session.target" ];
      partOf = [ "graphical-session.target" ];
      serviceConfig = {
        ExecStart = "${session}/bin/macos-kvm tray";
        Restart = "on-failure";
        RestartSec = 3;
      };
    };
    })
    (lib.mkIf (cfg.enable && options ? home-manager) {
      home-manager.users.${cfg.user}.programs.ssh.includes = [
        "${config.users.users.${cfg.user}.home}/.local/share/reims-ctl/vms/*/ssh/config"
      ];
    })
  ];
}
