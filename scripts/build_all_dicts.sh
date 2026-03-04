#!/usr/bin/env bash
#
# Download kaikki.org Wiktionary JSONL dumps and build compact lookup dictionaries.
# Output goes to backend/data/dictionaries/
#
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
DATA_DIR="$SCRIPT_DIR/../data/dictionaries"
TMP_DIR="$SCRIPT_DIR/../data/tmp_wiktionary"

mkdir -p "$DATA_DIR" "$TMP_DIR"

BASE_URL="https://kaikki.org/dictionary"

# Process each language
for pair in "es:Spanish" "fr:French" "de:German" "it:Italian" "pt:Portuguese"; do
    lang="${pair%%:*}"
    name="${pair#*:}"
    jsonl_file="$TMP_DIR/kaikki.org-dictionary-${name}.jsonl"
    output_file="$DATA_DIR/wiktionary-${lang}-lookup.json"

    if [ -f "$output_file" ]; then
        echo "=== $name ($lang): already exists, skipping ==="
        continue
    fi

    echo "=== $name ($lang) ==="

    if [ ! -f "$jsonl_file" ]; then
        url="${BASE_URL}/${name}/kaikki.org-dictionary-${name}.jsonl"
        echo "Downloading from $url ..."
        curl -L -o "$jsonl_file" "$url"
    fi

    echo "Building lookup dictionary..."
    python3 "$SCRIPT_DIR/build_wiktionary_dicts.py" "$jsonl_file" "$lang" "$output_file"
    echo ""
done

echo "=== Cleaning up temp files ==="
rm -rf "$TMP_DIR"

echo ""
echo "All dictionaries built:"
ls -lh "$DATA_DIR"/wiktionary-*-lookup.json 2>/dev/null || echo "No files generated"
