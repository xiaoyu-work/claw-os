#!/bin/bash
set -e
mkdir -p ~/.local/bin
cd /tmp
echo "downloading..."
curl -fsSL -o bun.zip https://github.com/oven-sh/bun/releases/latest/download/bun-linux-x64.zip
echo "extracting..."
python3 -c "import zipfile; zipfile.ZipFile('bun.zip').extractall('/tmp/bun-extract')"
find /tmp/bun-extract -name bun -type f -executable | head -3
cp /tmp/bun-extract/*/bun ~/.local/bin/
chmod +x ~/.local/bin/bun
~/.local/bin/bun --version
