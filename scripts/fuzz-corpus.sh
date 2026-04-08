#!/usr/bin/env bash
# Fuzz corpus sync for zenextras crates against R2.
#
# Layout:  s3://${R2_BUCKET}/fuzz/<crate>/<target>/<sha1>
# Local:   <crate>/fuzz/corpus/<target>/<sha1>
#
# Required env vars:
#   R2_ACCOUNT_ID
#   R2_BUCKET
#   R2_ACCESS_KEY_ID
#   R2_SECRET_ACCESS_KEY
set -euo pipefail

CRATES=(zentiff zensvg zenpdf zenjp2)

usage() {
    cat <<EOF
Usage: $(basename "$0") <command> [crate] [target]

Commands:
  push   [crate] [target]   Upload local corpus → R2 (one-way; R2 wins on conflict by mtime)
  pull   [crate] [target]   Download R2 → local (one-way)
  merge  [crate] [target]   Pull then push — combines both sides
  list                      List all corpora in R2 with file counts
  cmin   <crate> <target>   Run cargo fuzz cmin then push
  diff   [crate] [target]   Show what differs between local and R2
  help                      This message

If [crate] omitted: operates on all crates ($(IFS=, ; echo "${CRATES[*]}"))
If [target] omitted: operates on all targets in that crate
EOF
}

require_env() {
    : "${R2_ACCOUNT_ID:?R2_ACCOUNT_ID is not set}"
    : "${R2_BUCKET:?R2_BUCKET is not set}"
    : "${R2_ACCESS_KEY_ID:?R2_ACCESS_KEY_ID is not set}"
    : "${R2_SECRET_ACCESS_KEY:?R2_SECRET_ACCESS_KEY is not set}"
    export AWS_ACCESS_KEY_ID="$R2_ACCESS_KEY_ID"
    export AWS_SECRET_ACCESS_KEY="$R2_SECRET_ACCESS_KEY"
    export AWS_DEFAULT_REGION=auto
    ENDPOINT="https://${R2_ACCOUNT_ID}.r2.cloudflarestorage.com"
}

aws_s3() {
    aws s3 "$@" --endpoint-url "$ENDPOINT"
}

# Repo root = parent of scripts/
REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"

iter_targets() {
    local crate="$1"
    local target="${2:-}"
    if [ -n "$target" ]; then
        echo "$target"
        return
    fi
    local dir="${REPO_ROOT}/${crate}/fuzz/fuzz_targets"
    if [ ! -d "$dir" ]; then
        return
    fi
    for f in "$dir"/*.rs; do
        [ -f "$f" ] || continue
        basename "$f" .rs
    done
}

iter_crates() {
    local crate="${1:-}"
    if [ -n "$crate" ]; then
        echo "$crate"
        return
    fi
    for c in "${CRATES[@]}"; do
        if [ -d "${REPO_ROOT}/${c}/fuzz/fuzz_targets" ]; then
            echo "$c"
        fi
    done
}

cmd_push() {
    local crate="${1:-}" target="${2:-}"
    require_env
    while read -r c; do
        while read -r t; do
            local local_dir="${REPO_ROOT}/${c}/fuzz/corpus/${t}"
            local r2_path="s3://${R2_BUCKET}/fuzz/${c}/${t}/"
            if [ ! -d "$local_dir" ] || [ -z "$(ls -A "$local_dir" 2>/dev/null)" ]; then
                printf "  %-30s SKIP (empty)\n" "${c}/${t}"
                continue
            fi
            local count=$(ls "$local_dir" | wc -l)
            printf "  %-30s pushing %d files...\n" "${c}/${t}" "$count"
            aws_s3 sync "$local_dir/" "$r2_path" --no-progress --only-show-errors
        done < <(iter_targets "$c" "$target")
    done < <(iter_crates "$crate")
}

cmd_pull() {
    local crate="${1:-}" target="${2:-}"
    require_env
    while read -r c; do
        while read -r t; do
            local local_dir="${REPO_ROOT}/${c}/fuzz/corpus/${t}"
            local r2_path="s3://${R2_BUCKET}/fuzz/${c}/${t}/"
            mkdir -p "$local_dir"
            printf "  %-30s pulling...\n" "${c}/${t}"
            aws_s3 sync "$r2_path" "$local_dir/" --no-progress --only-show-errors
            local count=$(ls "$local_dir" | wc -l)
            printf "  %-30s now has %d files\n" "${c}/${t}" "$count"
        done < <(iter_targets "$c" "$target")
    done < <(iter_crates "$crate")
}

cmd_merge() {
    cmd_pull "$@"
    cmd_push "$@"
}

cmd_list() {
    require_env
    while read -r c; do
        while read -r t; do
            local r2_path="s3://${R2_BUCKET}/fuzz/${c}/${t}/"
            local count
            count=$(aws_s3 ls --recursive "$r2_path" 2>/dev/null | wc -l)
            local size_kb
            size_kb=$(aws_s3 ls --recursive --summarize "$r2_path" 2>/dev/null | grep 'Total Size' | awk '{print int($3/1024)}')
            printf "  %-30s %5d files  %6d KB\n" "${c}/${t}" "$count" "${size_kb:-0}"
        done < <(iter_targets "$c" "")
    done < <(iter_crates "")
}

cmd_cmin() {
    local crate="${1:?cmin requires <crate> <target>}"
    local target="${2:?cmin requires <crate> <target>}"
    require_env
    cd "${REPO_ROOT}/${crate}"
    local before
    before=$(ls "fuzz/corpus/${target}" 2>/dev/null | wc -l)
    echo "Before cmin: $before files"
    cargo +nightly fuzz cmin "$target"
    local after
    after=$(ls "fuzz/corpus/${target}" 2>/dev/null | wc -l)
    echo "After cmin: $after files"
    cmd_push "$crate" "$target"
}

cmd_diff() {
    local crate="${1:-}" target="${2:-}"
    require_env
    while read -r c; do
        while read -r t; do
            local local_dir="${REPO_ROOT}/${c}/fuzz/corpus/${t}"
            local r2_path="s3://${R2_BUCKET}/fuzz/${c}/${t}/"
            local local_count=0
            [ -d "$local_dir" ] && local_count=$(ls "$local_dir" 2>/dev/null | wc -l)
            local r2_count
            r2_count=$(aws_s3 ls --recursive "$r2_path" 2>/dev/null | wc -l)
            local sym="="
            [ "$local_count" -lt "$r2_count" ] && sym="<"
            [ "$local_count" -gt "$r2_count" ] && sym=">"
            printf "  %-30s local=%5d %s R2=%5d\n" "${c}/${t}" "$local_count" "$sym" "$r2_count"
        done < <(iter_targets "$c" "$target")
    done < <(iter_crates "$crate")
}

main() {
    local cmd="${1:-help}"
    shift || true
    case "$cmd" in
        push)  cmd_push  "$@" ;;
        pull)  cmd_pull  "$@" ;;
        merge) cmd_merge "$@" ;;
        list)  cmd_list ;;
        cmin)  cmd_cmin  "$@" ;;
        diff)  cmd_diff  "$@" ;;
        help|--help|-h) usage ;;
        *) echo "Unknown command: $cmd" >&2; usage; exit 1 ;;
    esac
}

main "$@"
