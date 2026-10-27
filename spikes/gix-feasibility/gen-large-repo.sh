#!/usr/bin/env bash
# Generate a synthetic git repository for the M1.7 large-repo benchmark.
#
# Usage: gen-large-repo.sh <path> [file-count] [dir-count]
#
# Creates `file-count` (default 50000) small files across `dir-count`
# (default 100) directories and commits them in a few batches. The result is a
# clean repository with a single branch and no upstream.
set -euo pipefail

path="${1:?usage: gen-large-repo.sh <path> [file-count] [dir-count]}"
files="${2:-50000}"
dirs="${3:-100}"
per_dir=$(( files / dirs ))

mkdir -p "$path"
cd "$path"
rm -rf .git

export GIT_CONFIG_NOSYSTEM=1
export GIT_CONFIG_GLOBAL=/dev/null
export GIT_AUTHOR_NAME=spike GIT_AUTHOR_EMAIL=spike@example.com GIT_AUTHOR_DATE=2020-01-01T00:00:00+00:00
export GIT_COMMITTER_NAME=spike GIT_COMMITTER_EMAIL=spike@example.com GIT_COMMITTER_DATE=2020-01-01T00:00:00+00:00

git init -q -b main

for (( d=0; d<dirs; d++ )); do
    sub=$(printf 'd%03d' "$d")
    mkdir -p "$sub"
    for (( i=0; i<per_dir; i++ )); do
        printf 'file %s/%d\n' "$sub" "$i" > "$sub/f$(printf '%04d' "$i").txt"
    done
    git add "$sub"
    if (( (d + 1) % 20 == 0 )); then
        git commit -qm "add $sub"
    fi
done
git commit -qm "final" 2>/dev/null || true

echo "generated $((dirs * per_dir)) files, $(git rev-list --count HEAD) commits, HEAD $(git rev-parse --short HEAD)"
