#!/usr/bin/env bash
# Downloads a Moonshine streaming model into $XDG_DATA_HOME/flow/models.
#
#   ./fetch-model.sh            small-streaming-en, the balanced profile
#   ./fetch-model.sh tiny       tiny-streaming-en, the fast profile
#
# File sizes are checked against the published catalogue. A truncated download
# fails the model load with an unhelpful ONNX error hours later, so it is
# worth catching here. Already-correct files are left alone, so re-running
# this after an interrupted download only fetches what is missing or wrong.

set -euo pipefail

model="${1:-small}"
version="${2:-quantized_26_08_21}"

case "$model" in
    small|tiny|medium) ;;
    *)
        echo "usage: $0 [small|tiny|medium] [version]" >&2
        exit 2
        ;;
esac

name="$model-streaming-en"
base="https://download.moonshine.ai/model/$name/$version"
data_home="${XDG_DATA_HOME:-$HOME/.local/share}"
dest="$data_home/flow/models/$name"

# name -> expected bytes, from core/moonshine-model-file-metadata.generated.cpp
declare -A small_sizes=(
    [frontend.model.ort]=26944
    [frontend.weights.ort]=7769464
    [encoder.ort]=44148576
    [adapter.ort]=2870368
    [cross_kv.ort]=5356536
    [decoder_kv.ort]=81878600
    [streaming_config.json]=512
    [tokenizer.bin]=249974
)
declare -A tiny_sizes=(
    [frontend.model.ort]=23344
    [frontend.weights.ort]=2093464
    [encoder.ort]=7675440
    [adapter.ort]=1319664
    [cross_kv.ort]=1287544
    [decoder_kv.ort]=32583720
    [streaming_config.json]=509
    [tokenizer.bin]=249974
)
files=(frontend.model.ort frontend.weights.ort encoder.ort adapter.ort
       cross_kv.ort decoder_kv.ort streaming_config.json tokenizer.bin)

mkdir -p "$dest"
echo "Fetching $name into $dest"

expected_size() {
    local file="$1"
    case "$model" in
        small) echo "${small_sizes[$file]:-}" ;;
        tiny)  echo "${tiny_sizes[$file]:-}" ;;
        *)     echo "" ;;
    esac
}

total=0
for f in "${files[@]}"; do
    out="$dest/$f"
    want="$(expected_size "$f")"

    if [[ -n "$want" && -f "$out" ]]; then
        have="$(stat -c%s "$out" 2>/dev/null || echo 0)"
        if [[ "$have" == "$want" ]]; then
            printf '  %-24s %10d bytes  already present\n' "$f" "$have"
            total=$((total + have))
            continue
        fi
    fi

    curl -sL --fail -o "$out" "$base/$f"
    size="$(stat -c%s "$out")"
    total=$((total + size))

    if [[ -n "$want" && "$want" != "$size" ]]; then
        echo "$f is $size bytes, expected $want. Download incomplete." >&2
        exit 1
    fi
    printf '  %-24s %10d bytes\n' "$f" "$size"
done

printf 'Done. %.1f MB total.\n' "$(awk -v b="$total" 'BEGIN { print b / 1048576 }')"
echo "Set model.profile in the settings file to use it:"
echo "  ${XDG_CONFIG_HOME:-$HOME/.config}/flow/settings.toml"
