#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later

set -euo pipefail

if (( $# < 6 || $# > 7 )); then
  echo "usage: audio-soak.sh LOUDNESSD SOX PIPEWIRE WIREPLUMBER DURATION_SECONDS OUTPUT [playback|capture|lifecycle|limiter|memory]" >&2
  exit 2
fi

loudnessd=$1
sox=$2
pipewire=$3
wireplumber=$4
duration=$5
output=$6
mode=${7:-playback}
case $mode in
  playback)
    expected_active=2
    pause_application=loudnessd.soak.intermittent
    pause_domain=playback
    ;;
  capture)
    expected_active=2
    pause_application=loudnessd.soak.duplex
    pause_domain=capture
    ;;
  lifecycle)
    expected_active=1
    pause_application=loudnessd.soak.lifecycle
    pause_domain=playback
    ;;
  limiter)
    expected_active=1
    pause_application=loudnessd.soak.limiter
    pause_domain=playback
    ;;
  memory)
    expected_active=8
    pause_application=loudnessd.soak.memory.0
    pause_domain=playback
    ;;
  *)
    echo "unknown soak mode: $mode" >&2
    exit 2
    ;;
esac
sink_name="loudnessd-soak-sink"
daemon_pid=
stream_pids=()
sink_id=
sink_serial=
capture_node_id=
pipewire_pid=
wireplumber_pid=
top_pid=
declare -A baseline_controls=()
declare -A baseline_targets=()
host_runtime=${XDG_RUNTIME_DIR:?XDG_RUNTIME_DIR must be set}
test_runtime=$(mktemp -d "$host_runtime/loudnessd-soak.XXXXXX")
test_config=$test_runtime/config.toml
client_config_dir=$test_runtime/config-home/pipewire/client.conf.d
server_log=$output.pipewire-server.log
wireplumber_log=$output.wireplumber.log
daemon_log=$output.daemon.log
top_output=$output.pipewire-top.txt
summary_output=$output.summary.json
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
  for pid in "${stream_pids[@]}"; do
    kill "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
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
  if [[ -n $top_pid ]]; then
    kill "$top_pid" 2>/dev/null || true
    wait "$top_pid" 2>/dev/null || true
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

if [[ $mode == playback ]]; then
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
elif [[ $mode == capture ]]; then
  printf '%s\n' \
    '[defaults]' \
    'playback = false' \
    'capture = false' \
    '' \
    '[applications."loudnessd.soak.duplex"]' \
    'playback = true' \
    'capture = true' >"$test_config"
elif [[ $mode == lifecycle ]]; then
  printf '%s\n' \
    '[defaults]' \
    'playback = false' \
    'capture = false' \
    '' \
    '[applications."loudnessd.soak.lifecycle"]' \
    'playback = true' \
    'capture = false' >"$test_config"
elif [[ $mode == limiter ]]; then
  printf '%s\n' \
    '[defaults]' \
    'playback = false' \
    'capture = false' \
    '' \
    '[applications."loudnessd.soak.limiter"]' \
    'playback = true' \
    'capture = false' >"$test_config"
else
  printf '%s\n' \
    '[defaults]' \
    'playback = false' \
    'capture = false' >"$test_config"
  for index in {0..7}; do
    printf '\n[applications."loudnessd.soak.memory.%d"]\nplayback = true\ncapture = false\n' \
      "$index" >>"$test_config"
  done
fi

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

start_daemon() {
  "${private_env[@]}" "$loudnessd" --daemon --config "$test_config" >>"$daemon_log" 2>&1 &
  daemon_pid=$!
}

: >"$daemon_log"
start_daemon

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

wait_for_sink_removal() {
  local removed_serial=$1
  for _ in {1..50}; do
    if ! "${private_env[@]}" pw-dump | jq -e --arg name "$sink_name" --arg serial "$removed_serial" '
      any(.[]; select(
        .info.props["node.name"]? == $name and
        (.info.props["object.serial"] | tostring) == $serial
      ))
    ' >/dev/null; then
      return 0
    fi
    sleep 0.1
  done
  return 1
}

