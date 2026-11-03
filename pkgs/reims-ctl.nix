# Reims vGPU's Linux host stack: the Vulkan library, the QEMU it is linked into,
# reims-ctl, and reims-util. Pins match reims-vgpu 1fe0acb (2026-11) and the
# reims-util revision that reims-ctl installs.
{
  lib,
  stdenv,
  fetchFromGitHub,
  fetchzip,
  callPackage,
  rust-bin,
  makeWrapper,
  pkg-config,
  cmake,
  clang,
  lld,
  llvmPackages,
  python3,
  meson,
  ninja,
  flex,
  bison,
  perl,
  writeShellScript,
  git,
  cacert,
  glib,
  pixman,
  libslirp,
  zlib,
  dtc,
  pipewire,
  libpulseaudio,
  alsa-lib,
  libusb1,
  vulkan-loader,
  libglvnd,
  gtk3,
  cairo,
  gdk-pixbuf,
  libpng,
  libjpeg,
  libxkbcommon,
  wayland,
  wayland-scanner,
  wayland-protocols,
  libdecor,
  libdrm,
  mesa,
  dbus,
  systemd,
  openssl,
  bzip2,
  libxcb,
  libx11,
  libxext,
  libxrandr,
  libxcursor,
  libxi,
  libxfixes,
  libxscrnsaver,
  libxtst,
}:
let
  rust = rust-bin.stable."1.98.1".default;

  reimsSrc = fetchFromGitHub {
    owner = "steelbrain";
    repo = "reims-vgpu";
    rev = "1fe0acb68b287db36f1e6c7b9f45383940b8a76f";
    hash = "sha256-g3U/IOTdZcxomBcCEexnnNn/FyD5XsigjTM1SWZdUtk=";
  };

  qemuSrc = fetchFromGitHub {
    owner = "steelbrain";
    repo = "qemu-reims-vgpu";
    rev = "1b41f5f5db4d1067d023973b9bd74c7b341e0d28";
    hash = "sha256-wXy4+i/kwHxrMBkbUij5396DwbrIVzrcxV47YvehQnA=";
  };

  utilSrc = fetchFromGitHub {
    owner = "steelbrain";
    repo = "reims-util";
    rev = "ed7398997f83f9d004477e6eab8eaf46a462a47a";
    hash = "sha256-2z/cGLhUgiHQaUf3ph8bvmsi9awdpPK2PyR8RNWnvaQ=";
  };

  # QEMU's configure refuses --disable-download unless this tree is present.
  # System libslirp and libfdt cover the other wraps.
  keycodemapdb = fetchzip {
    url = "https://gitlab.com/qemu-project/keycodemapdb/-/archive/f5772a62ec52591ff6870b7e8ef32482371f22c6/keycodemapdb-f5772a62ec52591ff6870b7e8ef32482371f22c6.tar.gz";
    hash = "sha256-GbZ5mrUYLXMi0IX4IZzles0Oyc095ij2xAsiLNJwfKQ=";
  };

  vendor =
    args:
    callPackage ./vendor.nix (
      {
        rustToolchain = rust;
      }
      // args
    );

  vgpuVendor = vendor {
    name = "reims-vgpu-vendor";
    src = reimsSrc;
    saveLock = true;
    hash = "sha256-Yd0G45+7+rG3S7ScTv2bCYPO2WHMvsKBMoPn3dVlL1k=";
  };

  ctlVendor = vendor {
    name = "reims-ctl-vendor";
    src = reimsSrc;
    workDir = "crates/reims-ctl";
    hash = "sha256-yRm5xdVQ1rXjOlz2EnkkW5blyAdn7gwkYJXRgNh0Fts=";
  };

  utilVendor = vendor {
    name = "reims-util-vendor";
    src = utilSrc;
    hash = "sha256-rnoHGAxU2axtdDvQYVusIsflLUVtLC7ItDZUd0ZIkA4=";
  };

  # Headers and libraries the statically linked SDL3 build and QEMU both see.
  graphicsDeps = [
    vulkan-loader
    libglvnd
    libxkbcommon
    wayland
    wayland-protocols
    libdecor
    libdrm
    mesa
    libxcb
    libx11
    libxext
    libxrandr
    libxcursor
    libxi
    libxfixes
    libxscrnsaver
    libxtst
    dbus
    systemd
    openssl
  ];

  qemuDeps = graphicsDeps ++ [
    glib
    pixman
    libslirp
    zlib
    dtc
    pipewire
    libpulseaudio
    alsa-lib
    libusb1
    gtk3
    bzip2
  ];

  # cargo vendor's config names the store path of the vendored crates.
  # The option ROM compiles as a Windows EFI target. Nix's clang wrapper adds
  # -fPIC, which that target rejects. Other C compiles keep the wrapper.
  romClang = writeShellScript "reims-rom-clang" ''
    for a in "$@"; do
      case "$a" in
        --target=*windows*)
          exec ${llvmPackages.clang-unwrapped}/bin/clang "$@"
          ;;
      esac
    done
    exec ${clang}/bin/clang "$@"
  '';

  # Append. Replacing .cargo/config.toml drops the project's `cargo xtask` alias.
  useVendor = cargoDeps: ''
    mkdir -p .cargo
    touch .cargo/config.toml
    sed "s|@vendor@|${cargoDeps}|g" ${cargoDeps}/config.toml >> .cargo/config.toml
    if [[ -f ${cargoDeps}/Cargo.lock ]]; then
      cp ${cargoDeps}/Cargo.lock Cargo.lock
      chmod u+w Cargo.lock
    fi
    export CARGO_NET_OFFLINE=true
  '';

  dist = stdenv.mkDerivation {
    pname = "reims-vgpu-dist";
    version = "1fe0acb";
    src = reimsSrc;

    nativeBuildInputs = [
      rust
      pkg-config
      cmake
      clang
      lld
      llvmPackages.clang-unwrapped
      python3
      wayland-scanner
    ];
    buildInputs = graphicsDeps;

    configurePhase = useVendor vgpuVendor;

    buildPhase = ''
      runHook preBuild
      mkdir -p "$TMPDIR/bin"
      ln -s ${romClang} "$TMPDIR/bin/clang"
      ln -s ${clang}/bin/clang++ "$TMPDIR/bin/clang++"
      export PATH="$TMPDIR/bin:${lld}/bin:$PATH"
      export HOME=$TMPDIR
      export CARGO_HOME=$TMPDIR/cargo-home
      cargo xtask dist --platform vulkan --out "$TMPDIR/dist-vulkan"
      runHook postBuild
    '';

    installPhase = ''
      runHook preInstall
      mkdir -p $out
      cp -a "$TMPDIR/dist-vulkan/." $out/
      test -f $out/lib/pkgconfig/reims-vgpu.pc
      test -f $out/share/reims-vgpu/ReimsVgpu.rom
      runHook postInstall
    '';
  };

  qemu = stdenv.mkDerivation {
    pname = "qemu-reims-vgpu";
    version = "11.1.50-1b41f5f";
    src = qemuSrc;

    nativeBuildInputs = [
      pkg-config
      # mkvenv stays offline when pip and setuptools are already importable.
      (python3.withPackages (ps: [
        ps.pip
        ps.setuptools
        ps.wheel
      ]))
      meson
      ninja
      flex
      bison
      perl
      clang
    ];
    buildInputs = qemuDeps ++ [ dist ];

    prePatch = ''
      mkdir -p subprojects
      cp -a ${keycodemapdb} subprojects/keycodemapdb
      chmod -R u+w subprojects/keycodemapdb
    '';

    configureFlags = [
      "--prefix=${placeholder "out"}"
      "--target-list=x86_64-softmmu"
      "--enable-kvm"
      "--disable-sdl"
      "--enable-gtk"
      "--enable-slirp"
      "--enable-libusb"
      "--enable-pvg"
      "--disable-docs"
      "--disable-tests"
      "--disable-werror"
      "--disable-download"
    ];

    # QEMU's ./configure writes config-host.mak, then drives Meson itself.
    # The stdenv Meson hook skips that and meson.build fails to load it.
    dontUseMesonConfigure = true;
    dontUseMesonBuild = true;
    dontUseMesonInstall = true;
    dontUseMesonCheck = true;
    # ninja's hook would build in the source root. QEMU's Makefile
    # runs ninja in ./build.
    dontUseNinjaBuild = true;
    dontUseNinjaInstall = true;
    dontUseNinjaCheck = true;

    preConfigure = ''
      export PKG_CONFIG_PATH="${dist}/lib/pkgconfig''${PKG_CONFIG_PATH:+:}$PKG_CONFIG_PATH"
    '';

    enableParallelBuilding = true;
  };

  ctl = stdenv.mkDerivation {
    pname = "reims-ctl-bin";
    version = "0.1.0-1fe0acb";
    src = reimsSrc;

    nativeBuildInputs = [
      rust
      pkg-config
    ];
    buildInputs = [ vulkan-loader ];

    configurePhase = ''
      cd crates/reims-ctl
      ${useVendor ctlVendor}
    '';

    buildPhase = ''
      runHook preBuild
      export HOME=$TMPDIR
      export CARGO_HOME=$TMPDIR/cargo-home
      cargo build --release --offline
      runHook postBuild
    '';

    installPhase = ''
      install -Dm755 target/release/reims-ctl $out/bin/reims-ctl
    '';
  };

  util = stdenv.mkDerivation {
    pname = "reims-util";
    version = "0.1.0-ed73989";
    src = utilSrc;

    nativeBuildInputs = [
      rust
      pkg-config
    ];

    configurePhase = useVendor utilVendor;

    buildPhase = ''
      runHook preBuild
      export HOME=$TMPDIR
      export CARGO_HOME=$TMPDIR/cargo-home
      cargo build --release --offline
      runHook postBuild
    '';

    installPhase = ''
      install -Dm755 target/release/reims-util $out/bin/reims-util
    '';
  };
