{
  description = "Gevulot Stuff";

  inputs.nixpkgs.url = "nixpkgs/nixos-unstable";
  inputs.flake-utils.url = "github:numtide/flake-utils";

  outputs = { self, nixpkgs, flake-utils }:

    # Add dependencies that are only needed for development
    flake-utils.lib.eachDefaultSystem
      (system:
        let
          pkgs = import nixpkgs {
            inherit system;
          };
        in
        {
          devShells.default = let p = pkgs; in
            pkgs.mkShell {
              buildInputs =
              [
                p.openssl
                p.ops
                p.pkg-config
                p.protobuf
              ];
            };
        });
}
