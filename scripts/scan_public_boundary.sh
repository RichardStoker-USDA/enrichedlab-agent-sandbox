#!/bin/sh
set -eu

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd -P)
repo_root=$(CDPATH='' cd -- "$script_dir/.." && pwd -P)
extra_patterns=${PUBLIC_BOUNDARY_EXTRA_PATTERNS_FILE:-}
failed=false

cd "$repo_root"

list_current_files() {
    git ls-files --cached --others --exclude-standard
}

# Arguments: label, extended pattern, then optional grep flags (-i).
report_worktree_matches() {
    label=$1
    pattern=$2
    flags=${3:-}
    matches=$(
        list_current_files | while IFS= read -r path; do
            [ -f "$path" ] || continue
            grep -I -E $flags -l -e "$pattern" -- "$path" 2>/dev/null || true
        done | sort -u
    )
    if [ -n "$matches" ]; then
        printf '%s\n%s\n' "$label" "$matches" >&2
        failed=true
    fi
}

baseline="$script_dir/public_boundary_baseline"

# Runs git grep with the given options over every reachable revision. Prints its matches; any git error fails the
# scan instead of reading as "no match".
history_grep() {
    revisions=$(git rev-list --all) || return 2
    for revision in $revisions; do
        status=0
        git grep -I "$@" "$revision" -- . || status=$?
        if [ "$status" -gt 1 ]; then
            printf 'git grep failed with status %s at %s\n' "$status" "$revision" >&2
            return 2
        fi
    done
}

# Reads revision:path lines and prints each path whose blob is not a reviewed exception in the baseline.
paths_outside_baseline() {
    sort -u | while IFS= read -r hit; do
        [ -n "$hit" ] || continue
        blob=$(git rev-parse --verify --quiet "$hit" 2>/dev/null || true)
        if [ -n "$blob" ] && [ -f "$baseline" ] \
            && awk -v blob="$blob" '$1 == blob { found = 1 } END { exit !found }' "$baseline"; then
            continue
        fi
        printf '%s\n' "${hit#*:}"
    done | sort -u
}

report_history_matches() {
    label=$1
    pattern=$2
    flags=${3:-}
    hits=$(history_grep $flags -l -E -e "$pattern") || exit 1
    matches=$(printf '%s\n' "$hits" | paths_outside_baseline)
    if [ -n "$matches" ]; then
        printf '%s\n%s\n' "$label" "$matches" >&2
        failed=true
    fi
}

generic_pattern='-----BEGIN ([A-Z ]+ )?PRIVATE KEY-----|AKIA[0-9A-Z]{16}|ASIA[0-9A-Z]{16}|ghp_[A-Za-z0-9]{30,}|github_pat_[A-Za-z0-9_]+'
report_worktree_matches 'Current tracked or untracked files contain a private-boundary marker:' "$generic_pattern"
report_history_matches 'Reachable history contains a private-boundary marker:' "$generic_pattern"

