{
  description = "Zyris: a desktop node for Attacca";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      system = "x86_64-linux";
      pkgs = nixpkgs.legacyPackages.${system};
    in
    {
      packages.${system} = rec {
        zyris = pkgs.callPackage ./nix/package.nix { };
        # The Lite build: the same program with the audio stack and the window taken out, so it
        # is a tray icon and a node, and it announces the same five capabilities. No web process
        # ever draws anything, which on a machine whose graphics stack WebKit cannot use is the
        # difference between a node with an empty window and a node that simply runs.
        zyris-lite = pkgs.callPackage ./nix/package.nix { lite = true; };
        default = zyris;
      };

      devShells.${system}.default = pkgs.callPackage ./nix/shell.nix { };

      # For a NixOS configuration: `nixpkgs.overlays = [ zyris.overlays.default ];`, then
      # `environment.systemPackages = [ pkgs.zyris pkgs.zyris-lite ];`.
      overlays.default = final: prev: {
        zyris = final.callPackage ./nix/package.nix { };
        zyris-lite = final.callPackage ./nix/package.nix { lite = true; };
      };
    };
}
