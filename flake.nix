{
  description = "Disable the laptop touchpad while an external mouse is connected";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs =
    { self, nixpkgs, flake-utils }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import nixpkgs { inherit system; };
      in
      {
        packages.default = pkgs.rustPlatform.buildRustPackage {
          pname = "touchpad-toggle-daemon";
          version = "0.1.0";

          src = ./.;
          cargoLock.lockFile = ./Cargo.lock;

          nativeBuildInputs = [
            pkgs.pkg-config
            pkgs.makeWrapper
          ];
          buildInputs = [ pkgs.udev ];

          # xinput is a runtime, not build-time, dependency: the daemon shells
          # out to it, so put it on PATH rather than linking against it.
          postFixup = ''
            wrapProgram $out/bin/touchpad-toggle-daemon \
              --prefix PATH : ${pkgs.lib.makeBinPath [ pkgs.xorg.xinput ]}
          '';

          meta = with pkgs.lib; {
            description = "Disable the laptop touchpad while an external mouse is connected";
            license = licenses.bsd3;
            mainProgram = "touchpad-toggle-daemon";
          };
        };

        devShells.default = pkgs.mkShell {
          inputsFrom = [ self.packages.${system}.default ];
          packages = [ pkgs.xorg.xinput ];
        };
      }
    );
}
