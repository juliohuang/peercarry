#!/usr/bin/env sh
set -eu
destination="${XDG_DATA_HOME:-$HOME/.local/share}/peercarry"
# Reuse the old installation location so existing Hook commands remain valid.
legacy="${XDG_DATA_HOME:-$HOME/.local/share}/sync-clip"
if test -d "$legacy"; then destination="$legacy"; fi
package=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
for name in peercarry-tray peercarry; do test -f "$package/$name"; done
test ! -L "$destination" || { echo 'Installation directory must not be a link' >&2; exit 1; }
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
cp "$destination/peercarry" "$destination/sclip"
ln -sf "$destination/sclip" "$HOME/.local/bin/sclip"
echo "Installed to $destination. Quit any running old tray before starting the new one."
echo "Run $destination/peercarry install-service to configure login startup."
