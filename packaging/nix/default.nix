{ lib, rustPlatform, installShellFiles, stdenv }:

rustPlatform.buildRustPackage {
  pname = "grava";
  version = (builtins.fromTOML (builtins.readFile ../../Cargo.toml)).package.version;

  src = lib.fileset.toSource {
    root = ../..;
    fileset = lib.fileset.unions [
      ../../Cargo.toml
      ../../Cargo.lock
      ../../src
      ../../assets
      ../../LICENSE
      ../../README.md
    ];
  };
  cargoLock.lockFile = ../../Cargo.lock;
  nativeBuildInputs = [ installShellFiles ];

  postInstall = ''
    install -Dm644 LICENSE $out/share/licenses/grava/LICENSE
    install -Dm644 README.md $out/share/doc/grava/README.md
    install -Dm644 assets/config.toml $out/share/doc/grava/config.toml
  '' + lib.optionalString (stdenv.buildPlatform.canExecute stdenv.hostPlatform) ''
    $out/bin/grava --man > grava.1
    installManPage grava.1
    installShellCompletion --cmd grava \
      --bash <($out/bin/grava --completions bash) \
      --zsh <($out/bin/grava --completions zsh) \
      --fish <($out/bin/grava --completions fish)
  '';

  meta = {
    description = "Terminal audio visualizer that draws Chladni figures with sand";
    homepage = "https://github.com/QinZinn/Grava";
    license = lib.licenses.mit;
    mainProgram = "grava";
    platforms = lib.platforms.linux;
  };
}
