#!/usr/bin/env bash
set -euo pipefail

fail() { printf '[display] brightness restoration refused: %s\n' "$*" >&2; exit 1; }
percent=${FRAME_DAY_BRIGHTNESS:-65}
[[ $percent =~ ^[0-9]{1,3}$ ]] || fail 'FRAME_DAY_BRIGHTNESS must be 1..100'
percent=$((10#$percent))
((percent >= 1 && percent <= 100)) || fail 'FRAME_DAY_BRIGHTNESS must be 1..100'

backlight_class=/sys/class/backlight
shopt -s nullglob
devices=("$backlight_class"/*)
((${#devices[@]} > 0)) || fail 'no system backlight found'
selected=
if [[ -n ${FRAME_BACKLIGHT_PATH:-} ]]; then
    requested=$(readlink -f -- "$FRAME_BACKLIGHT_PATH") || fail 'invalid backlight path'
    for device in "${devices[@]}"; do
        actual=$(readlink -f -- "$device") || continue
        [[ $actual != "$requested" ]] || selected=$actual
    done
    [[ -n $selected ]] || fail 'path is not a system backlight'
else
    ((${#devices[@]} == 1)) || fail 'multiple backlights; set FRAME_BACKLIGHT_PATH'
    selected=$(readlink -f -- "${devices[0]}") || fail 'invalid system backlight'
fi

brightness=$(readlink -f -- "$selected/brightness") || fail 'missing brightness attribute'
maximum_file=$(readlink -f -- "$selected/max_brightness") || fail 'missing maximum attribute'
[[ $brightness == "$selected/brightness" && $maximum_file == "$selected/max_brightness" ]] || fail 'attribute escapes the backlight directory'
read -r maximum < "$maximum_file" || fail 'cannot read maximum brightness'
[[ $maximum =~ ^[0-9]{1,7}$ ]] || fail 'invalid maximum brightness'
maximum=$((10#$maximum))
((maximum >= 1 && maximum <= 1000000)) || fail 'maximum brightness outside range'
value=$(((maximum * percent + 50) / 100))
((value > 0)) || value=1
printf '%s\n' "$value" > "$brightness"
printf '[display] restored brightness: %s%% (%s/%s)\n' "$percent" "$value" "$maximum" >&2
