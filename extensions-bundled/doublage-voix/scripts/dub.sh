#!/bin/sh
# Finds a python3 (an app started from Finder has a short PATH) and runs dub.py with the same arguments.
here=$(cd "$(dirname "$0")" && pwd)
for py in python3 /opt/homebrew/bin/python3 /usr/local/bin/python3 /usr/bin/python3; do
  if command -v "$py" >/dev/null 2>&1; then
    exec "$py" "$here/dub.py" "$@"
  fi
done
echo '{"log":"Doublage vocal needs python3 (not found on PATH, /opt/homebrew/bin, /usr/local/bin or /usr/bin)"}'
exit 1
