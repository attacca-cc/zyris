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
        default = zyris;
      };

      devShells.${system}.default = pkgs.callPackage ./nix/shell.nix { };

      # For a NixOS configuration: `nixpkgs.overlays = [ zyris.overlays.default ];`, then
      # `environment.systemPackages = [ pkgs.zyris ];`.
      overlays.default = final: prev: { zyris = final.callPackage ./nix/package.nix { }; };
    };
}
