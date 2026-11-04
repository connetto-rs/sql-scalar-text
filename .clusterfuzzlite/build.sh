#!/bin/bash
set -eu

cd "$SRC/sql-values"
cargo fuzz build -O --debug-assertions --fuzz-dir fuzz

targets=$(cargo fuzz list --fuzz-dir fuzz)
if [[ -z "$targets" ]]; then
    echo "cargo fuzz list named no target" >&2
    exit 1
fi

target_dir=fuzz/target/x86_64-unknown-linux-gnu/release
for name in $targets; do
    cp "$target_dir/$name" "$OUT/"
    cp fuzz/sql_scalars.dict "$OUT/$name.dict"
    # the runner unpacks <target>_seed_corpus.zip as the starting corpus
    zip -qj "$OUT/${name}_seed_corpus.zip" "fuzz/corpus/$name"/seed-*
done
