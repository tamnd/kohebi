#!/usr/bin/env bash
#
# Publish the workspace to crates.io, and be safe to run again.
#
# Two things make this more than one `cargo publish --workspace`.
#
# The first is the rate limit. crates.io lets an account hold five brand new
# crates in its bucket and refills one every ten minutes, and this workspace is
# fourteen crates, so the first release cannot go out in a single command
# however it is written. From a full bucket that is about ninety minutes and
# from an empty one it is a little over two hours. The tenth attempt is not a
# failure, it is the shape of the limit.
#
# The second follows from the first. A command that gets partway and stops has
# to be resumable, so every attempt starts by asking crates.io what is already
# there and excluding it. That means a re-run after any failure, a rate limit or
# a network blip or a cancelled job, picks up exactly where it left off, and a
# release that was already published in full is a no-op rather than an error.
#
# Ordering is left to cargo, which reads the dependency graph and publishes in
# an order that works. This script only ever decides what to leave out.

set -euo pipefail

metadata=$(cargo metadata --format-version 1 --no-deps)

# Every crate carries the workspace version, so there is one number here. It is
# checked rather than assumed, because a crate that had drifted off the shared
# version would publish under a number nothing else in the release used and the
# mistake would only show up in somebody's dependency tree.
version=$(python3 -c '
import json, sys
packages = json.load(sys.stdin)["packages"]
versions = {package["version"] for package in packages}
if len(versions) != 1:
    sys.exit(f"the workspace is not on one version: {sorted(versions)}")
print(versions.pop())
' <<<"$metadata")

# Read into an array the long way round, because macOS ships bash 3.2 and that
# has no `mapfile`, and this script is meant to be runnable where it is written
# as well as on the runner.
members=()
while IFS= read -r name; do
  members+=("$name")
done < <(python3 -c '
import json, sys
for package in json.load(sys.stdin)["packages"]:
    print(package["name"])
' <<<"$metadata")

attempts=${PUBLISH_ATTEMPTS:-30}
# What to wait when the registry says to come back but does not say when. Ten
# minutes is the refill interval, so it is the longest that can be useful.
fallback_wait=${PUBLISH_RETRY_SECONDS:-600}

# Whether crates.io already has this exact version.
#
# A 404 is the answer for a crate that does not exist and for a version that
# does not, so both come back the same way and both mean publish it. Anything
# else, a 500 or a timeout, is not an answer, and treating it as "not there"
# would turn a bad minute at the registry into a duplicate upload attempt. So an
# unknown answer stops the run.
#
# The user agent matters. crates.io answers 403 to a request that does not name
# itself, so a default curl looks exactly like a crate that is already taken.
published() {
  local name=$1 code
  code=$(curl --silent --show-error --location --retry 3 --max-time 30 \
    --output /dev/null --write-out '%{http_code}' \
    --user-agent 'kohebi release (https://github.com/tamnd/kohebi)' \
    "https://crates.io/api/v1/crates/${name}/${version}")
  case "$code" in
    200) return 0 ;;
    404) return 1 ;;
    *)
      echo "crates.io answered ${code} for ${name} ${version}, which is neither yes nor no" >&2
      exit 1
      ;;
  esac
}

# How long to wait, given what cargo printed.
#
# A 429 from crates.io carries the time to come back at, and honouring it is the
# difference between a release that finishes and one that either hammers the
# registry or sleeps far longer than it needs to. Nothing is guessed here: no
# 429 means the failure is a real one and this prints nothing, which is what the
# caller reads as "stop".
wait_for() {
  local output=$1 when
  grep -q '429 Too Many Requests' <<<"$output" || return 0
  when=$(sed -n 's/.*Please try again after \(.*\) and see.*/\1/p' <<<"$output" | head -1)
  if [ -z "$when" ]; then
    echo "$fallback_wait"
    return 0
  fi
  # Clamped at both ends. The floor is there because the registry hands back the
  # next refill tick, which can be seconds away and can still be too early, and
  # a run that took it literally would sit in a tight loop against crates.io.
  # The ceiling is there because a time far in the future is more likely two
  # clocks disagreeing than a real wait.
  python3 -c '
import datetime, sys
from email.utils import parsedate_to_datetime

when, fallback = sys.argv[1], int(sys.argv[2])
try:
    at = parsedate_to_datetime(when)
except (TypeError, ValueError):
    print(fallback)
    raise SystemExit
now = datetime.datetime.now(datetime.timezone.utc)
print(min(max(int((at - now).total_seconds()) + 5, 30), 900))
' "$when" "$fallback_wait"
}

echo "publishing ${#members[@]} crates at ${version}"

for attempt in $(seq 1 "$attempts"); do
  exclude=()
  waiting=()
  for name in "${members[@]}"; do
    if published "$name"; then
      exclude+=(--exclude "$name")
    else
      waiting+=("$name")
    fi
  done

  if [ ${#waiting[@]} -eq 0 ]; then
    echo "every crate is on crates.io at ${version}"
    exit 0
  fi

  echo "attempt ${attempt}: ${#waiting[@]} left (${waiting[*]})"

  # Kept rather than streamed, because the reason it stopped is in there and
  # this has to read it. `tee` puts it on the log as well so a watcher sees the
  # upload happen rather than a silent gap.
  output=$(cargo publish --workspace --locked ${exclude[@]+"${exclude[@]}"} 2>&1 | tee /dev/stderr) && continue

  # A rate limit is the one failure worth waiting out. Everything else, a crate
  # name taken or a bad token or a crate that will not package, is not going to
  # come right on its own, and sleeping through it wastes an hour and then says
  # the same thing.
  sleep_for=$(wait_for "$output")
  if [ -z "$sleep_for" ]; then
    echo "this is not the rate limit, so waiting will not help" >&2
    exit 1
  fi
  echo "rate limited, waiting ${sleep_for}s"
  sleep "$sleep_for"
done

echo "gave up after ${attempts} attempts, re-run this job to carry on" >&2
exit 1
