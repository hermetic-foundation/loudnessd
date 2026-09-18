#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later

set -euo pipefail

if (( $# < 6 || $# > 9 )); then
  echo "usage: audio-soak.sh LOUDNESSD SOX PIPEWIRE WIREPLUMBER DURATION_SECONDS OUTPUT [playback|capture|disconnect|lifecycle|limiter|memory] [SAMPLE_RATE] [CHANNELS]" >&2
  exit 2
fi

loudnessd=$1
sox=$2
pipewire=$3
wireplumber=$4
duration=$5
output=$6
mode=${7:-playback}
sample_rate=${8:-48000}
channels=${9:-2}
case $sample_rate in
  22050 | 32000 | 44100 | 48000 | 88200 | 96000 | 192000) ;;
  *)
    echo "unsupported sample rate: $sample_rate" >&2
    exit 2
    ;;
esac
case $channels in
  1)
    channel_map=Mono
    audio_position='[ MONO ]'
    ;;
  2)
    channel_map=Stereo
    audio_position='[ FL FR ]'
    ;;
  *)
    echo "unsupported channel count: $channels" >&2
    exit 2
    ;;
esac
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
  disconnect)
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
fixture_source_ids=()
fixture_source_names=()
fixture_transport_ids=()
sink_id=
sink_serial=
capture_node_id=
pipewire_pid=
wireplumber_pid=
top_pid=
monitor_pid=
declare -A baseline_controls=()
declare -A baseline_targets=()
host_runtime=${XDG_RUNTIME_DIR:?XDG_RUNTIME_DIR must be set}
test_runtime=$(mktemp -d "$host_runtime/loudnessd-soak.XXXXXX")
test_config=$test_runtime/config.toml
server_config_dir=$test_runtime/config-home/pipewire/pipewire.conf.d
server_log=$output.pipewire-server.log
wireplumber_log=$output.wireplumber.log
daemon_log=$output.daemon.log
top_output=$output.pipewire-top.txt
profiler_error_output=$output.profiler-errors.tsv
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
  if [[ -n $monitor_pid ]]; then
    kill "$monitor_pid" 2>/dev/null || true
    wait "$monitor_pid" 2>/dev/null || true
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
trap cleanup EXIT
trap 'exit 130' INT TERM

mkdir -p "$server_config_dir"
printf '%s\n' \
  'context.spa-libs = {' \
  '  audiotestsrc = audiotestsrc/libspa-audiotestsrc' \
  '}' >"$server_config_dir/10-audiotestsrc.conf"
if [[ $mode == playback ]]; then
  printf '%s\n' \
    'context.modules = [' \
    '  { name = libpipewire-module-loopback' \
    '    args = {' \
    "      audio.position = $audio_position" \
    '      capture.props = {' \
    '        node.name = loudnessd-soak-transport-continuous' \
    '        media.class = Stream/Input/Audio/Internal' \
    '        target.object = loudnessd-soak-source-continuous' \
    '        node.passive = true' \
    '      }' \
    '      playback.props = {' \
    '        node.name = loudnessd-soak-playback-continuous' \
    '        media.class = Stream/Output/Audio' \
    '        application.id = loudnessd.soak.continuous' \
    '        application.name = Loudnessd-Soak-Continuous' \
    "        target.object = $sink_name" \
    '        node.passive = false' \
    '      }' \
    '    }' \
    '  }' \
    '  { name = libpipewire-module-loopback' \
    '    args = {' \
    "      audio.position = $audio_position" \
    '      capture.props = {' \
    '        node.name = loudnessd-soak-transport-intermittent' \
    '        media.class = Stream/Input/Audio/Internal' \
    '        target.object = loudnessd-soak-source-intermittent' \
    '        node.passive = true' \
    '      }' \
    '      playback.props = {' \
    '        node.name = loudnessd-soak-playback-intermittent' \
    '        media.class = Stream/Output/Audio' \
    '        application.id = loudnessd.soak.intermittent' \
    '        application.name = Loudnessd-Soak-Intermittent' \
    "        target.object = $sink_name" \
    '        node.passive = false' \
    '      }' \
    '    }' \
    '  }' \
    ']' >"$server_config_dir/11-playback-loopbacks.conf"
