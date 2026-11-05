# Homebrew formula TEMPLATE. Not published and not usable yet: there is no
# release, tap or repository, so the url and sha256 below are placeholders.
# See docs/INSTALL.md for how this would be used.
class Secbox < Formula
  desc "Run a project and its AI agent in a disposable microVM (early build)"
  homepage "https://example.invalid/PLACEHOLDER-no-homepage-yet"
  url "https://example.invalid/PLACEHOLDER/secbox-0.1.0.tar.gz"
  sha256 "0000000000000000000000000000000000000000000000000000000000000000"
  license "Apache-2.0"

  depends_on "rust" => :build

  def install
    system "cargo", "install", *std_cargo_args(path: "crates/cli")
    # The daemon is installed only if the source tree has one.
    system "cargo", "install", *std_cargo_args(path: "crates/daemon") if File.exist?("crates/daemon/Cargo.toml")
  end

  def caveats
    <<~EOS
      This is an early build. It is not protecting anything yet: there is no
      VM, network control or Git review, so do not rely on it to contain code.

      State is kept in ~/.secbox (owner-only). To see what would be removed
      and then remove it:
        secbox uninstall --purge

      brew uninstall does not remove that folder. Neither command removes
      secrets from the OS keychain; run `secbox secrets rm NAME` in each
      project first.
    EOS
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/secbox --version")
    (testpath/"home").mkpath
    ENV["SECBOX_HOME"] = (testpath/"home").to_s
    assert_match "not protecting anything", shell_output("#{bin}/secbox up 2>&1", 69)

    (testpath/"project/compose.yaml").write <<~YAML
      services:
        web:
          image: nginx:1.27
          privileged: true
    YAML
    assert_match "Critical (1)", shell_output("#{bin}/secbox check #{testpath}/project")
    shell_output("#{bin}/secbox check --strict #{testpath}/project", 3)

    assert_match "Nothing to remove", shell_output("#{bin}/secbox uninstall --purge --yes")
  end
end