in
stdenv.mkDerivation {
  pname = "reims-ctl";
  version = "1fe0acb";

  dontUnpack = true;
  dontBuild = true;

  nativeBuildInputs = [ makeWrapper ];
  buildInputs = [
    vulkan-loader
    libglvnd
  ];

  installPhase = ''
    runHook preInstall
    mkdir -p $out/bin
    # QEMU's Meson link does not record NIX_LDFLAGS, so the binaries have
    # no rpath. The wrappers supply the libraries they were linked against.
    libpath="${
      lib.makeLibraryPath (
        qemuDeps
        ++ [
          cairo
          gdk-pixbuf
          libpng
          libjpeg
          vulkan-loader
          libglvnd
        ]
      )
    }"
    for b in qemu-system-x86_64 qemu-img qemu-nbd; do
      if [[ -x ${qemu}/bin/$b ]]; then
        makeWrapper ${qemu}/bin/$b $out/bin/$b --prefix LD_LIBRARY_PATH : "$libpath"
      fi
    done
    ln -s ${ctl}/bin/reims-ctl $out/bin/reims-ctl
    ln -s ${util}/bin/reims-util $out/bin/reims-util
    wrapProgram $out/bin/reims-ctl \
      --set REIMS_CTL_QEMU $out/bin/qemu-system-x86_64 \
      --set REIMS_CTL_UTIL $out/bin/reims-util \
      --prefix LD_LIBRARY_PATH : "$libpath"
    runHook postInstall
  '';

  meta = {
    description = "macOS VMs on QEMU with Reims vGPU (Metal commands on host Vulkan)";
    homepage = "https://reims-vgpu.com/";
    platforms = [ "x86_64-linux" ];
    mainProgram = "reims-ctl";
  };
}
