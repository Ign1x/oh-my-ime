{
  lib,
  rustPlatform,
  pkg-config,
  makeWrapper,
  addDriverRunpath,
  fontconfig,
  freetype,
  libGL,
  libxkbcommon,
  vulkan-loader,
  wayland,
}:

let
  workspace = (lib.importTOML ../Cargo.toml).workspace.package;
  cargoFlags = lib.concatMap (crate: [
    "-p"
    crate
  ]);
  # The test tools in tests/e2e are not installed.
  installed = [
    "omimed"
    "omime-cli"
    "omime-echo"
    "omime-sparkle"
  ];
  # Libraries Sparkle's toolkit (winit, glutin, fontique) opens at runtime.
  sparkleRuntimeLibs = [
    fontconfig
    freetype
    libGL
    libxkbcommon
    vulkan-loader
    wayland
  ];
in
rustPlatform.buildRustPackage {
  pname = "oh-my-ime";
  inherit (workspace) version;

  src = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [
      ../Cargo.toml
      ../Cargo.lock
      ../crates
      ../engines
      # A workspace member: cargo reads its manifest even though it is not built.
      ../tests
      ../data
    ];
  };

  cargoLock.lockFile = ../Cargo.lock;
  cargoBuildFlags = cargoFlags installed;
  cargoTestFlags = cargoFlags (
    installed
    ++ [
      "omime-proto"
      "omime-sdk"
    ]
  );

  nativeBuildInputs = [
    pkg-config
    makeWrapper
    addDriverRunpath
  ];
  buildInputs = [
    fontconfig
    freetype
    libxkbcommon
    wayland
  ];

  postInstall = ''
    for engine in echo sparkle; do
      manifest=$out/share/oh-my-ime/engines/$engine/engine.toml
      install -Dm644 engines/$engine/engine.toml $manifest
      substituteInPlace $manifest \
        --replace-fail "exec = [\"omime-$engine\"]" "exec = [\"$out/bin/omime-$engine\"]"
    done

    install -Dm644 data/omimed.service $out/lib/systemd/user/omimed.service
    substituteInPlace $out/lib/systemd/user/omimed.service \
      --replace-fail "ExecStart=%h/.local/bin/omimed" "ExecStart=$out/bin/omimed"
    install -Dm644 data/config.toml $out/share/oh-my-ime/config.toml
    install -Dm644 -t $out/share/oh-my-ime/hyprland data/hyprland/*

    # NixOS links only some of share/ into profiles; find the bundled engines anyway.
    wrapProgram $out/bin/omimed --suffix XDG_DATA_DIRS : $out/share
  '';

  # After the fixup phase has shrunk the RPATH to what is linked.
  postFixup = ''
    patchelf --add-rpath ${lib.makeLibraryPath sparkleRuntimeLibs} $out/bin/omime-sparkle
    addDriverRunpath $out/bin/omime-sparkle
  '';

  meta = {
    description = "A modern, Wayland-first, UI-agnostic input method platform for Linux";
    homepage = "https://github.com/Ign1x/oh-my-ime";
    changelog = "https://github.com/Ign1x/oh-my-ime/blob/main/CHANGELOG.md";
    # The Sparkle engine also contains Slint, used under its royalty-free license.
    license = with lib.licenses; [
      mit
      asl20
    ];
    mainProgram = "omimed";
    platforms = lib.platforms.linux;
  };
}
