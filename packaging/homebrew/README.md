# Homebrew packaging

`sunoto.rb` is the formula for the tap `shakdwipeea/sunoto`. The tap is a
GitHub repository named `homebrew-sunoto` containing `Formula/sunoto.rb`;
copy this file there on every change.

## Create the tap (once)

```bash
gh repo create shakdwipeea/homebrew-sunoto --public --clone
mkdir -p homebrew-sunoto/Formula
cp packaging/homebrew/sunoto.rb homebrew-sunoto/Formula/
cd homebrew-sunoto && git add . && git commit -m "Add sunoto formula" && git push
```

## Install as a user

```bash
brew tap shakdwipeea/sunoto
brew install --HEAD sunoto     # head-only until the first tagged release
sunoto setup
```

`sunoto setup` builds `Sunoto.app`, registers it as a Login Item, starts it,
offers to download the 2.7 GB polish model, and waits until the app reports
ready. `--with-llm` and `--without-llm` skip the question.

## Test the formula locally

```bash
brew style packaging/homebrew/sunoto.rb
brew install --HEAD --build-from-source packaging/homebrew/sunoto.rb
brew test sunoto
```

Building from source compiles the daemon, the Swift overlay, and
`llama-cpp-python` with Metal; expect ten to twenty minutes on a laptop.

## Why not `brew services`

`brew services` registers a launchd agent. A launchd-launched process has
no responsible GUI process, and macOS then disables the CGEventTap the
hotkey depends on. The Login Item that `sunoto setup` registers starts
`Sunoto.app` through Launch Services instead, which keeps the tap alive.

## Local signing

Release bundles are ad-hoc signed in CI. `sunoto setup` creates a stable,
self-signed code-signing identity in the user's login keychain and re-signs
the installed bundle. Input Monitoring and Accessibility grants therefore
survive formula upgrades on that machine. A Developer ID signature is still
required for notarization and cask distribution.
