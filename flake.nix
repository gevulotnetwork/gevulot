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
                p.git
                p.opentofu
                p.google-cloud-sdk-gce
                (p.google-cloud-sdk.withExtraComponents [p.google-cloud-sdk.components.gke-gcloud-auth-plugin])
                p.k9s
                p.kube-capacity
                #p.helix
                # Language Servers
                #p.nodePackages_latest.bash-language-server
                #p.nodePackages_latest.dockerfile-language-server-nodejs
                #p.terraform-ls
                #p.nodePackages_latest.vscode-langservers-extracted
                #p.marksman
                #p.nil
                #p.python311Packages.python-lsp-server
                #p.taplo
                #p.yaml-language-server
                #p.ansible-language-server
              ];
            };
        });
}
