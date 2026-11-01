#!/usr/bin/env sh
# Turns the failures in the output of the end-to-end tests into GitHub annotations,
# which show on the commit, and through the API without access to the logs.
#
#   scripts/ci-annotate-e2e.sh e2e.log
set -eu

awk '
function flush() {
    if (name != "") {
        if (text == "") text = "failed"
        gsub(/%/, "%25", text)
        printf "::error title=%s::%s\n", name, substr(text, 1, 500)
        seen[name] = 1
    }
    name = ""; text = ""; state = 0
}
/^---- .* stdout ----$/ { flush(); name = $2; next }
/^failures:$/ { flush(); next }
name == "" {
    # Tests that failed without a report of their own.
    if ($0 ~ /^test .* \.\.\. FAILED$/) failed[$2] = 1
    next
}
state == 0 && /^Error: / { text = $0; state = 1; next }
state == 0 && / panicked at / { text = $0; state = 2; next }
# The causes under "Caused by:", indented.
state == 1 && /^    [^ ]/ { sub(/^ +/, ""); text = text " - " $0; next }
# The message of a panic, on the next line.
state == 2 && NF > 0 { text = text " " $0; state = 3; next }
END {
    flush()
    for (test in failed) if (!(test in seen)) printf "::error title=%s::failed\n", test
}
' "$1"
