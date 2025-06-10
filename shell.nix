{ pkgs }:
pkgs.mkShell {
  # Get dependencies from the main package
  inputsFrom = [ (pkgs.callPackage ./package.nix { }) ];
  # Additional tooling
  buildInputs = with pkgs; [
    rust-analyzer
    rustfmt
    clippy
    bacon
  ];
}
