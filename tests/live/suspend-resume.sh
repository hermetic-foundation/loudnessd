#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later

set -euo pipefail

if (( $# < 5 )) || [[ ${4:-} != -- ]]; then
  cat >&2 <<'EOF'
usage: suspend-resume.sh LOUDNESSD OUTPUT EXPECT_ACTIVE -- COMMAND [ARG...]

The command must suspend and resume synchronously. For example:
  suspend-resume.sh loudnessd ./suspend 1 -- sudo rtcwake -m mem -s 45
EOF
  exit 2
fi

loudnessd=$1
output=$2
expect_active=$3
shift 4
suspend_command=("$@")
before_status_file=$output.before.json
after_status_file=$output.after.json
summary_file=$output.summary.json
before_graph_file=$output.before-graph.json
after_graph_file=$output.after-graph.json

if [[ ! $expect_active =~ ^[1-9][0-9]*$ ]]; then
  echo 'EXPECT_ACTIVE must be a positive integer' >&2
  exit 2
fi
if [[ ! -x $loudnessd ]]; then
  echo 'LOUDNESSD must be an executable path' >&2
  exit 2
fi
for command in jq pw-cli pw-dump; do
  if ! command -v "$command" >/dev/null; then
    printf 'required command not found: %s\n' "$command" >&2
    exit 2
  fi
done

mkdir -p "$(dirname "$output")"

read_status() {
  "$loudnessd" msg status-json
}

require_healthy_status() {
  local status=$1
  local expected=$2
  jq -e --argjson expected "$expected" '
    .enabled and .managed == $expected and .active == $expected and
    .skipped == 0 and (.streams | length) == $expected and
    all(.streams[];
      .route == "healthy" and .filter_state == "streaming" and
      .filter_error == null and .meter_sequence != null
    )
  ' >/dev/null <<<"$status"
}

stream_controls() {
  local node_id=$1
  pw-cli enum-params "$node_id" Props
}

before_status=$(read_status)
if ! require_healthy_status "$before_status" "$expect_active"; then
  echo 'pre-suspend status does not satisfy the healthy active-stream contract' >&2
  printf '%s\n' "$before_status" >"$before_status_file"
  exit 1
fi
printf '%s\n' "$before_status" >"$before_status_file"
pw-dump >"$before_graph_file"
mapfile -t node_ids < <(jq -r '.streams[].node_id' <<<"$before_status")
for node_id in "${node_ids[@]}"; do
  stream_controls "$node_id" >"$output.node-$node_id.controls.before"
done

started_at=$(date +%s)
"${suspend_command[@]}"
resumed_at=$(date +%s)

after_status=
for _ in {1..600}; do
  after_status=$(read_status 2>/dev/null || true)
  if [[ -n $after_status ]] && require_healthy_status "$after_status" "$expect_active"; then
    recovered=true
    for node_id in "${node_ids[@]}"; do
      before_sequence=$(jq -r --argjson node_id "$node_id" '
        first(.streams[] | select(.node_id == $node_id) | .meter_sequence) // -1
      ' <<<"$before_status")
      if ! jq -e --argjson node_id "$node_id" --argjson sequence "$before_sequence" '
        any(.streams[];
          .node_id == $node_id and .route == "healthy" and
          .filter_state == "streaming" and .meter_sequence > $sequence
        )
      ' >/dev/null <<<"$after_status"; then
        recovered=false
        break
      fi
    done
    if $recovered; then
      break
    fi
  fi
  sleep 0.1
done

if [[ -z $after_status ]] || ! require_healthy_status "$after_status" "$expect_active"; then
  echo 'post-resume status did not recover within 60 seconds' >&2
  [[ -n $after_status ]] && printf '%s\n' "$after_status" >"$after_status_file"
  exit 1
fi

for node_id in "${node_ids[@]}"; do
  before_sequence=$(jq -r --argjson node_id "$node_id" '
    first(.streams[] | select(.node_id == $node_id) | .meter_sequence) // -1
  ' <<<"$before_status")
  if ! jq -e --argjson node_id "$node_id" --argjson sequence "$before_sequence" '
    any(.streams[];
      .node_id == $node_id and .route == "healthy" and
      .filter_state == "streaming" and .meter_sequence > $sequence
    )
  ' >/dev/null <<<"$after_status"; then
    printf 'stream node %s did not recover with an advancing meter sequence\n' "$node_id" >&2
    exit 1
  fi
  stream_controls "$node_id" >"$output.node-$node_id.controls.after"
  if ! cmp -s \
    "$output.node-$node_id.controls.before" \
    "$output.node-$node_id.controls.after"; then
    printf 'stream controls changed across suspend for node %s\n' "$node_id" >&2
    exit 1
  fi
done

before_pid=$(jq -r '.process.pid' <<<"$before_status")
before_start=$(jq -r '.process.start_time_ticks' <<<"$before_status")
after_pid=$(jq -r '.process.pid' <<<"$after_status")
after_start=$(jq -r '.process.start_time_ticks' <<<"$after_status")
if [[ $before_pid != "$after_pid" || $before_start != "$after_start" ]]; then
  echo 'loudnessd restarted across suspend/resume' >&2
  exit 1
fi

printf '%s\n' "$after_status" >"$after_status_file"
pw-dump >"$after_graph_file"

expected_filter_ids=$(jq -c '[.streams[].filter_node_id] | sort' <<<"$after_status")
actual_filter_ids=$(jq -c '[
  .[] | select(
    .type == "PipeWire:Interface:Node" and
    .info.props["media.category"]? == "Filter" and
    .info.props["media.role"]? == "DSP" and
    (
      ((.info.props["node.name"]? // "") | tostring) as $name |
      ($name == "loudnessd" or ($name | startswith("loudnessd-")))
    )
  ) | .id
] | sort' "$after_graph_file")
if [[ $expected_filter_ids != "$actual_filter_ids" ]]; then
  echo 'post-resume graph contains missing or stale loudnessd filter nodes' >&2
  exit 1
fi

jq -n \
  --argjson started_at "$started_at" \
  --argjson resumed_at "$resumed_at" \
  --argjson before "$before_status" \
  --argjson after "$after_status" '
    {
      result: "pass",
      suspend_started_unix: $started_at,
      resumed_unix: $resumed_at,
      suspended_wall_seconds: ($resumed_at - $started_at),
      daemon_restarted: false,
      controls_unchanged: true,
      graph_clean: true,
      before: $before,
      after: $after
    }
  ' >"$summary_file"

printf 'suspend/resume qualification passed; summary: %s\n' "$summary_file"
