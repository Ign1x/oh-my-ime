# Install release binaries without a Rust toolchain. autoPatchelf supplies the Nix
# loader and libraries; GPU drivers are resolved from the running system.
{
  lib,
  stdenvNoCC,
  autoPatchelfHook,
  makeWrapper,
  gcc,
  dbus,
  fontconfig,
  freetype,
  libGL,
  libxkbcommon,
  libX11,
  libXcursor,
  libXi,
  libXrandr,
  vulkan-loader,
  wayland,
  librime,
  rime-data,
  version,
  src,
  rime ? false,
}:
let
  runtimeLibs = [
    fontconfig
    freetype
    libGL
    libxkbcommon
    libX11
    libXcursor
    libXi
    libXrandr
    vulkan-loader
    wayland
  ];
in
stdenvNoCC.mkDerivation {
  pname = if rime then "oh-my-ime-rime-bin" else "oh-my-ime-bin";
  inherit version src;
  strictDeps = true;
  dontConfigure = true;
  dontBuild = true;
  nativeBuildInputs = [
    autoPatchelfHook
    makeWrapper
  ];
  buildInputs = [
    gcc.cc.lib
    dbus
  ]
  ++ runtimeLibs
  ++ lib.optional rime librime;
  # Slint loads these libraries dynamically, so DT_NEEDED alone is insufficient.
  runtimeDependencies = map lib.getLib runtimeLibs;
  # Keep the host's GPU driver location after autoPatchelf rebuilds the RPATH.
  appendRunpaths = [ "/run/opengl-driver/lib" ];

  installPhase = ''
    runHook preInstall
    mkdir -p $out
    cp -r bin share $out/
    chmod -R u+w $out
    # The portable archive's commands are bare names; the Nix package resolves
    # every program it contains to its immutable installation path.
    for engine in echo sparkle rime ibus; do
      manifest=$out/share/oh-my-ime/engines/$engine/engine.toml
      if [ -f "$manifest" ]; then
        substituteInPlace "$manifest" \
          --replace-fail "[\"omime-$engine\"" "[\"$out/bin/omime-$engine\""
      fi
    done
    ${lib.optionalString (!rime) ''
      mkdir -p $out/lib
      mv $out/share/systemd $out/lib/
      substituteInPlace $out/lib/systemd/user/omimed.service \
        --replace-fail 'ExecStart=%h/.local/bin/omimed' "ExecStart=$out/bin/omimed" \
        --replace-fail 'ExecReload=%h/.local/bin/omime reload' "ExecReload=$out/bin/omime reload"
      substituteInPlace $out/share/applications/io.github.ign1x.OhMyIme.Settings.desktop \
        --replace-fail 'Exec=omime-settings' "Exec=$out/bin/omime-settings"
    ''}
    runHook postInstall
  '';

  # autoPatchelf's post-fixup hook processes the wrapped ELF programs as well.
  postFixup = ''
    ${
      if rime then
        ''
          wrapProgram $out/bin/omime-rime \
            --set-default OMIME_RIME_SHARED_DATA ${rime-data}/share/rime-data
        ''
      else
        ''
          wrapProgram $out/bin/omimed \
            --suffix XDG_DATA_DIRS : $out/share \
            --prefix PATH : ${lib.makeBinPath [ dbus ]}
        ''
    }
  '';

  doInstallCheck = true;
  installCheckPhase = ''
    runHook preInstallCheck
    ${
      if rime then
        ''
          patchelf --print-rpath $out/bin/.omime-rime-wrapped | grep -q /run/opengl-driver/lib
        ''
      else
        ''
          LC_ALL=C $out/bin/omimed --version
          LC_ALL=C $out/bin/omime help > /dev/null
          LC_ALL=C $out/bin/omime-settings --help > /dev/null
          patchelf --print-rpath $out/bin/omime-sparkle | grep -q /run/opengl-driver/lib
          patchelf --print-rpath $out/bin/omime-settings | grep -q /run/opengl-driver/lib
        ''
    }
    runHook postInstallCheck
  '';
  meta = {
    description =
      if rime then "Prebuilt Rime engine for oh-my-ime" else "Prebuilt oh-my-ime input method platform";
    homepage = "https://github.com/Ign1x/oh-my-ime";
    license = with lib.licenses; [
      mit
      asl20
    ];
    mainProgram = if rime then "omime-rime" else "omimed";
    platforms = [
      "x86_64-linux"
      "aarch64-linux"
    ];
    sourceProvenance = [ lib.sourceTypes.binaryNativeCode ];
  };
}
