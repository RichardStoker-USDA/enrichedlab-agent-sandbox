#!/bin/sh
# Runs the guard's target-built tests inside the image they were added to (images/guard.Containerfile, target
# runtime-check), with qpdf, pdftotext and pdftoppm required.
set -u
export GUARD_REQUIRE_TOOLS=1
cd /source || exit 1
failed=0
while read -r test; do
    echo "== $test"
    "./target/release/deps/$test" || failed=1
done <tests.txt
if [ "$failed" = 0 ]; then echo "== runtime check passed"; else echo "== runtime check failed"; fi
exit "$failed"
