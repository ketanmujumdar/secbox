# typed: false
# frozen_string_literal: true

# Homebrew formula TEMPLATE for the command line tool and the daemon.
#
# The release workflow (.github/workflows/release.yml) fills in the version and
# sha256 placeholders with scripts/render-homebrew.sh and pushes the result to
# ketanmujumdar/homebrew-tap as Formula/secbox.rb. Do not edit the copy in the
# tap by hand. See docs/RELEASING.md.
class Secbox < Formula
  desc "Run a project and its AI agent in a disposable microVM (early build)"
  homepage "https://github.com/ketanmujumdar/secbox"
  url "https://github.com/ketanmujumdar/secbox/releases/download/v@VERSION@/secbox-@VERSION@-darwin-aarch64.tar.gz"
  version "@VERSION@"
  sha256 "@SHA256@"
  license "FSL-1.1-ALv2"

  # The engine boots VMs with Hypervisor.framework, on Apple Silicon only.
  depends_on arch: :arm64
  depends_on macos: :ventura
  depends_on :macos

  # The engine, pinned to the version secbox was tested against
  # (engine::TESTED_VERSION in crates/engine/src/msb.rs). This is deliberately
  # NOT `depends_on "superradcompany/tap/microsandbox"`, whose formula moves to
  # each new engine release on its own. Bump this only together with
  # TESTED_VERSION, after the checks in docs/NEEDS_A_VM.md pass on it.
  resource "engine" do
    url "https://github.com/superradcompany/microsandbox/releases/download/v0.7.13/microsandbox-darwin-aarch64.tar.gz"
    sha256 "5eb53ca1d6beb36caa64d19602284b9ce37872eff340a8db0542ab91b5808c96"
  end

  def install
    # Signed with Developer ID and notarized by the release workflow.
    bin.install "secbox", "secboxd"

    # msb and its libkrunfw stay together and untouched: msb finds the library
    # beside itself through its @executable_path rpath, and editing the binary
    # would break the signature that carries its hypervisor entitlement. It is
    # not linked into bin, so it never shadows another msb on PATH.
    engine = libexec/"engine"
    resource("engine").stage do
      engine.install "msb", "libkrunfw.5.dylib"
    end
    engine.install_symlink engine/"libkrunfw.5.dylib" => "libkrunfw.dylib"
  end

  # `brew services start secbox` keeps the daemon running at login. The CLI,
  # the terminal interface and the desktop app all talk to it over its socket
  # in ~/.secbox; there is no network port.
  service do
    run [opt_bin/"secboxd"]
    environment_variables SECBOX_ENGINE_BIN: opt_libexec/"engine/msb"
    keep_alive true
    log_path var/"log/secboxd.log"
    error_log_path var/"log/secboxd.log"
  end

  def caveats
    <<~EOS
      secbox is an early build. Do not rely on it to contain untrusted code.
      See https://github.com/ketanmujumdar/secbox#status

      Start the daemon now and at every login:
        brew services start secbox

      `secbox doctor` checks the engine too. To point it at the one installed
      here, add this to your shell profile:
        export SECBOX_ENGINE_BIN="#{opt_libexec}/engine/msb"

      State is kept in ~/.secbox (owner-only). `brew uninstall` leaves it and
      the keychain alone. To remove them, first run `secbox secrets rm NAME` in
      each project, then:
        secbox uninstall --purge
    EOS
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/secbox --version")
    assert_match "0.7.13", shell_output("#{libexec}/engine/msb --version")

    ENV["SECBOX_HOME"] = (testpath/"home").to_s
    (testpath/"home").mkpath

    # No daemon is running in the test sandbox, and `up` must say so plainly.
    assert_match "daemon is not running", shell_output("#{bin}/secbox up 2>&1", 1)

    (testpath/"project/compose.yaml").write <<~YAML
      services:
        web:
          image: nginx:1.27
          privileged: true
    YAML
    assert_match "Critical (1)", shell_output("#{bin}/secbox check #{testpath}/project")
    shell_output("#{bin}/secbox check --strict #{testpath}/project", 3)

    assert_match "Removed", shell_output("#{bin}/secbox uninstall --purge --yes")
  end
end