wait_for_capture_node() {
  local node=
  for _ in {1..50}; do
    node=$("${private_env[@]}" pw-dump | jq -r '
      first(.[] | select(
        .type == "PipeWire:Interface:Node" and
        .info.props["application.id"]? == "loudnessd.soak.duplex" and
        .info.props["media.class"]? == "Stream/Input/Audio"
      ) | .id) // empty
    ')
    if [[ -n $node ]]; then
      capture_node_id=$node
      return 0
    fi
    sleep 0.1
  done
  return 1
}

link_capture_monitor() {
  local channel
  for channel in 0 1; do
    "${private_env[@]}" pw-cli create-link \
      "$sink_id" "$channel" "$capture_node_id" "$channel" \
      '{ object.linger = true }' >/dev/null
  done
}

save_startup_diagnostics() {
  printf '%s\n' "${status:-}" >"$output.startup-status.json"
  "${private_env[@]}" pw-dump >"$output.startup-graph.json" 2>/dev/null || true
}

stream_control_state() {
  "${private_env[@]}" pw-cli enum-params "$1" Props
}

stream_target_state() {
  local node_id=$1
  "${private_env[@]}" pw-dump | jq -Sc --argjson node_id "$node_id" '
    first(.[] | select(.id == $node_id) | .info.props) |
    {
      node_target: .["node.target"] // null,
      target_object: .["target.object"] // null
    }
  '
}

direct_route_channel_count() {
  local stream_node_id=$1
  "${private_env[@]}" pw-dump | jq -r \
    --arg stream_node_id "$stream_node_id" \
    --arg sink_node_id "$sink_id" '
      [
        .[] | select(
          .type == "PipeWire:Interface:Link" and
          (.info.props["link.output.node"] | tostring) == $stream_node_id and
          (.info.props["link.input.node"] | tostring) == $sink_node_id
        )
      ] | length
    '
}

wait_for_lifecycle_state() {
  local enabled=$1
  local active=$2
  local managed=$3
  for _ in {1..100}; do
    status=$("${private_env[@]}" "$loudnessd" msg status-json 2>/dev/null || true)
    if jq -e \
      --argjson enabled "$enabled" \
      --argjson active "$active" \
      --argjson managed "$managed" '
        .enabled == $enabled and .active == $active and .managed == $managed
      ' >/dev/null 2>&1 <<<"$status"; then
      return 0
    fi
    if [[ -n $daemon_pid ]] && ! kill -0 "$daemon_pid" 2>/dev/null; then
      return 1
    fi
    sleep 0.1
  done
  return 1
}

wait_for_healthy_routes() {
  for _ in {1..100}; do
    status=$("${private_env[@]}" "$loudnessd" msg status-json 2>/dev/null || true)
    if jq -e --argjson expected "$expected_active" '
      .enabled and .active == $expected and .managed == $expected and .skipped == 0 and
      all(.streams[]; .route == "healthy")
    ' >/dev/null 2>&1 <<<"$status"; then
      mapfile -t fixture_ids < <(jq -r '.streams[].node_id' <<<"$status")
      return 0
    fi
    if [[ -n $daemon_pid ]] && ! kill -0 "$daemon_pid" 2>/dev/null; then
      return 1
    fi
    sleep 0.1
  done
  return 1
}

wait_for_filter_removal() {
  for _ in {1..50}; do
    if ! "${private_env[@]}" pw-dump | jq -e '
      any(.[]; select(
        .type == "PipeWire:Interface:Node" and
        .info.props["media.category"]? == "Filter" and
        .info.props["media.role"]? == "DSP" and
        (.info.props["node.name"]? // "" | startswith("loudnessd-"))
      ))
    ' >/dev/null; then
      return 0
    fi
    sleep 0.1
  done
  return 1
}

save_state_mismatch() {
  local node_id=$1
  local kind=$2
  local before=$3
  local after=$4
  printf '%s\n' "$before" >"$output.$node_id.$kind.before"
  printf '%s\n' "$after" >"$output.$node_id.$kind.after"
}

create_sink
if ! wait_for_sink; then
  echo "loudnessd soak sink did not appear" >&2
  exit 1
fi

generate_first_stream() {
  local output_file=$1
  local generated=0
  : >"$output_file"
  while (( generated < duration + 60 )); do
    {
      "$sox" -q -n -t raw -e floating-point -b 32 -L -r 48000 -c 2 - \
        synth 45 sine 220 sine 330 vol 0.035
      "$sox" -q -n -t raw -e floating-point -b 32 -L -r 48000 -c 2 - \
        synth 15 sine 220 sine 330 vol 0.003
    } >>"$output_file"
    generated=$((generated + 60))
  done
}

generate_second_stream() {
  local output_file=$1
  local generated=0
  : >"$output_file"
  while (( generated < duration + 60 )); do
    {
      "$sox" -q -n -t raw -e floating-point -b 32 -L -r 48000 -c 2 - \
        synth 30 pinknoise vol 0.12
      "$sox" -q -n -t raw -e floating-point -b 32 -L -r 48000 -c 2 - \
        synth 10 sine 550 sine 770 vol 0
      "$sox" -q -n -t raw -e floating-point -b 32 -L -r 48000 -c 2 - \
        synth 20 sine 550 sine 770 vol 0.08
    } >>"$output_file"
    generated=$((generated + 60))
  done
}

generate_limiter_stream() {
  local output_file=$1
  local generated=0
  : >"$output_file"
  while (( generated < duration + 60 )); do
    {
      "$sox" -q -n -t raw -e floating-point -b 32 -L -r 48000 -c 2 - \
        synth 0.998 sine 220 sine 330 vol 0.02
      "$sox" -q -n -t raw -e floating-point -b 32 -L -r 48000 -c 2 - \
        synth 0.002 sine 997 sine 997 vol 0.95
    } >>"$output_file"
    generated=$((generated + 1))
  done
}

if [[ $mode == playback ]]; then
  first_fixture=$test_runtime/first-stream.raw
  second_fixture=$test_runtime/second-stream.raw
  generate_first_stream "$first_fixture"
  generate_second_stream "$second_fixture"

  cat "$first_fixture" | "${private_env[@]}" pw-cat --playback --raw --target "$sink_name" \
    --rate 48000 --channels 2 --channel-map Stereo --format f32 \
    --properties='application.id=loudnessd.soak.continuous application.name=Loudnessd-Soak-Continuous' - &
  stream_pids+=("$!")

  cat "$second_fixture" | "${private_env[@]}" pw-cat --playback --raw --target "$sink_name" \
    --rate 48000 --channels 2 --channel-map Stereo --format f32 \
    --properties='application.id=loudnessd.soak.intermittent application.name=Loudnessd-Soak-Intermittent' - &
  stream_pids+=("$!")
elif [[ $mode == capture ]]; then
  second_fixture=$test_runtime/second-stream.raw
  generate_second_stream "$second_fixture"

  cat "$second_fixture" | "${private_env[@]}" pw-cat --playback --raw --target "$sink_name" \
    --rate 48000 --channels 2 --channel-map Stereo --format f32 \
    --properties='application.id=loudnessd.soak.duplex application.name=Loudnessd-Soak-Duplex' - &
  stream_pids+=("$!")

  "${private_env[@]}" pw-cat --record --raw --target 0 \
    --rate 48000 --channels 2 --channel-map Stereo --format f32 \
    --properties='application.id=loudnessd.soak.duplex application.name=Loudnessd-Soak-Duplex' \
    /dev/null &
  stream_pids+=("$!")
  if ! wait_for_capture_node; then
    save_startup_diagnostics
    echo "isolated capture recorder did not appear" >&2
    exit 1
  fi
  link_capture_monitor
elif [[ $mode == lifecycle ]]; then
  lifecycle_fixture=$test_runtime/lifecycle-stream.raw
  generate_first_stream "$lifecycle_fixture"

  cat "$lifecycle_fixture" | "${private_env[@]}" pw-cat --playback --raw --target "$sink_name" \
    --rate 48000 --channels 2 --channel-map Stereo --format f32 \
    --properties='application.id=loudnessd.soak.lifecycle application.name=Loudnessd-Soak-Lifecycle' - &
  stream_pids+=("$!")
elif [[ $mode == limiter ]]; then
  limiter_fixture=$test_runtime/limiter-stream.raw
  generate_limiter_stream "$limiter_fixture"

  cat "$limiter_fixture" | "${private_env[@]}" pw-cat --playback --raw --target "$sink_name" \
    --rate 48000 --channels 2 --channel-map Stereo --format f32 \
    --properties='application.id=loudnessd.soak.limiter application.name=Loudnessd-Soak-Limiter' - &
  stream_pids+=("$!")
else
  second_fixture=$test_runtime/second-stream.raw
  generate_second_stream "$second_fixture"

  for index in {0..7}; do
    cat "$second_fixture" | "${private_env[@]}" pw-cat --playback --raw --target "$sink_name" \
      --rate 48000 --channels 2 --channel-map Stereo --format f32 \
      --properties="application.id=loudnessd.soak.memory.$index application.name=Loudnessd-Soak-Memory-$index" - &
    stream_pids+=("$!")
  done
fi

ready=false
fixture_ids=()
for _ in {1..100}; do
  status=$("${private_env[@]}" "$loudnessd" msg status-json 2>/dev/null || true)
  if jq -e --argjson expected "$expected_active" '
    .active == $expected and .managed == $expected and .skipped == 0 and
    all(.streams[]; .route == "healthy")
  ' >/dev/null 2>&1 <<<"$status"; then
    mapfile -t fixture_ids < <(jq -r '.streams[].node_id' <<<"$status")
    ready=true
    break
  fi
  if ! kill -0 "$daemon_pid" 2>/dev/null; then
    echo "candidate daemon exited before the fixtures became active" >&2
    exit 1
  fi
  sleep 0.1
done
if [[ $ready != true ]]; then
  save_startup_diagnostics
  echo "isolated $mode soak streams did not become active" >&2
  exit 1
fi

for index in "${!fixture_ids[@]}"; do
  node_id=${fixture_ids[$index]}
  if [[ $mode == limiter ]]; then
    fixture_volume=1.0
  else
    fixture_volume=$(printf '0.%02d' "$((67 + index * 4))")
  fi
  "${private_env[@]}" wpctl set-volume "$node_id" "$fixture_volume"
  "${private_env[@]}" wpctl set-mute "$node_id" 0
done
sleep 1
for node_id in "${fixture_ids[@]}"; do
  baseline_controls[$node_id]=$(stream_control_state "$node_id")
done

if [[ $mode == lifecycle ]]; then
  lifecycle_node_id=${fixture_ids[0]}
  recovery_journal=$test_runtime/loudnessd-routes.toml
  if [[ ! -f $recovery_journal ]]; then
    save_startup_diagnostics
    echo "active lifecycle route has no recovery journal" >&2
    exit 1
  fi

  kill -KILL "$daemon_pid"
  wait "$daemon_pid" 2>/dev/null || true
  daemon_pid=
  start_daemon
  if ! wait_for_healthy_routes; then
    save_startup_diagnostics
    echo "lifecycle stream did not recover after forced daemon termination" >&2
    exit 1
  fi
  if ! grep -Fq 'restored 2 direct links after an unclean exit' "$daemon_log"; then
    echo "daemon restart did not report stereo journal recovery" >&2
    exit 1
  fi

  response=$("${private_env[@]}" "$loudnessd" msg disable)
  if [[ $response != ok ]] || ! wait_for_lifecycle_state false 0 0; then
    save_startup_diagnostics
    echo "runtime disable did not bypass the lifecycle stream" >&2
    exit 1
  fi
  if [[ $(direct_route_channel_count "$lifecycle_node_id") != 2 ]]; then
    save_startup_diagnostics
    echo "runtime disable did not restore both direct channels" >&2
    exit 1
  fi
  if [[ -e $recovery_journal ]]; then
    echo "runtime disable left a recovery journal" >&2
    exit 1
  fi

  response=$("${private_env[@]}" "$loudnessd" msg enable)
  if [[ $response != ok ]] || ! wait_for_healthy_routes; then
    save_startup_diagnostics
    echo "runtime enable did not restore normalization" >&2
    exit 1
  fi

  valid_config=$(<"$test_config")
  printf '%s\n' '[defaults' >"$test_config"
  response=$("${private_env[@]}" "$loudnessd" msg reload)
  if [[ $response != error:* ]] || ! wait_for_healthy_routes; then
    save_startup_diagnostics
    echo "invalid reload did not preserve active normalization" >&2
    exit 1
  fi
  printf '%s\n' "$valid_config" >"$test_config"
  response=$("${private_env[@]}" "$loudnessd" msg reload)
  if [[ $response != ok ]] || ! wait_for_healthy_routes; then
    save_startup_diagnostics
    echo "valid reload did not restore normalization" >&2
    exit 1
  fi
fi

previous_sink_serial=$sink_serial
"${private_env[@]}" pw-cli destroy "$sink_id"
sink_id=
sink_serial=
if ! wait_for_sink_removal "$previous_sink_serial"; then
  save_startup_diagnostics
  echo "original soak sink did not disappear" >&2
  exit 1
fi
create_sink
if ! wait_for_sink "$previous_sink_serial"; then
  save_startup_diagnostics
  echo "replacement soak sink did not appear" >&2
  exit 1
fi
if [[ $mode == capture ]]; then
  link_capture_monitor
fi

recovered=false
for _ in {1..100}; do
  status=$("${private_env[@]}" "$loudnessd" msg status-json 2>/dev/null || true)
  if jq -e --argjson expected "$expected_active" '
    .active == $expected and .managed == $expected and .skipped == 0 and
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
for node_id in "${fixture_ids[@]}"; do
  baseline_targets[$node_id]=$(stream_target_state "$node_id")
done

pause_ready=false
paused_stream_id=
paused_sequence=
for _ in {1..50}; do
  status=$("${private_env[@]}" "$loudnessd" msg status-json 2>/dev/null || true)
  paused_stream_id=$(jq -r --arg application "$pause_application" --arg domain "$pause_domain" '
    first(.streams[] | select(
      .application == $application and .domain == $domain
    ) | .node_id) // empty
  ' <<<"$status")
  if [[ -n $paused_stream_id ]]; then
    paused_sequence=$(jq -r --argjson node_id "$paused_stream_id" '
      first(.streams[] | select(
        .node_id == $node_id and .output_lufs != null
      ) | .meter_sequence) // empty
    ' <<<"$status")
  fi
  if [[ -n $paused_sequence ]]; then
    pause_ready=true
    break
  fi
  if ! kill -0 "$daemon_pid" 2>/dev/null; then
    echo "candidate daemon exited before the pause probe" >&2
    exit 1
  fi
  sleep 0.1
done
if [[ $pause_ready != true ]]; then
  save_startup_diagnostics
  echo "intermittent stream produced no post-filter reading before pause" >&2
  exit 1
fi

"${private_env[@]}" pw-cli send-command "$paused_stream_id" Pause '{}' >/dev/null
sleep 1
"${private_env[@]}" pw-cli send-command "$paused_stream_id" Start '{}' >/dev/null

resumed=false
for _ in {1..50}; do
  status=$("${private_env[@]}" "$loudnessd" msg status-json 2>/dev/null || true)
  if jq -e --argjson node_id "$paused_stream_id" --argjson sequence "$paused_sequence" '
    first(.streams[] | select(.node_id == $node_id)) as $stream |
    $stream.route == "healthy" and
    $stream.meter_sequence > $sequence and
    $stream.output_lufs != null
  ' >/dev/null 2>&1 <<<"$status"; then
    resumed=true
    break
  fi
  if ! kill -0 "$daemon_pid" 2>/dev/null; then
    echo "candidate daemon exited while resuming the paused fixture" >&2
    exit 1
  fi
  sleep 0.1
done
if [[ $resumed != true ]]; then
  save_startup_diagnostics
  echo "intermittent stream did not resume normalized processing" >&2
  exit 1
fi

node_max_error() {
  local node_id=$1
  awk -v node_id="$node_id" '
    $2 == node_id && $9 ~ /^[0-9]+$/ {
      found = 1
      if ($9 > maximum) maximum = $9
    }
    END {
      if (!found) exit 1
      print maximum + 0
    }
  ' "$top_output"
}

"${private_env[@]}" pw-top -b -n "$((duration + 2))" >"$top_output" &
top_pid=$!

monitor_status=0
"${private_env[@]}" "$loudnessd" monitor \
  --duration "$duration" \
  --interval 1000 \
  --expect-active "$expected_active" \
  --output "$output" >"$summary_output" || monitor_status=$?
cat "$summary_output"

if ! wait "$top_pid"; then
  echo "continuous PipeWire profiler failed" >&2
  exit 1
fi
top_pid=

if [[ $mode == limiter ]] && ! jq -e '
  .maximum_limiter_reduction_db > 0.1 and
  .maximum_output_true_peak_dbtp != null and
  .maximum_output_true_peak_dbtp <= -0.95
' "$summary_output" >/dev/null; then
  echo "limiter qualification did not engage or exceeded the -1 dBTP ceiling" >&2
  exit 1
fi

for node_id in "${fixture_ids[@]}"; do
  final_controls=$(stream_control_state "$node_id")
  if [[ $final_controls != "${baseline_controls[$node_id]}" ]]; then
    save_state_mismatch \
      "$node_id" controls "${baseline_controls[$node_id]}" "$final_controls"
    echo "isolated soak node $node_id changed application volume or mute controls" >&2
    exit 1
  fi

  final_target=$(stream_target_state "$node_id")
  if [[ $final_target != "${baseline_targets[$node_id]}" ]]; then
    save_state_mismatch \
      "$node_id" target "${baseline_targets[$node_id]}" "$final_target"
    echo "isolated soak node $node_id changed its persistent target" >&2
    exit 1
  fi
done

mapfile -t filter_ids < <(awk '
  $2 ~ /^[0-9]+$/ && $NF == "loudnessd" { ids[$2] = 1 }
  END { for (node_id in ids) print node_id }
' "$top_output")
if (( ${#filter_ids[@]} != expected_active )); then
  echo "isolated loudnessd filters are missing from the profiler timeline" >&2
  exit 1
fi

for node_id in "${fixture_ids[@]}" "${filter_ids[@]}"; do
  maximum_error=$(node_max_error "$node_id" || true)
  if [[ -z $maximum_error ]]; then
    echo "isolated soak node $node_id is missing from the profiler timeline" >&2
    exit 1
  fi
  if (( maximum_error != 0 )); then
    echo "isolated soak node $node_id reached $maximum_error PipeWire errors during monitoring" >&2
    exit 1
  fi
done
if grep -q '^\[E\]' "$server_log" "$wireplumber_log"; then
  echo "isolated PipeWire services logged an error" >&2
  exit 1
fi

if [[ $mode == lifecycle ]]; then
  kill -TERM "$daemon_pid"
  if ! wait "$daemon_pid"; then
    daemon_pid=
    echo "daemon did not stop cleanly after lifecycle qualification" >&2
    exit 1
  fi
  daemon_pid=
  if [[ $(direct_route_channel_count "$lifecycle_node_id") != 2 ]]; then
    echo "clean daemon stop did not preserve both direct channels" >&2
    exit 1
  fi
  if [[ -e $recovery_journal ]]; then
    echo "clean daemon stop left a recovery journal" >&2
    exit 1
  fi
  if ! wait_for_filter_removal; then
    echo "clean daemon stop left a loudnessd filter node" >&2
    exit 1
  fi
fi
exit "$monitor_status"
