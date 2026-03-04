#!/usr/bin/env python3
"""
Convert a kaikki.org Wiktionary JSONL dump into a compact lookup JSON file.

Usage:
    python build_wiktionary_dicts.py <input.jsonl> <language_code> <output.json>

Example:
    python build_wiktionary_dicts.py kaikki.org-dictionary-Spanish.jsonl es wiktionary-es-lookup.json

Output format matches jmdict-lookup.json:
    { "word": { "reading": "", "definitions": ["def1", "def2"] } }
"""

import json
import sys
import re


MAX_DEFS_PER_WORD = 3
MAX_GLOSS_LENGTH = 200

# Patterns that indicate an inflected form rather than a base entry
INFLECTION_PATTERNS = re.compile(
    r"^(inflection of|plural of|feminine of|masculine of|"
    r"past participle of|present participle of|"
    r"gerund of|diminutive of|augmentative of|"
    r"superlative of|comparative of|"
    r"alternative form of|alternative spelling of|"
    r"obsolete form of|archaic form of|"
    r"conjugation of|declension of|"
    r"genitive of|dative of|accusative of|"
    r"nominative of|ablative of|vocative of|"
    r"first-person|second-person|third-person|"
    r"preterite of|imperfect of|subjunctive of|"
    r"imperative of|conditional of|future of)",
    re.IGNORECASE,
)


def is_valid_word(word: str) -> bool:
    """Only keep lowercase alphabetic words (with accented chars), reasonable length."""
    if not word or len(word) < 2 or len(word) > 40:
        return False
    # Must start with lowercase letter
    if not word[0].islower():
        return False
    # Allow letters (including accented), hyphens, and apostrophes
    return bool(re.match(r"^[a-zà-ÿ\u00C0-\u024F'\-]+$", word, re.IGNORECASE))


def is_inflection_gloss(text: str) -> bool:
    """Check if a gloss describes an inflected form rather than a real definition."""
    return bool(INFLECTION_PATTERNS.match(text.strip()))


def extract_glosses(senses: list) -> list[str]:
    """Extract English glosses from the senses array, skipping inflection descriptions."""
    glosses = []
    for sense in senses:
        if not isinstance(sense, dict):
            continue

        # Skip senses tagged as "form-of"
        tags = sense.get("tags", [])
        if isinstance(tags, list) and "form-of" in tags:
            continue

        for gloss_obj in sense.get("glosses", []):
            if isinstance(gloss_obj, str):
                text = gloss_obj.strip()
                if not text or len(text) > MAX_GLOSS_LENGTH:
                    continue
                if is_inflection_gloss(text):
                    continue
                glosses.append(text)
                if len(glosses) >= MAX_DEFS_PER_WORD:
                    return glosses
    return glosses


def process_jsonl(input_path: str, lang_code: str, output_path: str):
    lookup = {}
    processed = 0
    kept = 0
    skipped_inflections = 0

    with open(input_path, "r", encoding="utf-8") as f:
        for line in f:
            processed += 1
            if processed % 100000 == 0:
                print(
                    f"  Processed {processed} entries, kept {kept}, "
                    f"skipped {skipped_inflections} inflections...",
                    flush=True,
                )

            line = line.strip()
            if not line:
                continue

            try:
                entry = json.loads(line)
            except json.JSONDecodeError:
                continue

            word = entry.get("word", "").strip().lower()
            if not is_valid_word(word):
                continue

            senses = entry.get("senses", [])
            glosses = extract_glosses(senses)
            if not glosses:
                skipped_inflections += 1
                continue

            # Keep the entry with the most definitions
            if word in lookup:
                if len(lookup[word]["definitions"]) >= len(glosses):
                    continue

            lookup[word] = {
                "reading": "",
                "definitions": glosses,
            }
            kept += 1

    print(
        f"  Total: {processed} entries processed, {kept} words kept, "
        f"{skipped_inflections} inflections skipped"
    )

    with open(output_path, "w", encoding="utf-8") as f:
        json.dump(lookup, f, ensure_ascii=False, separators=(",", ":"))

    import os

    size_mb = os.path.getsize(output_path) / (1024 * 1024)
    print(f"  Output: {output_path} ({size_mb:.1f} MB)")


def main():
    if len(sys.argv) != 4:
        print(f"Usage: {sys.argv[0]} <input.jsonl> <lang_code> <output.json>")
        sys.exit(1)

    input_path = sys.argv[1]
    lang_code = sys.argv[2]
    output_path = sys.argv[3]

    print(f"Processing {lang_code} dictionary from {input_path}...")
    process_jsonl(input_path, lang_code, output_path)
    print("Done!")


if __name__ == "__main__":
    main()
