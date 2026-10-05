#!/usr/bin/env bash
# Experiment only: replay identical final server objects while rustc owns them.
set -euo pipefail
arguments=("$@")
output_index=-1
for ((index=0; index < ${#arguments[@]}; index++)); do
  if [[ "${arguments[index]}" == '-o' ]]; then output_index=$((index + 1)); fi
done
[[ "$output_index" -ge 0 && "$output_index" -lt "${#arguments[@]}" ]] || exit 2
output="${arguments[output_index]}"
if [[ "${output##*/}" != rsctf-* ]]; then exec cc "${arguments[@]}"; fi

: "${RSCTF_LINK_BENCH_DIR:?}"
: "${RSCTF_LINK_BENCH_LLD_DIR:?}"
[[ "$RSCTF_LINK_BENCH_DIR" = /* && "$RSCTF_LINK_BENCH_LLD_DIR" = /* ]] || exit 2
result_dir="${RSCTF_LINK_BENCH_DIR}/server-links"
# Never overwrite evidence or accidentally measure two different link inputs.
mkdir "$result_dir"
printf '%s\0' "${arguments[@]}" > "$result_dir/arguments.nul"
TIMEFORMAT='%R'
{ time cc "${arguments[@]}"; } 2> "$result_dir/original.seconds.log"

iteration=0
for mode in bfd lld bfd lld lld bfd bfd lld; do
  arguments[output_index]="$result_dir/${iteration}-${mode}.bin"
  extra=("-fuse-ld=${mode}")
  if [[ "$mode" == lld ]]; then extra=("-B${RSCTF_LINK_BENCH_LLD_DIR}" "${extra[@]}"); fi
  { time cc "${arguments[@]}" "${extra[@]}"; } 2> "$result_dir/${iteration}-${mode}.seconds.log"
  iteration=$((iteration + 1))
done
