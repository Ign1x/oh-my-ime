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
  librime,
  rime-data,
  vulkan-loader,
  wayland,
}:

# oh-my-ime's Rime engine, apart from the platform: librime and Rime's data are large,
# and only users of Rime need them. omimed finds the engine through its manifest in
# share/oh-my-ime/engines, in a profile or (with the NixOS module) on its search path.
let
  workspace = (lib.importTOML ../Cargo.toml).workspace.package;
  # Libraries Slint's toolkit (winit, glutin, fontique) opens at runtime.
  runtimeLibs = [
    fontconfig
    freetype
    libGL
    libxkbcommon
    vulkan-loader
    wayland
  ];
in
rustPlatform.buildRustPackage {
  pname = "oh-my-ime-rime";
  inherit (workspace) version;

  src = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [
      ../Cargo.toml
      ../Cargo.lock
      ../crates
      ../engines
      # Workspace members: cargo reads their manifests even though they are not built.
      ../tests/e2e
    ];
  };

  cargoLock.lockFile = ../Cargo.lock;
  cargoBuildFlags = [
    "-p"
    "omime-rime"
  ];
  cargoTestFlags = [
    "-p"
    "omime-rime"
  ];

  nativeBuildInputs = [
    pkg-config
    makeWrapper
    addDriverRunpath
  ];
  buildInputs = [
    fontconfig
    freetype
    libxkbcommon
    librime
    wayland
  ];

  postInstall = ''
    manifest=$out/share/oh-my-ime/engines/rime/engine.toml
    install -Dm644 engines/rime/engine.toml $manifest
    # The engine's command line and its settings window's.
    substituteInPlace $manifest \
      --replace-fail '["omime-rime"' "[\"$out/bin/omime-rime\""
    # Rime's schemas and dictionaries, unless the user points elsewhere.
    wrapProgram $out/bin/omime-rime \
      --set-default OMIME_RIME_SHARED_DATA ${rime-data}/share/rime-data
  '';

  # After the fixup phase has shrunk the RPATH to what is linked.
  postFixup = ''
    patchelf --add-rpath ${lib.makeLibraryPath runtimeLibs} $out/bin/.omime-rime-wrapped
    addDriverRunpath $out/bin/.omime-rime-wrapped
  '';

  meta = {
    description = "oh-my-ime's Rime engine: Xiaohe shuangpin, pinyin and Rime's other schemas";
    homepage = "https://github.com/Ign1x/oh-my-ime";
    # The engine also contains Slint, used under its royalty-free license; librime is
    # BSD-3-Clause and Rime's data LGPL-3.0.
    license = with lib.licenses; [
      mit
      asl20
    ];
    mainProgram = "omime-rime";
    platforms = lib.platforms.linux;
  };
}
