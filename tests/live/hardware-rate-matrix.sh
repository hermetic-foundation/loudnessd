#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later

set -euo pipefail

if (( $# < 7 || $# > 9 )); then
  cat >&2 <<'EOF'
usage: hardware-rate-matrix.sh LOUDNESSD SOX OUTPUT PLAYBACK_TARGET PLAYBACK_PROC CAPTURE_TARGET CAPTURE_PROC [PLAYBACK_RATES] [CAPTURE_RATES]

RATE lists are comma-separated. Defaults:
  playback: 44100,48000,88200,96000,176400,192000
  capture:  44100,48000,88200,96000
EOF
  exit 2
fi

loudnessd=$1
sox=$2
output=$3
playback_target=$4
playback_proc=$5
capture_target=$6
capture_proc=$7
playback_rates=${8:-44100,48000,88200,96000,176400,192000}
capture_rates=${9:-44100,48000,88200,96000}
playback_application=loudnessd.hardware-rate.playback
capture_application=loudnessd.hardware-rate.capture
stream_pid=
initial_force_rate=
raw_files=()
baseline_managed_ids=()
results_file=$output.observations.jsonl
summary_file=$output.summary.json

metadata_value() {
  local key=$1
  pw-metadata -n settings 0 2>/dev/null |
    sed -n "s/.*key:'$key' value:'\([^']*\)'.*/\1/p" |
    tail -n 1
}

stop_stream() {
  if [[ -n $stream_pid ]]; then
    kill "$stream_pid" 2>/dev/null || true
    wait "$stream_pid" 2>/dev/null || true
    stream_pid=
  fi
}

# shellcheck disable=SC2329 # Invoked indirectly by the traps below.
cleanup() {
  trap - EXIT INT TERM
  stop_stream
  "$loudnessd" msg reset "$playback_application" >/dev/null 2>&1 || true
  "$loudnessd" msg reset "$capture_application" >/dev/null 2>&1 || true
  if [[ -n $initial_force_rate ]]; then
    pw-metadata -n settings 0 clock.force-rate "$initial_force_rate" >/dev/null 2>&1 || true
  fi
  if (( ${#raw_files[@]} > 0 )); then
    rm -f "${raw_files[@]}"
  fi
}
trap cleanup EXIT INT TERM

wait_for_graph_rate() {
  local expected=$1
  local current=
  for _ in {1..100}; do
    current=$(metadata_value clock.rate)
    if [[ $current == "$expected" ]]; then
      return 0
    fi
    sleep 0.1
  done
  printf 'graph clock did not reach %s Hz; last value was %s\n' "$expected" "$current" >&2
  return 1
}

stream_status() {
  local application=$1
  local domain=$2
  "$loudnessd" msg status-json | jq -c \
    --arg application "$application" --arg domain "$domain" '
      first(.streams[] | select(
        .application == $application and .domain == $domain
      )) // empty
    '
}

wait_for_stream() {
  local application=$1
  local domain=$2
  local status=
  for _ in {1..150}; do
    status=$(stream_status "$application" "$domain")
    if [[ -n $status ]] && jq -e '
      .route == "healthy" and .filter_state == "streaming" and
      .meter_sequence != null and .meter_sequence > 0
    ' >/dev/null <<<"$status"; then
      printf '%s\n' "$status"
      return 0
    fi
    sleep 0.1
  done
  printf 'no healthy %s route appeared for %s\n' "$domain" "$application" >&2
  return 1
}

wait_for_sequence_advance() {
  local application=$1
  local domain=$2
  local initial_sequence=$3
  local status=
  for _ in {1..100}; do
    status=$(stream_status "$application" "$domain")
    if [[ -n $status ]] && jq -e \
      --argjson initial "$initial_sequence" '
        .route == "healthy" and .filter_state == "streaming" and
        .meter_sequence > $initial
      ' >/dev/null <<<"$status"; then
      printf '%s\n' "$status"
      return 0
    fi
    sleep 0.1
  done
  printf 'meter sequence stalled for %s %s\n' "$application" "$domain" >&2
  return 1
}

physical_rate() {
  local proc_file=$1
  awk '
    /Momentary freq =/ {
      value = $4
      printf "%.0f\n", value
      exit
    }
  ' "$proc_file"
}

wait_for_physical_rate() {
  local proc_file=$1
  local expected=$2
  local current=
  for _ in {1..100}; do
    current=$(physical_rate "$proc_file")
    if [[ $current == "$expected" ]]; then
      printf '%s\n' "$current"
      return 0
    fi
    sleep 0.1
  done
  printf 'physical clock in %s did not reach %s Hz; last value was %s\n' \
    "$proc_file" "$expected" "$current" >&2
  return 1
}

stream_controls() {
  local node_id=$1
  pw-cli enum-params "$node_id" Props
}

wait_for_baseline_recovery() {
  local node_id status
  for _ in {1..100}; do
    status=$("$loudnessd" msg status-json)
    for node_id in "${baseline_managed_ids[@]}"; do
      if pw-dump | jq -e --argjson node_id "$node_id" '
        any(.[]; .type == "PipeWire:Interface:Node" and .id == $node_id)
      ' >/dev/null; then
        if ! jq -e --argjson node_id "$node_id" '
          any(.streams[]; .node_id == $node_id and .route == "healthy")
        ' >/dev/null <<<"$status"; then
          sleep 0.1
          continue 2
        fi
      fi
    done
    return 0
  done
  echo 'a pre-existing managed stream did not recover after rate-matrix cleanup' >&2
  return 1
}

start_playback() {
  local rate=$1
  local raw_file=$2
  "$sox" -q -n -t raw -e floating-point -b 32 -L -r "$rate" -c 2 "$raw_file" \
    synth 30 sine 440 sine 660 vol 0.005
  raw_files+=("$raw_file")
  "$sox" -q -t raw -e floating-point -b 32 -L -r "$rate" -c 2 "$raw_file" \
    -t raw -e floating-point -b 32 -L -r "$rate" -c 2 - repeat 4 |
    pw-cat --playback --raw --latency 100ms --target "$playback_target" \
      --rate "$rate" --channels 2 --channel-map Stereo --format f32 \
      --properties="application.id=$playback_application application.name=Loudnessd-Hardware-Rate-Playback" - &
  stream_pid=$!
}

start_capture() {
  local rate=$1
  local sample_count=$((rate * 120))
  pw-cat --record --raw --latency 100ms --target "$capture_target" \
    --rate "$rate" --channels 1 --channel-map Mono --format f32 \
    --sample-count "$sample_count" \
    --properties="application.id=$capture_application application.name=Loudnessd-Hardware-Rate-Capture" \
    /dev/null &
  stream_pid=$!
}

run_rate() {
  local domain=$1
  local rate=$2
  local application target_proc raw_file start end node_id before_controls after_controls hardware_rate
  application=$playback_application
  target_proc=$playback_proc
  raw_file=$output.playback-$rate.raw
  if [[ $domain == capture ]]; then
    application=$capture_application
    target_proc=$capture_proc
  fi

  pw-metadata -n settings 0 clock.force-rate "$rate" >/dev/null
  wait_for_graph_rate "$rate"

  if [[ $domain == playback ]]; then
    start_playback "$rate" "$raw_file"
  else
    start_capture "$rate"
  fi

  start=$(wait_for_stream "$application" "$domain")
  node_id=$(jq -r .node_id <<<"$start")
  before_controls=$(stream_controls "$node_id")
  hardware_rate=$(wait_for_physical_rate "$target_proc" "$rate")
  end=$(wait_for_sequence_advance "$application" "$domain" "$(jq -r .meter_sequence <<<"$start")")
  sleep 2
  after_controls=$(stream_controls "$node_id")
  if [[ $before_controls != "$after_controls" ]]; then
    printf '%s\n' "$before_controls" >"$output.$domain-$rate.controls.before"
    printf '%s\n' "$after_controls" >"$output.$domain-$rate.controls.after"
    printf 'stream controls changed during %s test at %s Hz\n' "$domain" "$rate" >&2
    return 1
  fi

  jq -cn \
    --arg domain "$domain" \
    --argjson requested_rate "$rate" \
    --argjson graph_rate "$(metadata_value clock.rate)" \
    --argjson physical_rate "$hardware_rate" \
    --argjson start "$start" \
    --argjson end "$end" '
      {
        domain: $domain,
        requested_rate: $requested_rate,
        graph_rate: $graph_rate,
        physical_rate: $physical_rate,
        controls_unchanged: true,
        start: $start,
        end: $end
      }
    ' >>"$results_file"

  stop_stream
  rm -f "$raw_file"
  for _ in {1..100}; do
    if [[ -z $(stream_status "$application" "$domain") ]]; then
      return 0
    fi
    sleep 0.1
  done
  printf '%s stream remained after its client exited at %s Hz\n' "$domain" "$rate" >&2
  return 1
}

for command in jq pw-cat pw-cli pw-dump pw-metadata sed awk; do
  if ! command -v "$command" >/dev/null; then
    printf 'required command not found: %s\n' "$command" >&2
    exit 2
  fi
done
for file in "$playback_proc" "$capture_proc"; do
  if [[ ! -r $file ]]; then
    printf 'hardware capability file is not readable: %s\n' "$file" >&2
    exit 2
  fi
done
if [[ ! -x $loudnessd || ! -x $sox ]]; then
  echo 'LOUDNESSD and SOX must be executable paths' >&2
  exit 2
fi

mkdir -p "$(dirname "$output")"
: >"$results_file"
initial_force_rate=$(metadata_value clock.force-rate)
if [[ -z $initial_force_rate ]]; then
  echo 'PipeWire settings metadata does not expose clock.force-rate' >&2
  exit 1
fi
mapfile -t baseline_managed_ids < <("$loudnessd" msg status-json | jq -r '.streams[].node_id')

"$loudnessd" msg set "$playback_application" playback on >/dev/null
"$loudnessd" msg set "$capture_application" capture on >/dev/null

IFS=, read -r -a rates <<<"$playback_rates"
for rate in "${rates[@]}"; do
  run_rate playback "$rate"
done
IFS=, read -r -a rates <<<"$capture_rates"
for rate in "${rates[@]}"; do
  run_rate capture "$rate"
done

cleanup
if [[ $(metadata_value clock.force-rate) != "$initial_force_rate" ]]; then
  echo 'PipeWire forced rate was not restored' >&2
  exit 1
fi
for domain_application in \
  "playback:$playback_application" \
  "capture:$capture_application"; do
  domain=${domain_application%%:*}
  application=${domain_application#*:}
  if [[ -n $(stream_status "$application" "$domain") ]]; then
    printf 'managed %s route remained after cleanup for %s\n' "$domain" "$application" >&2
    exit 1
  fi
done
wait_for_baseline_recovery

jq -s \
  --arg initial_force_rate "$initial_force_rate" \
  --arg playback_target "$playback_target" \
  --arg capture_target "$capture_target" '
    {
      result: "pass",
      initial_force_rate: $initial_force_rate,
      playback_target: $playback_target,
      capture_target: $capture_target,
      observations: .
    }
  ' "$results_file" >"$summary_file"

printf 'hardware rate matrix passed; summary: %s\n' "$summary_file"