elif [[ $mode == memory ]]; then
  {
    printf '%s\n' 'context.modules = ['
    for index in {0..7}; do
      printf '%s\n' \
        '  { name = libpipewire-module-loopback' \
        '    args = {' \
        "      audio.position = $audio_position" \
        '      capture.props = {' \
        "        node.name = loudnessd-soak-transport-memory-$index" \
        '        media.class = Stream/Input/Audio/Internal' \
        "        target.object = loudnessd-soak-source-memory-$index" \
        '        node.passive = true' \
        '      }' \
        '      playback.props = {' \
        "        node.name = loudnessd-soak-playback-memory-$index" \
        '        media.class = Stream/Output/Audio' \
        "        application.id = loudnessd.soak.memory.$index" \
        "        application.name = Loudnessd-Soak-Memory-$index" \
        "        target.object = $sink_name" \
        '        node.passive = false' \
        '      }' \
        '    }' \
        '  }'
    done
    printf '%s\n' ']'
  } >"$server_config_dir/11-memory-loopbacks.conf"
fi
printf '%s\n' \
  'context.properties = {' \
  "  default.clock.rate = $sample_rate" \
  "  default.clock.allowed-rates = [ $sample_rate ]" \
  '}' >"$server_config_dir/20-test-rate.conf"

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
elif [[ $mode == lifecycle || $mode == disconnect ]]; then
  printf '%s\n' \
    '[defaults]' \
    'playback = false' \
    'capture = false' \
    '' \
    '[applications."loudnessd.soak.lifecycle"]' \
    'playback = true' \
    'capture = false' \
    '' \
    '[applications."loudnessd.soak.exiting"]' \
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

start_wireplumber() {
  "${private_env[@]}" "$wireplumber" --profile policy >>"$wireplumber_log" 2>&1 &
  wireplumber_pid=$!
}

wait_for_wireplumber() {
  for _ in {1..50}; do
    if "${private_env[@]}" wpctl status >/dev/null 2>&1; then
      return 0
    fi
    if ! kill -0 "$wireplumber_pid" 2>/dev/null; then
      return 1
    fi
    sleep 0.1
  done
  return 1
}

: >"$wireplumber_log"
start_wireplumber
if ! wait_for_wireplumber; then
  echo "private WirePlumber did not become ready" >&2
  exit 1
fi

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
    audio.position = $audio_position
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
  for ((channel = 0; channel < channels; channel++)); do
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

wait_for_stable_healthy_routes() {
  local previous_routes=
  local stable_observations=0
  local current_routes=
  for _ in {1..150}; do
    status=$("${private_env[@]}" "$loudnessd" msg status-json 2>/dev/null || true)
    if jq -e --argjson expected "$expected_active" '
      .enabled and .active == $expected and .managed == $expected and .skipped == 0 and
      all(.streams[]; .route == "healthy")
    ' >/dev/null 2>&1 <<<"$status"; then
      current_routes=$(jq -c '[.streams[] | [.node_id, .filter_node_id]] | sort' <<<"$status")
      if [[ $current_routes == "$previous_routes" ]]; then
        stable_observations=$((stable_observations + 1))
      else
        previous_routes=$current_routes
        stable_observations=1
      fi
      if (( stable_observations >= 20 )); then
        mapfile -t fixture_ids < <(jq -r '.streams[].node_id' <<<"$status")
        return 0
      fi
    else
      previous_routes=
      stable_observations=0
    fi
    if [[ -n $daemon_pid ]] && ! kill -0 "$daemon_pid" 2>/dev/null; then
      return 1
    fi
    sleep 0.1
  done
  return 1
}

