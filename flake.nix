{
  description = "Dust build environment";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/825e2028c29b702a4a5f085f08095d12099784f2";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { nixpkgs, rust-overlay, ... }:
    let
      pkgs = import nixpkgs {
        system = "x86_64-linux";
        overlays = [ rust-overlay.overlays.default ];
      };
      shell = pkgs.mkShell {
        packages = [
          (pkgs.rust-bin.stable."1.97.0".default.override { extensions = [ "clippy" "rustfmt" ]; })
          pkgs.pkg-config
          pkgs.openssl
          pkgs.zlib
          pkgs.git
          pkgs.python3
        ];
      };
    in {
      devShells.x86_64-linux = {
        linux = shell;
        guest = pkgs.mkShell {
          packages = [
            (pkgs.rust-bin.stable."1.97.0".default.override {
              targets = [ "wasm32-unknown-unknown" ];
            })
          ];
        };
        default = shell;
      };
    };
}
