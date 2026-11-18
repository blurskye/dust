{
  description = "Dusty-owned Linux executable compatibility runtime";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/b6018f87da91d19d0ab4cf979885689b469cdd41";

  outputs = { nixpkgs, ... }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forEachSystem = nixpkgs.lib.genAttrs systems;
    in {
      packages = forEachSystem (system:
        let
          pkgs = import nixpkgs { inherit system; };
          loader = if system == "aarch64-linux"
            then "${pkgs.glibc}/lib/ld-linux-aarch64.so.1"
            else "${pkgs.glibc}/lib/ld-linux-x86-64.so.2";
          runtimeLibraries = with pkgs; [
            stdenv.cc.cc.lib
            zlib
            openssl
            gmp
            ncurses
            brotli.lib
            libxkbcommon
            libinput
            libgbm
            unixODBC
            glib.out
            fontconfig.lib
            freetype
            systemdLibs
            icu
            nss
            curl
            expat
            bzip2
            xz
            zstd
            libxml2
            libssh
            libsodium
          ];
          libraryPath = pkgs.lib.makeLibraryPath runtimeLibraries;
          runtimePath = pkgs.lib.makeBinPath [ pkgs.nodejs pkgs.jdk_headless ];
          compatRuntime = pkgs.runCommand "dusty-linux-compat-runtime" { } ''
            mkdir -p "$out/bin" "$out/share/dusty"
            ln -s ${pkgs.nix-ld}/bin/nix-ld "$out/bin/dusty-nix-ld"
            ln -s ${pkgs.patchelf}/bin/patchelf "$out/bin/dusty-patchelf"
            cat > "$out/share/dusty/runtime.env" <<EOF
DUSTY_NIX_RUNTIME_ID=v1-b6018f87da91d19d0ab4cf979885689b469cdd41-common-libraries-1
DUSTY_NIX_LOADER=${loader}
DUSTY_NIX_LD=${pkgs.nix-ld}/bin/nix-ld
DUSTY_NIX_PATCHELF=${pkgs.patchelf}/bin/patchelf
DUSTY_NIX_LIBRARY_PATH=${libraryPath}
DUSTY_NIX_RUNTIME_PATH=${runtimePath}
EOF
          '';
        in {
          compat-runtime = compatRuntime;
          default = compatRuntime;
        });
    };
}
