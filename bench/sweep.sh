#!/usr/bin/env bash
# Config sweep for the key-up latency. Prints one line per configuration.
B="C:/Users/anton/AppData/Local/Flow/target/release/bench-e2e.exe"
MODELS="C:/Users/anton/AppData/Local/Flow/models"
HOLDS=${HOLDS:-16}

run() {
  local label="$1"; shift
  local out
  out=$("$B" --holds "$HOLDS" --base-secs 4 "$@" 2>&1)
  local total inflight drain first
  total=$(echo "$out"   | grep "KEY UP"           | sed 's/.*n=//')
  inflight=$(echo "$out"| grep "in-flight"        | sed 's/.*n=//')
  drain=$(echo "$out"   | grep "stop + drain"     | sed 's/.*n=//')
  first=$(echo "$out"   | grep "first partial"    | sed 's/.*n=//')
  printf '%-38s\n' "== $label"
  printf '   total    %s\n' "$total"
  printf '   inflight %s\n' "$inflight"
  printf '   drain    %s\n' "$drain"
  printf '   first    %s\n' "$first"
}
