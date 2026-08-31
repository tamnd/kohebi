#!/usr/bin/env bash
#
# Publish the workspace to crates.io, and be safe to run again.
#
# Two things make this more than one `cargo publish --workspace`.
#
# The first is the rate limit. crates.io lets an account publish five brand new
# crates at once and then one every ten minutes, and this workspace is fourteen
# crates, so the first release cannot go out in a single command however it is
# written. The tenth attempt is not a failure, it is the shape of the limit.
#
# The second follows from the first. A command that gets halfway and stops has
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

# The delay between attempts, which is the ten minutes the limit refills in.
# Overridable so a test can watch the loop work without waiting for it.
sleep_for=${PUBLISH_RETRY_SECONDS:-600}
attempts=${PUBLISH_ATTEMPTS:-20}

# Whether crates.io already has this exact version.
#
# A 404 is the answer for a crate that does not exist and for a version that
# does not, so both come back the same way and both mean publish it. Anything
# else, a 500 or a timeout, is not an answer, and treating it as "not there"
# would turn a bad minute at the registry into a duplicate upload attempt. So an
# unknown answer stops the run.
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

echo "publishing ${#members[@]} crates at ${version}"
stalled=0
# Not the number of crates, because the first attempt has not tried anything yet
# and would otherwise count as having made no progress.
left=-1

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

  # An attempt that publishes nothing at all is the difference between a rate
  # limit and a real failure. Once the burst is spent every attempt gets exactly
  # one crate through before the limit stops it, so no progress twice running
  # means waiting longer is not going to help and the error is worth looking at.
  if [ "${#waiting[@]}" -eq "$left" ]; then
    stalled=$((stalled + 1))
  else
    stalled=0
  fi
  if [ "$stalled" -ge 2 ]; then
    echo "two attempts in a row published nothing, so this is not the rate limit" >&2
    exit 1
  fi
  left=${#waiting[@]}

  echo "attempt ${attempt}: ${#waiting[@]} left (${waiting[*]})"
  if cargo publish --workspace --locked ${exclude[@]+"${exclude[@]}"}; then
    continue
  fi

  echo "attempt ${attempt} stopped, waiting ${sleep_for}s for the rate limit to refill"
  sleep "$sleep_for"
done

echo "gave up after ${attempts} attempts" >&2
exit 1