wait_for_active_count() {
  local expected=$1
  for _ in {1..100}; do
    status=$("${private_env[@]}" "$loudnessd" msg status-json 2>/dev/null || true)
    if jq -e --argjson expected "$expected" '
      .enabled and .active == $expected and .managed == $expected and .skipped == 0 and
      all(.streams[]; .route == "healthy")
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

wait_for_application_removal() {
  local application_id=$1
  for _ in {1..50}; do
    if ! "${private_env[@]}" pw-dump | jq -e --arg application_id "$application_id" '
      any(.[];
        .type == "PipeWire:Interface:Node" and
        .info.props["application.id"]? == $application_id
      )
    ' >/dev/null; then
      return 0
    fi
    sleep 0.1
  done
  return 1
}

wait_for_filter_identity_removal() {
  local filter_id=$1
  local filter_name=$2
  for _ in {1..50}; do
    if ! "${private_env[@]}" pw-dump | jq -e \
      --argjson filter_id "$filter_id" --arg filter_name "$filter_name" '
      any(.[];
        .id == $filter_id and
        .type == "PipeWire:Interface:Node" and
        .info.props["node.name"]? == $filter_name
      )
    ' >/dev/null; then
      return 0
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
        (
          ((.info.props["node.name"]? // "") | tostring) as $name |
          ($name == "loudnessd" or ($name | startswith("loudnessd-")))
        )
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
  local tones=(sine 220)
  if (( channels == 2 )); then
    tones+=(sine 330)
  fi
  {
    "$sox" -q -n -t raw -e floating-point -b 32 -L -r "$sample_rate" -c "$channels" - \
      synth 20 "${tones[@]}" vol 0.035
    "$sox" -q -n -t raw -e floating-point -b 32 -L -r "$sample_rate" -c "$channels" - \
      synth 20 "${tones[@]}" vol 0.6
    "$sox" -q -n -t raw -e floating-point -b 32 -L -r "$sample_rate" -c "$channels" - \
      synth 20 "${tones[@]}" vol 0.003
  } >"$output_file"
}

generate_second_stream() {
  local output_file=$1
  local tones=(sine 550)
  if (( channels == 2 )); then
    tones+=(sine 770)
  fi
  {
    "$sox" -q -n -t raw -e floating-point -b 32 -L -r "$sample_rate" -c "$channels" - \
      synth 20 pinknoise vol 0.12
    "$sox" -q -n -t raw -e floating-point -b 32 -L -r "$sample_rate" -c "$channels" - \
      synth 10 "${tones[@]}" vol 0
    "$sox" -q -n -t raw -e floating-point -b 32 -L -r "$sample_rate" -c "$channels" - \
      synth 15 "${tones[@]}" vol 0.08
    "$sox" -q -n -t raw -e floating-point -b 32 -L -r "$sample_rate" -c "$channels" - \
      synth 15 "${tones[@]}" vol 0.6
  } >"$output_file"
}

generate_limiter_stream() {
  local output_file=$1
  local base_tones=(sine 220)
  local spike_tones=(sine 997)
  if (( channels == 2 )); then
    base_tones+=(sine 330)
    spike_tones+=(sine 997)
  fi
  {
    "$sox" -q -n -t raw -e floating-point -b 32 -L -r "$sample_rate" -c "$channels" - \
      synth 0.998 "${base_tones[@]}" vol 0.02
    "$sox" -q -n -t raw -e floating-point -b 32 -L -r "$sample_rate" -c "$channels" - \
      synth 0.002 "${spike_tones[@]}" vol 0.95
  } >"$output_file"
}

repeat_fixture() {
  local input_file=$1
  local cycle_seconds=$2
  local total_seconds=$((duration + 60))
  local cycles=$(((total_seconds + cycle_seconds - 1) / cycle_seconds))
  local repeats=$((cycles - 1))
  "$sox" -q \
    -t raw -e floating-point -b 32 -L -r "$sample_rate" -c "$channels" "$input_file" \
    -t raw -e floating-point -b 32 -L -r "$sample_rate" -c "$channels" - \
    repeat "$repeats"
}

wait_for_named_node() {
  local node_name=$1
  local node_id=
  for _ in {1..50}; do
    node_id=$("${private_env[@]}" pw-dump | jq -r --arg name "$node_name" '
      first(.[] | select(
        .type == "PipeWire:Interface:Node" and
        .info.props["node.name"]? == $name
      ) | .id) // empty
    ')
    if [[ -n $node_id ]]; then
      printf '%s\n' "$node_id"
      return 0
    fi
    sleep 0.1
  done
  return 1
}

create_realtime_source() {
  local fixture_name=$1
  local frequency=$2
  local initial_volume=$3
  local source_name="loudnessd-soak-source-$fixture_name"
  local source_id

  "${private_env[@]}" pw-cli create-node adapter "{
    factory.name = audiotestsrc
    node.name = $source_name
    node.description = \"loudnessd soak source $fixture_name\"
    media.class = Audio/Source
    object.linger = true
    node.always-process = true
    node.param.Props = {
      live = false
      waveType = 0
      frequency = $frequency
      volume = $initial_volume
    }
    audio.position = $audio_position
  }" >/dev/null
  source_id=$(wait_for_named_node "$source_name")
  fixture_source_ids+=("$source_id")
  fixture_source_names+=("$source_name")
}

create_realtime_playback_fixture() {
  local fixture_name=$1
  local capture_name="loudnessd-soak-transport-$fixture_name"
  local capture_id

  capture_id=$(wait_for_named_node "$capture_name")
  fixture_transport_ids+=("$capture_id")
}

set_realtime_source() {
  local source_name=$1
  local wave_type=$2
  local volume=$3
  local source_id
  while true; do
    source_id=$(wait_for_named_node "$source_name" || true)
    if [[ -n $source_id ]] && "${private_env[@]}" pw-cli set-param "$source_id" Props \
      "{ live = false, waveType = $wave_type, volume = $volume }" >/dev/null 2>&1; then
      return 0
    fi
    sleep 0.1
  done
}

control_continuous_source() {
  local source_name=$1
  while true; do
    set_realtime_source "$source_name" 0 0.035
    sleep 20
    set_realtime_source "$source_name" 1 1.0
    sleep 20
    set_realtime_source "$source_name" 0 0.003
    sleep 20
  done
}

control_intermittent_source() {
  local source_name=$1
  while true; do
    set_realtime_source "$source_name" 1 0.12
    sleep 20
    set_realtime_source "$source_name" 0 0.0
    sleep 10
    set_realtime_source "$source_name" 0 0.08
    sleep 15
    set_realtime_source "$source_name" 1 1.0
    sleep 15
  done
}

destroy_realtime_sources() {
  local source_id
  local source_serial
  for source_id in "${fixture_source_ids[@]}"; do
    source_serial=$("${private_env[@]}" pw-dump | jq -r --argjson node_id "$source_id" '
      first(.[] | select(.id == $node_id) | .info.props["object.serial"]) // empty
    ')
    "${private_env[@]}" pw-cli destroy "$source_id"
    for _ in {1..50}; do
      if ! "${private_env[@]}" pw-dump | jq -e --arg serial "$source_serial" '
        any(.[]; (.info.props["object.serial"]? | tostring) == $serial)
      ' >/dev/null; then
        break
      fi
      sleep 0.1
    done
  done
  fixture_source_ids=()
  fixture_source_names=()
}

create_realtime_sources() {
  create_realtime_source continuous 220.0 0.035
  create_realtime_source intermittent 550.0 0.12
}

create_memory_realtime_sources() {
  local index
  for index in {0..7}; do
    create_realtime_source "memory-$index" "$((220 + index * 55)).0" 0.08
  done
}

suspend_realtime_fixture_graph() {
  local node_id
  for node_id in "${fixture_transport_ids[@]}" "${fixture_ids[@]}" "$sink_id"; do
    "${private_env[@]}" pw-cli send-command "$node_id" Suspend '{}' >/dev/null 2>&1 || true
  done
  sleep 1
}

start_realtime_fixture_graph() {
  local node_id
  for node_id in "${fixture_source_ids[@]}" "${fixture_transport_ids[@]}" "${fixture_ids[@]}" "$sink_id"; do
    "${private_env[@]}" pw-cli send-command "$node_id" Start '{}' >/dev/null 2>&1 || true
  done
}

if [[ $mode == playback ]]; then
  create_realtime_playback_fixture continuous
  create_realtime_playback_fixture intermittent
elif [[ $mode == capture ]]; then
  second_fixture=$test_runtime/second-stream.raw
  generate_second_stream "$second_fixture"

  repeat_fixture "$second_fixture" 60 | "${private_env[@]}" pw-cat --playback --raw --latency 500ms --target "$sink_name" \
    --rate "$sample_rate" --channels "$channels" --channel-map "$channel_map" --format f32 \
    --properties='application.id=loudnessd.soak.duplex application.name=Loudnessd-Soak-Duplex' - &
  stream_pids+=("$!")

  "${private_env[@]}" pw-cat --record --raw --latency 500ms --target 0 \
    --rate "$sample_rate" --channels "$channels" --channel-map "$channel_map" --format f32 \
    --properties='application.id=loudnessd.soak.duplex application.name=Loudnessd-Soak-Duplex' \
    /dev/null &
  stream_pids+=("$!")
  if ! wait_for_capture_node; then
    save_startup_diagnostics
    echo "isolated capture recorder did not appear" >&2
    exit 1
  fi
  link_capture_monitor
elif [[ $mode == lifecycle || $mode == disconnect ]]; then
  lifecycle_fixture=$test_runtime/lifecycle-stream.raw
  generate_first_stream "$lifecycle_fixture"

  repeat_fixture "$lifecycle_fixture" 60 | "${private_env[@]}" pw-cat --playback --raw --latency 500ms --target "$sink_name" \
    --rate "$sample_rate" --channels "$channels" --channel-map "$channel_map" --format f32 \
    --properties='application.id=loudnessd.soak.lifecycle application.name=Loudnessd-Soak-Lifecycle' - &
  stream_pids+=("$!")
elif [[ $mode == limiter ]]; then
  limiter_fixture=$test_runtime/limiter-stream.raw
  generate_limiter_stream "$limiter_fixture"

  repeat_fixture "$limiter_fixture" 1 | "${private_env[@]}" pw-cat --playback --raw --latency 500ms --target "$sink_name" \
    --rate "$sample_rate" --channels "$channels" --channel-map "$channel_map" --format f32 \
    --properties='application.id=loudnessd.soak.limiter application.name=Loudnessd-Soak-Limiter' - &
  stream_pids+=("$!")
else
  for index in {0..7}; do
    create_realtime_playback_fixture "memory-$index"
  done
  create_memory_realtime_sources
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

if [[ $mode == playback ]]; then
  suspend_realtime_fixture_graph
  create_realtime_sources
  start_realtime_fixture_graph
  control_continuous_source "${fixture_source_names[0]}" &
  stream_pids+=("$!")
  control_intermittent_source "${fixture_source_names[1]}" &
  stream_pids+=("$!")
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

if [[ $mode == disconnect ]]; then
  kill -TERM "$pipewire_pid"
  wait "$pipewire_pid" 2>/dev/null || true
  pipewire_pid=

  daemon_exited=false
  for _ in {1..100}; do
    daemon_state=$(awk '{ print $3 }' "/proc/$daemon_pid/stat" 2>/dev/null || true)
    if [[ -z $daemon_state || $daemon_state == Z ]]; then
      daemon_exited=true
      break
    fi
    sleep 0.1
  done
  if [[ $daemon_exited != true ]]; then
    kill -KILL "$daemon_pid" 2>/dev/null || true
    wait "$daemon_pid" 2>/dev/null || true
    daemon_pid=
    echo "candidate daemon did not exit after the private PipeWire server disconnected" >&2
    exit 1
  fi

  set +e
  wait "$daemon_pid"
  daemon_status=$?
  set -e
  daemon_pid=
  if (( daemon_status != 1 )); then
    echo "candidate daemon exited with status $daemon_status after PipeWire disconnected; expected 1" >&2
    exit 1
  fi
  if ! grep -Fq 'PipeWire connection failed:' "$daemon_log"; then
    echo "candidate daemon did not report the fatal PipeWire disconnect" >&2
    exit 1
  fi
  echo "private PipeWire disconnect exited cleanly"
  exit 0
fi

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
  if [[ $(direct_route_channel_count "$lifecycle_node_id") != "$channels" ]]; then
    save_startup_diagnostics
    echo "runtime disable did not restore every direct channel" >&2
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
if [[ $mode == playback || $mode == memory ]]; then
  destroy_realtime_sources
fi
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
if [[ $mode == playback ]]; then
  suspend_realtime_fixture_graph
  create_realtime_sources
  start_realtime_fixture_graph
elif [[ $mode == memory ]]; then
  suspend_realtime_fixture_graph
  create_memory_realtime_sources
  start_realtime_fixture_graph
fi

if ! wait_for_stable_healthy_routes; then
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
if ! wait_for_stable_healthy_routes; then
  save_startup_diagnostics
  echo "routes did not stabilize after the pause probe" >&2
  exit 1
fi

if [[ $mode == lifecycle ]]; then
  kill "$wireplumber_pid"
  wait "$wireplumber_pid"
  wireplumber_pid=
  start_wireplumber
  if ! wait_for_wireplumber || ! wait_for_stable_healthy_routes; then
    save_startup_diagnostics
    echo "lifecycle stream did not remain healthy after WirePlumber restart" >&2
    exit 1
  fi

  repeat_fixture "$lifecycle_fixture" 60 | "${private_env[@]}" pw-cat \
    --playback --raw --target "$sink_name" \
    --rate "$sample_rate" --channels "$channels" --channel-map "$channel_map" --format f32 \
    --properties='application.id=loudnessd.soak.exiting application.name=Loudnessd-Soak-Exiting' - &
  exiting_pid=$!
  stream_pids+=("$exiting_pid")
  if ! wait_for_active_count 2; then
    save_startup_diagnostics
    echo "exiting lifecycle stream did not become healthy" >&2
    exit 1
  fi
  exiting_node_id=$(jq -r '
    first(.streams[] | select(.application == "loudnessd.soak.exiting") | .node_id) // empty
  ' <<<"$status")
  exiting_filter_id=$(jq -r '
    first(.streams[] | select(.application == "loudnessd.soak.exiting") | .filter_node_id) // empty
  ' <<<"$status")
  exiting_filter_name="loudnessd-playback-$exiting_node_id"
  kill "$exiting_pid"
  wait "$exiting_pid" 2>/dev/null || true
  if ! wait_for_active_count 1 ||
    ! wait_for_application_removal loudnessd.soak.exiting ||
    ! wait_for_filter_identity_removal "$exiting_filter_id" "$exiting_filter_name"; then
    save_startup_diagnostics
    echo "application exit left managed state or a filter node behind" >&2
    exit 1
  fi

  repeat_fixture "$lifecycle_fixture" 60 | "${private_env[@]}" pw-cat \
    --playback --raw --target "$sink_name" \
    --rate "$sample_rate" --channels "$channels" --channel-map "$channel_map" --format f32 \
    --properties='application.id=loudnessd.soak.exiting application.name=Loudnessd-Soak-Exiting' - &
  bypass_exit_pid=$!
  stream_pids+=("$bypass_exit_pid")
  if ! wait_for_active_count 2; then
    save_startup_diagnostics
    echo "bypass-exit lifecycle stream did not become healthy" >&2
    exit 1
  fi
  disable_response=$test_runtime/disable-response
  "${private_env[@]}" "$loudnessd" msg disable >"$disable_response" &
  disable_pid=$!
  kill "$bypass_exit_pid"
  wait "$bypass_exit_pid" 2>/dev/null || true
  if ! wait "$disable_pid" || [[ $(<"$disable_response") != ok ]] ||
    ! wait_for_lifecycle_state false 0 0 ||
    ! wait_for_application_removal loudnessd.soak.exiting ||
    ! wait_for_filter_removal; then
    save_startup_diagnostics
    echo "application exit during bypass left managed state or a filter behind" >&2
    exit 1
  fi
  if [[ $(direct_route_channel_count "$lifecycle_node_id") != "$channels" ]]; then
    save_startup_diagnostics
    echo "application exit during bypass did not restore the survivor's direct route" >&2
    exit 1
  fi
  if [[ -e $recovery_journal ]]; then
    echo "application exit during bypass left a recovery journal" >&2
    exit 1
  fi
  response=$("${private_env[@]}" "$loudnessd" msg enable)
  if [[ $response != ok ]] || ! wait_for_stable_healthy_routes; then
    save_startup_diagnostics
    echo "surviving stream did not normalize after bypass-exit recovery" >&2
    exit 1
  fi
fi

node_initial_running_error() {
  local node_id=$1
  awk -v node_id="$node_id" '
    $1 == "R" && $2 == node_id && $9 ~ /^[0-9]+$/ {
      print $9
      exit
    }
  ' "$top_output"
}

node_max_running_error() {
  local node_id=$1
  awk -v node_id="$node_id" '
    $1 == "R" && $2 == node_id && $9 ~ /^[0-9]+$/ {
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
  --output "$output" >"$summary_output" &
monitor_pid=$!
set +e
wait "$monitor_pid"
monitor_status=$?
set -e
monitor_pid=

if (( monitor_status != 0 )); then
  echo "loudnessd monitor failed during the soak" >&2
  exit "$monitor_status"
fi

if [[ $mode == memory ]] && (( duration >= 3600 )); then
  one_hour_rss_bytes=
  final_warm_rss_bytes=
  while IFS= read -r rss_bytes; do
    if [[ -z $one_hour_rss_bytes ]]; then
      one_hour_rss_bytes=$rss_bytes
    fi
    final_warm_rss_bytes=$rss_bytes
  done < <(jq -r '
    select(.elapsed_milliseconds >= 3600000) |
    .status.process.rss_bytes // empty
  ' "$output")
  if [[ -z $one_hour_rss_bytes || -z $final_warm_rss_bytes ]]; then
    echo "resource qualification has no complete warm-state RSS interval" >&2
    exit 1
  fi
  post_warmup_rss_growth_bytes=$((final_warm_rss_bytes - one_hour_rss_bytes))
  jq \
    --argjson one_hour "$one_hour_rss_bytes" \
    --argjson growth "$post_warmup_rss_growth_bytes" '
      . + {
        one_hour_rss_bytes: $one_hour,
        post_warmup_rss_growth_bytes: $growth
      }
    ' "$summary_output" >"$test_runtime/summary-with-warm-rss.json"
  mv "$test_runtime/summary-with-warm-rss.json" "$summary_output"
fi

cat "$summary_output"

if ! jq -e --argjson expected "$expected_active" '
  .samples > 0 and
  .ipc_failures == 0 and
  .daemon_restarts == 0 and
  .minimum_active_streams == $expected and
  .maximum_active_streams == $expected and
  .active_stream_shortfall_observations == 0 and
  .skipped_stream_observations == 0 and
  .maximum_skipped_streams == 0 and
  .unhealthy_route_observations == 0 and
  .stalled_callback_observations == 0
' "$summary_output" >/dev/null; then
  echo "soak monitor reported a continuity, routing, or process-identity failure" >&2
  exit 1
fi

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

if [[ $mode == playback ]] && ! jq -e '
  .boost_observations > 0 and
  .cut_observations > 0 and
  .minimum_gain_db < -0.01 and
  .maximum_gain_db > 0.01 and
  .convergence_eligible_observations > 0 and
  .convergence_ratio >= 0.95
' "$summary_output" >/dev/null; then
  echo "playback qualification did not cover gain paths or meet convergence" >&2
  exit 1
fi

if [[ $mode == memory ]] && ! jq -e '
  .initial_rss_bytes != null and
  .final_rss_bytes != null and
  .peak_rss_bytes != null and
  .rss_growth_bytes != null and
  .peak_rss_bytes < 33554432 and
  .rss_growth_bytes < 2097152
' "$summary_output" >/dev/null; then
  echo "resource qualification exceeded the resident-memory bounds" >&2
  exit 1
fi

if [[ $mode == memory ]] && (( duration >= 3600 )) && ! jq -e '
  .one_hour_rss_bytes != null and
  .post_warmup_rss_growth_bytes != null and
  .post_warmup_rss_growth_bytes < 2097152
' "$summary_output" >/dev/null; then
  echo "resource qualification exceeded the warm-state RSS growth bound" >&2
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

printf 'node_id\trole\tenforced\trecovery_baseline\tmaximum\tdelta\n' >"$profiler_error_output"
check_profiler_node() {
  local node_id=$1
  local role=$2
  local enforced=$3
  local initial_error
  local maximum_error
  local error_delta
  initial_error=$(node_initial_running_error "$node_id" || true)
  maximum_error=$(node_max_running_error "$node_id" || true)
  if [[ -z $initial_error || -z $maximum_error ]]; then
    echo "isolated soak node $node_id is missing from the profiler timeline" >&2
    exit 1
  fi
  error_delta=$((maximum_error - initial_error))
  printf '%s\t%s\t%s\t%s\t%s\t%s\n' \
    "$node_id" "$role" "$enforced" "$initial_error" "$maximum_error" "$error_delta" \
    >>"$profiler_error_output"
  if [[ $enforced == true ]] && (( initial_error > 2 )); then
    echo "isolated soak node $node_id started monitoring with $initial_error PipeWire recovery errors" >&2
    exit 1
  fi
  if [[ $enforced == true ]] && (( error_delta != 0 )); then
    echo "isolated soak node $node_id accumulated $error_delta PipeWire errors during monitoring" >&2
    exit 1
  fi
}

for node_id in "${fixture_ids[@]}"; do
  check_profiler_node "$node_id" fixture true
done
if [[ $mode == playback || $mode == memory ]]; then
  for node_id in "${fixture_source_ids[@]}"; do
    check_profiler_node "$node_id" fixture-source true
  done
  for node_id in "${fixture_transport_ids[@]}"; do
    check_profiler_node "$node_id" fixture-transport true
  done
  check_profiler_node "$sink_id" fixture-sink true
fi
for node_id in "${filter_ids[@]}"; do
  check_profiler_node "$node_id" filter true
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
  if [[ $(direct_route_channel_count "$lifecycle_node_id") != "$channels" ]]; then
    echo "clean daemon stop did not preserve every direct channel" >&2
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
