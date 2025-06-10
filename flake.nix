{
  description = "TeamSpeak 3 Music Bot";
  inputs = {
    nixpkgs.url = "nixpkgs/nixos-25.05";
    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = {
    self,
    nixpkgs,
    fenix,
  }: let
    supportedSystems = ["x86_64-linux"];
    forAllSystems = nixpkgs.lib.genAttrs supportedSystems;
    pkgsFor = nixpkgs.legacyPackages;
  in {
    packages = forAllSystems (system: {
      default = let
        toolchain = fenix.packages.${system}.stable.toolchain;
      in
        pkgsFor.${system}.callPackage ./package.nix {
          rustPlatform = pkgsFor.${system}.makeRustPlatform {
            cargo = toolchain;
            rustc = toolchain;
          };
        };
    });
    devShells = forAllSystems (system: {
      default = pkgsFor.${system}.callPackage ./shell.nix {};
    });
  };
}
