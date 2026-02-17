{
  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-unstable";
  };

  outputs = { self, nixpkgs, ... }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" "aarch64-darwin" "x86_64-darwin" ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
    in {
      packages = forAllSystems (system:
        let pkgs = import nixpkgs { inherit system; };
        in {
          default = pkgs.rustPlatform.buildRustPackage {
            pname = "leshy";
            version = "0.1.1";
            src = ./.;

            cargoLock = { lockFile = ./Cargo.lock; };
            nativeBuildInputs = [ pkgs.pkg-config ];
            doCheck = false;

            meta = with pkgs.lib; {
              description = "Split-tunnel DNS server with automatic route injection";
              license = licenses.mit;
            };
          };
        });
    };
}