# Private process references: decision numbers from the operator's notes, dated owner citations, internal review,
# finding, release, canary, suite, plan and design-slice ids, and the names of the models that reviewed a change. Code and docs explain themselves
# instead.
process_pattern='[Dd]ecisions? [0-9]{3}([^0-9]|$)|review R[0-9]+ [A-Z]+-[0-9]|(Co''dex|Day''break|Cla''ude)( containment)? (re-)?[Rr]eview|[Rr]eview( round [0-9]+)? [(](Sol|Day''break|Co''dex)|(^|[^A-Za-z])(Sol|Day''break|Co''dex|Cla''ude),? (r[0-9]+ )?[Rr]eview|[Rr]eview [Rr][0-9]+|[Rr]eview r[0-9]+|[Rr][0-9]+ round [0-9]|[Rr]eview rounds? [0-9]+ [FR][0-9]|[Ll]ive review|VISION [0-9]|[Bb]roker r[0-9]+|[Cc]anar(y|ies) [cLl][0-9]|[Hh]arness [A-Z]([,:)]| review)|OPEN''_WORK|the owner, (January|February|March|April|May|June|July|August|September|October|November|December) [0-9]|[Rr]ound [0-9]+ review|[Rr]eview rounds? [0-9]|[Rr]eview finding [A-Z]|[Ss]hapes? v[0-9]|question s[0-9]|goal g[0-9]|program B[0-9]|(^|[^A-Za-z0-9])B[0-9][a-h]? (suite|pool|replies)|([Ww]orkspace |[(])S[0-9]+[a-z]?([),:]| rule| proposal)|(^|[^A-Za-z0-9])S[0-9]{1,2}('"'"'s| previews| adds)|[Rr]eview [A-Z],? F[0-9]|suite runs? (on|in) generations? [0-9]{2}|R[0-9][.][0-9] (lesson|finding|item)|(^|[^A-Za-z0-9])D[0-9][a-z]? (draft|slices?|adds|planner|records|call)|pre-D[0-9]|[Ii]n D[0-9][a-z]?,|by D[0-9]|generation-[0-9]{2}|[(][0-9]{3} [A-Z][0-9]+[a-z]?[)]|(ASSURE|WR|OPS)-[0-9]{2,}|predates [A-Z][0-9]([^0-9A-Za-z]|$)|(before|after|since|until) [A-Z][0-9][a-z]?([]).,;:!?]|$)|guard [(]G[0-9][)]'
report_worktree_matches 'Current tracked or untracked files contain a private process reference:' "$process_pattern"
report_history_matches 'Reachable history contains a private process reference:' "$process_pattern"

home_pattern='/'"Users/"'[^/[:space:]]+|/'"home/"'[^/[:space:]]+'
home_matches=$(
    list_current_files | while IFS= read -r path; do
        [ -f "$path" ] || continue
        grep -I -H -n -E -- "$home_pattern" "$path" 2>/dev/null || true
    done \
        | grep -E -v '/(Users|home)/(agent|example|sample|sample-user|username|yourname)(/|$|[^[:alnum:]_.-])' \
        | cut -d: -f1 | sort -u || true
)
if [ -n "$home_matches" ]; then
    printf '%s\n%s\n' 'Current tracked or untracked files contain a non-placeholder home path:' "$home_matches" >&2
    failed=true
fi
home_hits=$(history_grep -n -E -e "$home_pattern") || exit 1
home_matches=$(
    printf '%s\n' "$home_hits" \
        | grep -E -v '/(Users|home)/(agent|example|sample|sample-user|username|yourname)(/|$|[^[:alnum:]_.-])' \
        | cut -d: -f2 | sort -u || true
)
if [ -n "$home_matches" ]; then
    printf '%s\n%s\n' 'Reachable history contains a non-placeholder home path:' "$home_matches" >&2
    failed=true
fi

# Site markers live outside this public repository: PUBLIC_BOUNDARY_EXTRA_PATTERNS_FILE names a file of extended
# regular expressions, one per line, written in lowercase and matched against lowercased text. Lines starting with `#`
# and blank lines are skipped. A line `allow:<pattern>` names text a public file may hold although it contains a marker
# (a public dataset named after an institution, say); that text is removed from a line before the markers are checked.
# The awk program reads one file's text on standard input and prints `hit` when a line still holds a marker.
marker_awk='
    BEGIN {
        while ((status = getline line < markers) > 0) marker[++markers_n] = line
        if (status < 0) exit 3
        while ((status = getline line < allows) > 0) allow[++allows_n] = line
        if (status < 0) exit 3
    }
    {
        text = tolower($0)
        for (i = 1; i <= allows_n; i++) gsub(allow[i], "", text)
        for (i = 1; i <= markers_n; i++) if (text ~ marker[i]) { found = 1; exit }
    }
    END { if (found) print "hit" }'

# Prints `hit` when the text on standard input holds a marker; any awk failure stops the scan.
text_has_marker() {
    awk -v markers="$marker_file" -v allows="$allow_file" "$marker_awk" || {
        printf '%s\n' 'the private marker check failed' >&2
        exit 1
    }
}

