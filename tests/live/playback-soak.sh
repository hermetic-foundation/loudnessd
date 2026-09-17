#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later

set -euo pipefail

if (( $# != 6 )); then
  echo "usage: playback-soak.sh LOUDNESSD SOX PIPEWIRE WIREPLUMBER DURATION_SECONDS OUTPUT" >&2
  exit 2
fi

loudnessd=$1
sox=$2
pipewire=$3
wireplumber=$4
duration=$5
output=$6
sink_name="loudnessd-soak-sink"
daemon_pid=
first_stream_pid=
second_stream_pid=
sink_id=
sink_serial=
pipewire_pid=
wireplumber_pid=
host_runtime=${XDG_RUNTIME_DIR:?XDG_RUNTIME_DIR must be set}
test_runtime=$(mktemp -d "$host_runtime/loudnessd-soak.XXXXXX")
test_config=$test_runtime/config.toml
client_config_dir=$test_runtime/config-home/pipewire/client.conf.d
server_log=$output.pipewire-server.log
wireplumber_log=$output.wireplumber.log
top_output=$output.pipewire-top.txt
baseline_top_output=$output.pipewire-top-baseline.txt
private_env=(env
  PIPEWIRE_RUNTIME_DIR="$test_runtime"
  XDG_RUNTIME_DIR="$test_runtime"
  XDG_CONFIG_HOME="$test_runtime/config-home"
  XDG_STATE_HOME="$test_runtime/state-home"
  XDG_CACHE_HOME="$test_runtime/cache-home"
)

# shellcheck disable=SC2329 # Invoked indirectly by the traps below.
cleanup() {
  trap - EXIT INT TERM
  for pid in "$first_stream_pid" "$second_stream_pid"; do
    if [[ -n $pid ]]; then
      kill "$pid" 2>/dev/null || true
      wait "$pid" 2>/dev/null || true
    fi
  done
  if [[ -n $sink_id ]]; then
    "${private_env[@]}" pw-cli destroy "$sink_id" 2>/dev/null || true
  fi
  if [[ -n $daemon_pid ]]; then
    kill -INT "$daemon_pid" 2>/dev/null || true
    wait "$daemon_pid" 2>/dev/null || true
  fi
  if [[ -n $wireplumber_pid ]]; then
    kill "$wireplumber_pid" 2>/dev/null || true
    wait "$wireplumber_pid" 2>/dev/null || true
  fi
  if [[ -n $pipewire_pid ]]; then
    kill "$pipewire_pid" 2>/dev/null || true
    wait "$pipewire_pid" 2>/dev/null || true
  fi
  find "$test_runtime" -mindepth 1 -delete 2>/dev/null || true
  rmdir "$test_runtime" 2>/dev/null || true
}
trap cleanup EXIT INT TERM

mkdir -p "$client_config_dir"
printf '%s\n' \
  'context.properties = {' \
  '  module.rt = false' \
  '  loop.rt-prio = 0' \
  '}' >"$client_config_dir/10-no-realtime.conf"

printf '%s\n' \
  '[defaults]' \
  'playback = false' \
  'capture = false' \
  '' \
  '[applications."loudnessd.soak.continuous"]' \
  'playback = true' \
  'capture = false' \
  '' \
  '[applications."loudnessd.soak.intermittent"]' \
  'playback = true' \
  'capture = false' >"$test_config"

"${private_env[@]}" "$pipewire" >"$server_log" 2>&1 &
pipewire_pid=$!
for _ in {1..100}; do
  [[ -S $test_runtime/pipewire-0 ]] && break
  if ! kill -0 "$pipewire_pid" 2>/dev/null; then
    echo "private PipeWire daemon exited during startup" >&2
    exit 1
  fi
  sleep 0.1
done
if [[ ! -S $test_runtime/pipewire-0 ]]; then
  echo "private PipeWire socket did not appear" >&2
  exit 1
fi

"${private_env[@]}" "$wireplumber" --profile policy >"$wireplumber_log" 2>&1 &
wireplumber_pid=$!
for _ in {1..50}; do
  if "${private_env[@]}" pw-cli info 0 >/dev/null 2>&1; then
    break
  fi
  if ! kill -0 "$wireplumber_pid" 2>/dev/null; then
    echo "private WirePlumber exited during startup" >&2
    exit 1
  fi
  sleep 0.1
done

"${private_env[@]}" "$loudnessd" --daemon --config "$test_config" &
daemon_pid=$!

create_sink() {
  "${private_env[@]}" pw-cli create-node adapter "{
    factory.name = support.null-audio-sink
    node.name = $sink_name
    node.description = \"loudnessd soak sink\"
    media.class = Audio/Sink
    object.linger = true
    node.always-process = true
    node.want-driver = true
    priority.driver = 0
    audio.position = [ FL FR ]
  }" >/dev/null
}

wait_for_sink() {
  local previous_serial=${1:-}
  local sink=
  for _ in {1..50}; do
    sink=$("${private_env[@]}" pw-dump | jq -r --arg name "$sink_name" --arg previous "$previous_serial" '
      first(.[] | select(
        .info.props["node.name"]? == $name and
        (.info.props["object.serial"] | tostring) != $previous
      ) | [.id, .info.props["object.serial"]] | @tsv) // empty
    ')
    if [[ -n $sink ]]; then
      IFS=$'\t' read -r sink_id sink_serial <<<"$sink"
      return 0
    fi
    sleep 0.1
  done
  return 1
}

save_startup_diagnostics() {
  printf '%s\n' "${status:-}" >"$output.startup-status.json"
  "${private_env[@]}" pw-dump >"$output.startup-graph.json" 2>/dev/null || true
}

create_sink
if ! wait_for_sink; then
  echo "loudnessd soak sink did not appear" >&2
  exit 1
fi

generate_first_stream() {
  while true; do
    "$sox" -q -n -t raw -e floating-point -b 32 -L -r 48000 -c 2 - \
      synth 45 sine 220 sine 330 vol 0.035
    "$sox" -q -n -t raw -e floating-point -b 32 -L -r 48000 -c 2 - \
      synth 15 sine 220 sine 330 vol 0.003
  done
}

generate_second_stream() {
  while true; do
    "$sox" -q -n -t raw -e floating-point -b 32 -L -r 48000 -c 2 - \
      synth 30 pinknoise vol 0.12
    "$sox" -q -n -t raw -e floating-point -b 32 -L -r 48000 -c 2 - \
      synth 10 sine 550 sine 770 vol 0
    "$sox" -q -n -t raw -e floating-point -b 32 -L -r 48000 -c 2 - \
      synth 20 sine 550 sine 770 vol 0.08
  done
}

generate_first_stream | "${private_env[@]}" pw-cat --playback --raw --target "$sink_name" \
  --rate 48000 --channels 2 --channel-map Stereo --format f32 \
  --properties='application.id=loudnessd.soak.continuous application.name=Loudnessd-Soak-Continuous' - &
first_stream_pid=$!

generate_second_stream | "${private_env[@]}" pw-cat --playback --raw --target "$sink_name" \
  --rate 48000 --channels 2 --channel-map Stereo --format f32 \
  --properties='application.id=loudnessd.soak.intermittent application.name=Loudnessd-Soak-Intermittent' - &
second_stream_pid=$!

ready=false
fixture_ids=()
for _ in {1..100}; do
  status=$("${private_env[@]}" "$loudnessd" msg status-json 2>/dev/null || true)
  if jq -e '
    .active == 2 and .managed == 2 and .skipped == 0 and
    all(.streams[]; .route == "healthy")
  ' >/dev/null 2>&1 <<<"$status"; then
    ready=true
    break
  fi
  if ! kill -0 "$daemon_pid" 2>/dev/null; then
    echo "candidate daemon exited before both fixtures became active" >&2
    exit 1
  fi
  sleep 0.1
done
if [[ $ready != true ]]; then
  save_startup_diagnostics
  echo "both isolated soak streams did not become active" >&2
  exit 1
fi

previous_sink_serial=$sink_serial
"${private_env[@]}" pw-cli destroy "$sink_id"
sink_id=
sink_serial=
create_sink
if ! wait_for_sink "$previous_sink_serial"; then
  save_startup_diagnostics
  echo "replacement soak sink did not appear" >&2
  exit 1
fi

recovered=false
for _ in {1..100}; do
  status=$("${private_env[@]}" "$loudnessd" msg status-json 2>/dev/null || true)
  if jq -e '
    .active == 2 and .managed == 2 and .skipped == 0 and
    all(.streams[]; .route == "healthy")
  ' >/dev/null 2>&1 <<<"$status"; then
    mapfile -t fixture_ids < <(jq -r '.streams[].node_id' <<<"$status")
    recovered=true
    break
  fi
  if ! kill -0 "$daemon_pid" 2>/dev/null; then
    echo "candidate daemon exited during endpoint replacement" >&2
    exit 1
  fi
  sleep 0.1
done
if [[ $recovered != true ]]; then
  save_startup_diagnostics
  echo "streams did not recover after isolated sink replacement" >&2
  exit 1
fi

"${private_env[@]}" pw-top -b -n 3 >"$baseline_top_output"

node_error() {
  local node_id=$1
  local snapshot=$2
  awk -v node_id="$node_id" '
    $2 == node_id { found = 1; errors = $9 }
    END {
      if (!found) exit 1
      print errors
    }
  ' "$snapshot"
}

mapfile -t filter_ids < <(awk '
  $2 ~ /^[0-9]+$/ && $NF == "loudnessd" { ids[$2] = 1 }
  END { for (node_id in ids) print node_id }
' "$baseline_top_output")
if (( ${#filter_ids[@]} != 2 )); then
  echo "isolated loudnessd filters are missing after route recovery" >&2
  exit 1
fi

monitor_status=0
"${private_env[@]}" "$loudnessd" monitor \
  --duration "$duration" \
  --interval 1000 \
  --expect-active 2 \
  --output "$output" || monitor_status=$?

"${private_env[@]}" pw-top -b -n 3 >"$top_output"
for node_id in "${fixture_ids[@]}" "${filter_ids[@]}"; do
  baseline_error=$(node_error "$node_id" "$baseline_top_output" || true)
  final_error=$(node_error "$node_id" "$top_output" || true)
  if [[ -z $baseline_error || -z $final_error ]]; then
    echo "isolated soak node $node_id disappeared" >&2
    exit 1
  fi
  if (( final_error != baseline_error )); then
    echo "isolated soak node $node_id accumulated PipeWire errors during monitoring" >&2
    exit 1
  fi
done
if grep -q '^\[E\]' "$server_log" "$wireplumber_log"; then
  echo "isolated PipeWire services logged an error" >&2
  exit 1
fi
exit "$monitor_status"
