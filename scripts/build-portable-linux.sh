#!/usr/bin/env bash
set -euo pipefail

raiz="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
destino="$raiz/dist"
alvo="$raiz/target/portable-linux"
cargo_home="$raiz/target/portable-cargo-home"
imagem="rust:1.88-bullseye"
versao="$(awk -F '"' '/^version = "/ { print $2; exit }' "$raiz/Cargo.toml")"
pacote="tuipe-$versao-x86_64-linux"

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
(cd "$destino" && sha256sum tuipe-x86_64-linux) > "$destino/tuipe-x86_64-linux.sha256"
tar -czf "$destino/$pacote.tar.gz" \
  --transform "s,^,$pacote/," \
  -C "$destino" tuipe-x86_64-linux \
  -C "$raiz" LICENSE NOTICE README.md CHANGELOG.md assets/manifest.json
(cd "$destino" && sha256sum "$pacote.tar.gz") > "$destino/$pacote.tar.gz.sha256"

echo "artefato: $destino/tuipe-x86_64-linux"
echo "checksum: $destino/tuipe-x86_64-linux.sha256"
echo "pacote: $destino/$pacote.tar.gz"
echo "checksum do pacote: $destino/$pacote.tar.gz.sha256"
