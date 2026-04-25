# Homebrew Cask for Vryn
# This file is automatically updated by CI on release.
# To install: brew tap contember/vryn-ws && brew install --cask vryn-ws

cask "vryn-ws" do
  arch arm: "arm64", intel: "x64"

  version "0.4.3"
  sha256 arm:   "bb5140ada0b9649a3a8f3e867f13dcb48ea92df9e4ce5e6ea697365f5fbef33f",
         intel: "fd3a3f295c647835ca004d6d7dfb31990ffc682b878b523fa6268bab7263a4f5"

  url "https://github.com/contember/vryn-ws/releases/download/v#{version}/vryn-ws-macos-#{arch}.zip"
  name "Vryn"
  desc "Terminal multiplexer for managing multiple terminal sessions"
  homepage "https://github.com/contember/vryn-ws"

  livecheck do
    url :url
    strategy :github_latest
  end

  app "Vryn.app"

  zap trash: [
    "~/.config/vryn-ws",
    "~/Library/Application Support/vryn-ws",
    "~/Library/Caches/vryn-ws",
    "~/Library/Preferences/dev.vryn.ws.plist",
  ]
end
