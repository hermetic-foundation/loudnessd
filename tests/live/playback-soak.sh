#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later

set -euo pipefail

if (( $# != 5 )); then
  echo "usage: playback-soak.sh LOUDNESSD SOX DURATION_SECONDS CONFIG OUTPUT" >&2
  exit 2
fi

loudnessd=$1
sox=$2
duration=$3
config=$4
output=$5
sink_name=loudnessd-soak-sink
daemon_pid=
first_stream_pid=
second_stream_pid=
sink_id=

cleanup() {
  trap - EXIT INT TERM
  for pid in "$first_stream_pid" "$second_stream_pid"; do
    if [[ -n $pid ]]; then
      kill "$pid" 2>/dev/null || true
      wait "$pid" 2>/dev/null || true
    fi
  done
  if [[ -n $sink_id ]]; then
    pw-cli destroy "$sink_id" 2>/dev/null || true
  fi
  if [[ -n $daemon_pid ]]; then
    kill -INT "$daemon_pid" 2>/dev/null || true
    wait "$daemon_pid" 2>/dev/null || true
  fi
  systemctl --user start loudnessd.service
}
trap cleanup EXIT INT TERM

systemctl --user stop loudnessd.service
"$loudnessd" --daemon --config "$config" &
daemon_pid=$!

pw-cli create-node adapter "{
  factory.name = support.null-audio-sink
  node.name = $sink_name
  node.description = \"loudnessd soak sink\"
  media.class = Audio/Sink
  object.linger = true
  audio.position = [ FL FR ]
}"

for _ in {1..50}; do
  sink_id=$(pw-dump | jq -r --arg name "$sink_name" '
    first(.[] | select(.info.props["node.name"]? == $name) | .id) // empty
  ')
  [[ -n $sink_id ]] && break
  sleep 0.1
done
if [[ -z $sink_id ]]; then
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

generate_first_stream | pw-cat --playback --raw --target "$sink_name" \
  --rate 48000 --channels 2 --channel-map Stereo --format f32 \
  --properties='application.id=loudnessd.soak.continuous application.name=Loudnessd-Soak-Continuous' - &
first_stream_pid=$!

generate_second_stream | pw-cat --playback --raw --target "$sink_name" \
  --rate 48000 --channels 2 --channel-map Stereo --format f32 \
  --properties='application.id=loudnessd.soak.intermittent application.name=Loudnessd-Soak-Intermittent' - &
second_stream_pid=$!

"$loudnessd" monitor --duration "$duration" --interval 1000 --output "$output"
