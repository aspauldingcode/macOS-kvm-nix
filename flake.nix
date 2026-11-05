{
  description = "macOS guests on Linux KVM, with Reims vGPU (Metal through Vulkan)";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      rust-overlay,
    }:
    let
      system = "x86_64-linux";
      pkgs = import nixpkgs {
        inherit system;
        overlays = [ rust-overlay.overlays.default ];
        config.allowUnfree = true;
      };
      reims = pkgs.callPackage ./pkgs/reims-ctl.nix { };
      macserial = pkgs.callPackage ./pkgs/macserial.nix { };
      # crate2nix. libdbus is what the tray's StatusNotifier item links.
      macos-kvm = (
        pkgs.callPackage ./crates/macos-kvm/Cargo.nix {
          defaultCrateOverrides = pkgs.defaultCrateOverrides // {
            libdbus-sys = attrs: {
              nativeBuildInputs = (attrs.nativeBuildInputs or [ ]) ++ [ pkgs.pkg-config ];
              buildInputs = (attrs.buildInputs or [ ]) ++ [ pkgs.dbus ];
            };
          };
        }
      ).rootCrate.build;
    in
    {
      packages.${system} = {
        inherit reims macserial macos-kvm;
        default = reims;
      };

      nixosModules.default = import ./modules/nixos.nix { inherit self; };
    };
}