if [ -n "$extra_patterns" ]; then
    [ -f "$extra_patterns" ] || {
        printf '%s\n' 'PUBLIC_BOUNDARY_EXTRA_PATTERNS_FILE is not a regular file' >&2
        exit 1
    }
    marker_work=$(mktemp -d "${TMPDIR:-/tmp}/boundary-markers.XXXXXX")
    trap 'rm -rf "$marker_work"' EXIT
    marker_file=$marker_work/markers
    allow_file=$marker_work/allows
    grep -v -e '^#' -e '^[[:space:]]*$' -e '^allow:' -- "$extra_patterns" >"$marker_file" || true
    grep -e '^allow:' -- "$extra_patterns" | sed 's/^allow://' >"$allow_file" || true
    [ -s "$marker_file" ] || {
        printf '%s\n' 'PUBLIC_BOUNDARY_EXTRA_PATTERNS_FILE holds no marker pattern' >&2
        exit 1
    }
    # Lowercase only, so grep's case-blind prefilter and awk's lowercased match agree; every pattern must compile.
    if grep -q -e '[A-Z]' -e '\[:upper:\]' -- "$marker_file" "$allow_file"; then
        printf '%s\n' 'PUBLIC_BOUNDARY_EXTRA_PATTERNS_FILE patterns must be lowercase' >&2
        exit 1
    fi
    for list in "$marker_file" "$allow_file"; do
        [ -s "$list" ] || continue
        status=0
        grep -E -f "$list" </dev/null >/dev/null 2>&1 || status=$?
        if [ "$status" -gt 1 ] || ! awk '{ if ("" ~ $0) n++ }' "$list" >/dev/null 2>&1; then
            printf '%s\n' 'PUBLIC_BOUNDARY_EXTRA_PATTERNS_FILE holds a pattern that does not compile' >&2
            exit 1
        fi
    done

    list_current_files >"$marker_work/files"
    : >"$marker_work/hits"
    while IFS= read -r path; do
        [ -f "$path" ] || continue
        grep -I -q -i -E -f "$marker_file" -- "$path" 2>/dev/null || continue
        hit=$(text_has_marker <"$path") || exit 1
        [ -z "$hit" ] || printf '%s\n' "$path" >>"$marker_work/hits"
    done <"$marker_work/files"
    if [ -s "$marker_work/hits" ]; then
        printf '%s\n%s\n' 'Current tracked or untracked files contain a private site marker:' "$(sort -u "$marker_work/hits")" >&2
        failed=true
    fi

    # History: each `revision:path` whose blob passes the prefilter is read whole. A revision is hexadecimal, so the
    # first colon ends it and the rest is the path, colons included.
    history_grep -l -i -E -f "$marker_file" >"$marker_work/candidates" || exit 1
    : >"$marker_work/history"
    : >"$marker_work/blobs"
    while IFS= read -r candidate; do
        [ -n "$candidate" ] || continue
        blob=$(git rev-parse --verify --quiet "$candidate") || exit 1
        if grep -q -x -F -e "$blob hit" -- "$marker_work/blobs"; then
            printf '%s\n' "$candidate" >>"$marker_work/history"
            continue
        fi
        grep -q -x -F -e "$blob clean" -- "$marker_work/blobs" && continue
        git cat-file blob "$blob" >"$marker_work/blob" || exit 1
        hit=$(text_has_marker <"$marker_work/blob") || exit 1
        if [ -n "$hit" ]; then
            printf '%s hit\n' "$blob" >>"$marker_work/blobs"
            printf '%s\n' "$candidate" >>"$marker_work/history"
        else
            printf '%s clean\n' "$blob" >>"$marker_work/blobs"
        fi
    done <"$marker_work/candidates"
    matches=$(paths_outside_baseline <"$marker_work/history")
    if [ -n "$matches" ]; then
        printf '%s\n%s\n' 'Reachable history contains a private site marker:' "$matches" >&2
        failed=true
    fi
fi

if command -v gitleaks >/dev/null 2>&1; then
    gitleaks git --redact --no-banner --log-opts=--all
    list_current_files | while IFS= read -r path; do
        [ -f "$path" ] || continue
        if grep -Iq . "$path" 2>/dev/null; then
            printf '\nsource-file: %s\n' "$path"
            awk '{ print }' "$path"
        fi
    done | gitleaks stdin --redact --no-banner
else
    printf '%s\n' 'gitleaks not installed; built-in path, key-header, and token-prefix checks ran'
fi

[ "$failed" = false ] || exit 1
printf '%s\n' 'Public boundary scan passed.'
