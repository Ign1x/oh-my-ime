#!/bin/sh
set -eu
root=$(CDPATH='' cd -- "$(dirname -- "$(readlink -f -- "$0")")/../../../.." && pwd -P)
exec "$root/AppRun" --tool @PROGRAM@ "$@"
