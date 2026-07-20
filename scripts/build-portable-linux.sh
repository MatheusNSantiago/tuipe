#!/usr/bin/env bash
set -euo pipefail

raiz="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
destino="$raiz/dist"
alvo="$raiz/target/portable-linux"
cargo_home="$raiz/target/portable-cargo-home"
imagem="rust:1.88-bullseye"

mkdir -p "$destino" "$alvo" "$cargo_home"

docker run --rm \
  --user "$(id -u):$(id -g)" \
  --env CARGO_HOME=/cargo-home \
  --env CARGO_TARGET_DIR=/alvo \
  --volume "$raiz:/fonte" \
  --volume "$alvo:/alvo" \
  --volume "$cargo_home:/cargo-home" \
  --workdir /fonte \
  "$imagem" \
  cargo build --locked --release

install -m 0755 "$alvo/release/tuipe" "$destino/tuipe-x86_64-linux"
sha256sum "$destino/tuipe-x86_64-linux" > "$destino/tuipe-x86_64-linux.sha256"

echo "artefato: $destino/tuipe-x86_64-linux"
echo "checksum: $destino/tuipe-x86_64-linux.sha256"
