#!/usr/bin/env sh
set -eu
destination="${XDG_DATA_HOME:-$HOME/.local/share}/peercarry"
# Reuse the old installation location so existing Hook commands remain valid.
legacy="${XDG_DATA_HOME:-$HOME/.local/share}/sync-clip"
if test ! -e "$destination" && test -d "$legacy"; then destination="$legacy"; fi
package=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
for name in peercarry-tray peercarry; do test -f "$package/$name"; done
command -v pgrep >/dev/null 2>&1 || { echo 'pgrep is required to check running processes' >&2; exit 1; }
for name in peercarry-tray peercarry sync-clip-tray sclip; do
  if pgrep -u "$(id -u)" -x "$name" >/dev/null 2>&1; then
    echo 'Quit the tray and stop the PeerCarry/sync-clip user service before installing.' >&2
    exit 1
  else
    status=$?
    test "$status" -eq 1 || { echo 'Could not check running processes' >&2; exit 1; }
  fi
done
test ! -L "$destination" || { echo 'Installation directory must not be a link' >&2; exit 1; }
# Validate all paths before cp can follow a stale backup or staging symlink.
for name in peercarry-tray peercarry sclip; do
  for path in "$destination/$name" "$destination/$name.new" "$destination/$name.bak"; do
    if test -L "$path" || { test -e "$path" && test ! -f "$path"; }; then
      echo "Unsafe executable destination: $path" >&2; exit 1
    fi
  done
  if test -d "$HOME/.local/bin/$name" || { test -e "$HOME/.local/bin/$name" && test ! -L "$HOME/.local/bin/$name"; }; then
    echo "Refusing to replace an existing command: $HOME/.local/bin/$name" >&2; exit 1
  fi
done
mkdir -p "$destination" "$HOME/.local/bin"
for name in peercarry-tray peercarry; do
  test ! -L "$destination/$name" || { echo 'Executable must not be a link' >&2; exit 1; }
  if test -f "$destination/$name"; then cp -p "$destination/$name" "$destination/$name.bak"; fi
  cp "$package/$name" "$destination/$name.new"
  chmod 755 "$destination/$name.new"
  mv -f "$destination/$name.new" "$destination/$name"
  ln -sf "$destination/$name" "$HOME/.local/bin/$name"
done
test ! -L "$destination/sclip" || { echo "Compatibility executable must not be a link" >&2; exit 1; }
cp "$destination/peercarry" "$destination/sclip.new"
mv -f "$destination/sclip.new" "$destination/sclip"
ln -sf "$destination/sclip" "$HOME/.local/bin/sclip"
echo "Installed to $destination."
echo "Run \"$destination/peercarry\" install-service to configure login startup."
echo 'On Linux, startup runs the CLI daemon; do not also launch the tray.'
