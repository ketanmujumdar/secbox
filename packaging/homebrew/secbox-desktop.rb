# typed: false
# frozen_string_literal: true

# Homebrew cask TEMPLATE for the desktop app.
#
# The release workflow fills in the version and sha256 placeholders with
# scripts/render-homebrew.sh and pushes the result to
# ketanmujumdar/homebrew-tap as Casks/secbox-desktop.rb. See docs/RELEASING.md.
cask "secbox-desktop" do
  version "@VERSION@"
  sha256 "@DMG_SHA256@"

  url "https://github.com/ketanmujumdar/secbox/releases/download/v#{version}/secbox-desktop-#{version}-aarch64.dmg"
  name "secbox"
  desc "Menu bar app and window for secbox sandboxes (early build)"
  homepage "https://github.com/ketanmujumdar/secbox"

  depends_on arch: :arm64
  depends_on macos: ">= :ventura"
  # The CLI, the daemon and the pinned engine. The app talks to the daemon
  # that `brew services start secbox` runs.
  depends_on formula: "ketanmujumdar/tap/secbox"

  app "secbox.app"

  # ~/.secbox is left alone on purpose: it holds project records and the
  # engine's data, and `secbox uninstall --purge` removes it with checks.
  zap trash: [
    "~/Library/Caches/dev.secbox.desktop",
    "~/Library/Saved Application State/dev.secbox.desktop.savedState",
    "~/Library/WebKit/dev.secbox.desktop",
  ]

  caveats <<~EOS
    secbox is an early build. Do not rely on it to contain untrusted code.

    Start the daemon before opening the app:
      brew services start secbox
  EOS
end
