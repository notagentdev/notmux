# Homebrew Cask for NotMux
# This file is automatically updated by CI on release.
# To install: brew tap notagentdev/notmux && brew install --cask notmux

cask "notmux" do
  arch arm: "arm64", intel: "x64"

  version "0.1.0"
  sha256 arm:   "0000000000000000000000000000000000000000000000000000000000000000",
         intel: "0000000000000000000000000000000000000000000000000000000000000000"

  url "https://github.com/notagentdev/notmux/releases/download/v#{version}/notmux-macos-#{arch}.zip"
  name "NotMux"
  desc "Terminal multiplexer for managing multiple terminal sessions"
  homepage "https://github.com/notagentdev/notmux"

  livecheck do
    url :url
    strategy :github_latest
  end

  app "NotMux.app"

  zap trash: [
    "~/.config/notmux",
    "~/Library/Application Support/notmux",
    "~/Library/Caches/notmux",
    "~/Library/Preferences/dev.notmux.app.plist",
  ]
end
