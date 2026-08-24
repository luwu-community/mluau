#!/bin/bash
set -euo pipefail

BRANCH="${1:-master}"

echo "Cloning luwu (branch: $BRANCH)..."
git clone --depth 1 -b "$BRANCH" https://github.com/mluau/luwu.git luwu-tmp

echo "Creating luwu directory..."
rm -rf luwu
mkdir -p luwu

echo "Copying source directories..."
DIRS=(Ast CodeGen Common Bytecode Require VM Compiler Config)

for dir in "${DIRS[@]}"; do
    if [ -d "luwu-tmp/$dir" ]; then
        cp -r "luwu-tmp/$dir" "luwu/"
    else
        echo "Warning: Directory luwu-tmp/$dir does not exist."
    fi
done

echo "Cleaning up..."
rm -rf luwu-tmp

echo "Done! luwu source code has been vendored."
