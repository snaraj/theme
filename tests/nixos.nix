# A booted NixOS machine, not merely Nix installed on an Ubuntu runner.
let
  pkgs = import (builtins.fetchTarball {
    url = "https://github.com/NixOS/nixpkgs/archive/eaad089433ca2bb662274377d33df3d0e51ef28b.tar.gz";
  }) { system = "x86_64-linux"; };
  manifest = builtins.fromTOML (builtins.readFile ../Cargo.toml);
  theme = pkgs.rustPlatform.buildRustPackage {
    pname = "theme";
    version = manifest.package.version;
    src = pkgs.lib.cleanSourceWith {
      src = ../.;
      filter = path: type:
        !(builtins.elem (baseNameOf path) [ "target" ".claude" ])
        && pkgs.lib.cleanSourceFilter path type;
    };
    cargoLock.lockFile = ../Cargo.lock;
    nativeCheckInputs = [ pkgs.jq ];
    postPatch = ''
      substituteInPlace src/net.rs src/save_tests.rs \
        --replace-fail '"/usr/bin/env"' '"${pkgs.coreutils}/bin/env"'
    '';
    # The build sandbox has no POSIX ACL xattrs. Compile the unchanged unit
    # assertions here, then execute every test on the VM's real filesystem.
    checkPhase = ''
      runHook preCheck
      cargo test --workspace --release --locked --offline --target ${pkgs.stdenv.hostPlatform.rust.rustcTarget} --no-run --message-format=json > test-binaries.json
      runHook postCheck
    '';
    postCheck = ''
      for name in theme pigment; do
        binary=$(jq -er --arg name "$name" 'select(.profile.test == true and .target.name == $name and .executable != null) | .executable' test-binaries.json)
        install -Dm755 "$binary" "$out/libexec/$name-tests"
      done
    '';
  };
in
pkgs.testers.runNixOSTest {
  name = "theme-cli";
  nodes.machine = {
    environment.systemPackages = [ theme pkgs.curl pkgs.python3 ];
    users.users.tester = { isNormalUser = true; };
    virtualisation.memorySize = 2048;
    virtualisation.diskImage = "theme.qcow2"; # ext4 root, including POSIX ACLs
  };
  testScript = ''
    machine.wait_for_unit("multi-user.target")
    machine.succeed("test ! -e /usr/bin/curl")
    machine.succeed("install -d -o tester -g users /build")
    machine.succeed("su - tester -c 'mkdir -p /build/source/crates/pigment && ${theme}/libexec/pigment-tests && ${theme}/libexec/theme-tests'")
    machine.succeed("su - tester -c 'sh ${./portable-smoke.sh} ${theme}/bin/theme'")
    assert machine.succeed("su - tester -c '${theme}/bin/theme update'").strip() == "Managed by Nix. Update theme in your Nix configuration."
    machine.succeed("su - tester -c 'mkdir -p checks/tests && cp ${./browser_cli_test.py} checks/tests/browser_cli_test.py && CI=true python3 -I -B checks/tests/browser_cli_test.py ${theme}/bin/theme'")
  '';
}
