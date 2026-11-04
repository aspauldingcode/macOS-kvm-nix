# macserial is the OpenCore utility osx-serial-generator shells out to. It prints a
# model-correct serial and logic-board serial. The flake writes those into
# reims-util's OpenCore image, which already carries ReimsVgpu.kext. Building
# sickcodes' boot disk would replace that image.
{
  stdenv,
  fetchFromGitHub,
}:
stdenv.mkDerivation {
  pname = "macserial";
  version = "1.0.6-unstable-2026-10-31";

  src = fetchFromGitHub {
    owner = "acidanthera";
    repo = "OpenCorePkg";
    rev = "8d8d690cb543f63114340cdae958de4ce7a8296f";
    hash = "sha256-VKKUsPU0RJzvGwQ2+tUGq2H/foR2Rq7ZK1xdcFsDmDs=";
    sparseCheckout = [
      "Utilities/macserial"
      "User/Include/UserPseudoRandom.h"
      "User/Library/UserPseudoRandom.c"
    ];
  };

  buildPhase = ''
    runHook preBuild
    $CC -O2 -D EFIUSER \
      -I Utilities/macserial -I User/Include \
      -c Utilities/macserial/macserial.c -o macserial.o
    $CC -O2 -D EFIUSER -I User/Include \
      -c User/Library/UserPseudoRandom.c -o pseudo.o
    $CC -o macserial macserial.o pseudo.o
    runHook postBuild
  '';

  installPhase = ''
    runHook preInstall
    install -Dm755 macserial $out/bin/macserial
    runHook postInstall
  '';

  meta = {
    description = "OpenCore macserial, the generator osx-serial-generator runs";
    homepage = "https://github.com/acidanthera/OpenCorePkg";
    platforms = [ "x86_64-linux" ];
    mainProgram = "macserial";
  };
}
