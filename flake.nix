{
  description = "nar-toolbox";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs?ref=nixos-unstable";
    crane.url = "github:ipetkov/crane";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, rust-overlay, crane }:
    let
      inherit (nixpkgs.lib) nameValuePair mergeAttrsList listToAttrs singleton;
      # Pairs of localSystem tp multiple crossSystems.
      systems = [
        { localSystem = "x86_64-linux"; crossSystems = singleton "aarch64-linux"; }
        { localSystem = "aarch64-linux"; crossSystems = singleton "x86_64-linux"; }
        { localSystem = "x86_64-darwin"; crossSystems = []; }
        { localSystem = "aarch64-darwin"; crossSystems = singleton "x86_64-darwin"; }
      ];

      forAllSystems = f:
        listToAttrs (map (args:
          nameValuePair args.localSystem
            (mergeAttrsList (map (crossSystem:
              f {
                localSystem = args.localSystem;
                crossSystem = if crossSystem == null then args.localSystem else crossSystem;
              }) (singleton args.localSystem ++ args.crossSystems)
            ))
        ) systems);
    in
    {
      packages = forAllSystems ({ localSystem, crossSystem }:
        let
          overlays = [ (import rust-overlay) ];
          pkgs = import nixpkgs {
            inherit localSystem crossSystem overlays;
          };
          inherit (pkgs) lib;

          target =
            if crossSystem == "x86_64-linux" then "x86_64-unknown-linux-musl"
            else if crossSystem == "aarch64-linux" then "aarch64-unknown-linux-musl"
            else if crossSystem == "x86_64-darwin" then "x86_64-apple-darwin"
            else if crossSystem == "aarch64-darwin" then "aarch64-apple-darwin"
            else throw "unsupported target system";

          toolchain = p: p.rust-bin.stable.latest.default.override {
            targets = [target];
          };

          craneLib = (crane.mkLib pkgs).overrideToolchain toolchain;

          nar-toolbox = craneLib.buildPackage {
            src = craneLib.cleanCargoSource ./.;
            strictDeps = true;

            CARGO_BUILD_TARGET = target;
            CARGO_BUILD_RUSTFLAGS = "-C target-feature=+crt-static";
          };
        in ((lib.optionalAttrs (localSystem == crossSystem) {
          inherit nar-toolbox;
          default = nar-toolbox;
        }) // (lib.optionalAttrs (localSystem != crossSystem) {
          "cross-${crossSystem}-nar-toolbox" = nar-toolbox;
        }))
      );
    };
}
