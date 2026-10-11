{
  description = "grava, a terminal Chladni sand audio visualizer";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forEachSystem = nixpkgs.lib.genAttrs systems;
    in
    {
      packages = forEachSystem (system:
        let pkgs = import nixpkgs { inherit system; };
        in rec {
          grava = pkgs.callPackage ./packaging/nix { };
          default = grava;
        });
      apps = forEachSystem (system: {
        default = {
          type = "app";
          program = "${self.packages.${system}.grava}/bin/grava";
        };
      });
      checks = forEachSystem (system: {
        build = self.packages.${system}.grava;
      });
    };
}
