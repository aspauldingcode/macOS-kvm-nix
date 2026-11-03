# Fixed-output cargo vendor.
#
# reims-vgpu's committed lock omits the git source lines for metal2vulkan.
# `cargo fetch` records them, then `cargo vendor` freezes the tree the
# offline build uses.
{
  lib,
  stdenvNoCC,
  rustToolchain,
  git,
  cacert,
  pkg-config,
  src,
  name,
  hash,
  workDir ? null,
  # cargo fetch records git dependencies the committed lock omits.
  saveLock ? false,
}:
stdenvNoCC.mkDerivation {
  pname = name;
  version = "vendor";

  inherit src;

  nativeBuildInputs = [
    rustToolchain
    git
    cacert
    pkg-config
  ];

  dontConfigure = true;

  buildPhase = ''
    runHook preBuild
    export HOME=$TMPDIR
    export CARGO_HOME=$TMPDIR/cargo-home
    export SSL_CERT_FILE=${cacert}/etc/ssl/certs/ca-bundle.crt
    export GIT_SSL_CAINFO=$SSL_CERT_FILE
    ${lib.optionalString (workDir != null) "cd ${workDir}"}
    cargo fetch
    mkdir -p "$out"
    cargo vendor --versioned-dirs "$out/vendor" > "$TMPDIR/vendor-config.toml"
    # metal2vulkan's build script resolves path = "../metal2vulkan-air"
    # from the crate directory. cargo vendor only writes the versioned name.
    if [[ -d "$out/vendor/metal2vulkan-air-0.2.0" ]]; then
      ln -s metal2vulkan-air-0.2.0 "$out/vendor/metal2vulkan-air"
    fi
    # The printed config names this output. A fixed-output derivation cannot
    # contain its own store path, so the consumer rewrites the token.
    sed "s|$out|@vendor@|g" "$TMPDIR/vendor-config.toml" > "$out/config.toml"
    ${lib.optionalString saveLock "cp Cargo.lock $out/Cargo.lock"}
    runHook postBuild
  '';

  installPhase = "true";

  # Shebang patching rewrites scripts to this nixpkgs' bash, so the hash
  # would change whenever bash's store path does.
  dontFixup = true;

  outputHashMode = "recursive";
  outputHashAlgo = "sha256";
  outputHash = hash;
}
