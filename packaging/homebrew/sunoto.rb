# typed: strict
# frozen_string_literal: true

# Homebrew formula for Sunoto, the local push-to-talk dictation daemon.
#
# Lives in the tap `shakdwipeea/sunoto` (repository `homebrew-sunoto`); this
# copy in the main repository is the source of truth and is mirrored there.
# Head-only until the first tagged release: `brew install --HEAD sunoto`.
#
# Layout under libexec mirrors a repository checkout, because the daemon
# resolves sidecar scripts, virtualenvs, and the overlay relative to one
# root: services/, src/, target/release/{sunoto-daemon,sunoto-overlay},
# .venv-nemotron-mac/, .venv-llm-polish-mac/.
#
# This tap formula installs Python packages from PyPI during the build,
# which core Homebrew would not allow. They are pinned to the versions the
# project was benchmarked with.
class Sunoto < Formula
  desc "Local push-to-talk voice dictation for macOS (Parakeet-MLX, llama.cpp polish)"
  homepage "https://github.com/shakdwipeea/voice-dictation"
  license "MIT"
  head "https://github.com/shakdwipeea/voice-dictation.git", branch: "master"

  depends_on "cmake" => :build
  depends_on "rust" => :build
  depends_on :macos
  depends_on "python@3.12"

  def install
    odie "swiftc not found; install the Xcode Command Line Tools: xcode-select --install" unless which("swiftc")

    # The daemon lands in libexec/bin first (cargo's layout), then moves next
    # to the overlay so the daemon finds it as a sibling.
    system "cargo", "install", *std_cargo_args(root: libexec, path: "apps/daemon")
    system "swiftc", "-O", "services/macos/sunoto-overlay.swift", "-o", "sunoto-overlay"
    (libexec/"target/release").install "sunoto-overlay"
    (libexec/"target/release").install libexec/"bin/sunoto-daemon"
    rm_r(libexec/"bin")
    libexec.install "services", "src", "AGENTS.md", "README.md", "docs"

    python = formula_opt_bin("python@3.12")/"python3.12"

    # Speech recognition runtime (Parakeet-MLX on Apple silicon).
    asr = libexec/".venv-nemotron-mac"
    system python, "-m", "venv", asr
    system asr/"bin/pip", "install", "--upgrade", "pip"
    system asr/"bin/pip", "install", "parakeet-mlx==0.5.2", "mlx==0.31.2"

    # LLM polish runtime (llama.cpp with Metal). The model itself is
    # downloaded by `sunoto setup`, not at install time.
    polish = libexec/".venv-llm-polish-mac"
    system python, "-m", "venv", polish
    system polish/"bin/pip", "install", "--upgrade", "pip"
    ENV["CMAKE_ARGS"] = "-DGGML_METAL=on"
    system polish/"bin/pip", "install", "llama-cpp-python==0.3.31"

    bin.install_symlink libexec/"target/release/sunoto-daemon"
    (bin/"sunoto").write <<~SH
      #!/bin/bash
      # User-facing entry point: same subcommands as sunoto-daemon, with the
      # install root pinned so sidecars and virtualenvs resolve under libexec.
      export SUNOTO_ROOT="#{libexec}"
      exec "#{libexec}/target/release/sunoto-daemon" "$@"
    SH
  end

  def caveats
    <<~EOS
      Finish the install (builds Sunoto.app, registers it at login, starts it,
      and waits until it reports ready):

        sunoto setup

      Then grant Accessibility and Input Monitoring to ~/Applications/Sunoto.app
      when the installer opens those panes, and allow the Microphone prompt.

      Everyday commands: sunoto status, sunoto restart, sunoto log,
      sunoto uninstall. Configuration:
      ~/Library/Application Support/sunoto/config.json

      Upgrades re-sign the bundle, so macOS asks for the two grants again.
    EOS
  end

  test do
    ENV["HOME"] = testpath
    ENV["SUNOTO_ROOT"] = libexec
    system bin/"sunoto", "config", "init"
    assert_path_exists testpath/"Library/Application Support/sunoto/config.json"
    output = shell_output("#{bin}/sunoto config show")
    assert_match "parakeet_mlx_streaming", output
  end
end
