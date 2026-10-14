#!/usr/bin/env bash
set -eu

cap=144000000
previous=''
sysfs_root=${PF_GPU_SYSFS_ROOT:-/sys}
gpu=$sysfs_root/class/devfreq/1c40000.gpu
ready=false
sensor_valid=true
function stop_guard() {
    local status=$?
    if test -w "$gpu/max_freq" && printf '144000000\n' > "$gpu/max_freq"; then
        printf '<4>GPU thermal guard stopped (exit %s); fallback ceiling: 144 MHz\n' "$status" >&2
    else
        printf '<3>GPU thermal guard stopped (exit %s); cannot set fallback ceiling\n' "$status" >&2
    fi
    return "$status"
}
trap stop_guard EXIT
trap 'exit 0' HUP INT TERM
while true; do
    temperature=''
    for zone in "$sysfs_root"/class/thermal/thermal_zone*; do
        test -f "$zone/type" || continue
        read -r name < "$zone/type" || continue
        if test "$name" = cpu-thermal; then
            read -r temperature < "$zone/temp" || temperature=''
            break
        fi
    done
    if [[ "$temperature" =~ ^-?[0-9]+$ ]] && (( temperature >= -40000 && temperature <= 125000 )); then
        if ! $sensor_valid; then
            printf 'GPU temperature readings recovered\n'
            test -z "${NOTIFY_SOCKET:-}" || systemd-notify --status='GPU temperature readings recovered'
        fi
        sensor_valid=true
        if (( temperature >= 95000 || (cap == 144000000 && temperature >= 93000) )); then
            cap=144000000
        elif (( temperature >= 85000 || (cap == 240000000 && temperature >= 83000) )); then
            cap=240000000
        else
            cap=384000000
        fi
    else
        if $sensor_valid; then
            printf '<4>GPU temperature unavailable or invalid; fallback ceiling: 144 MHz\n' >&2
            test -z "${NOTIFY_SOCKET:-}" || systemd-notify --status='GPU temperature unavailable; fallback ceiling: 144 MHz'
        fi
        sensor_valid=false
        cap=144000000
    fi
    if test -w "$gpu/max_freq"; then
        read -r current < "$gpu/max_freq"
        if test "$current" != "$cap"; then
            printf '%s\n' "$cap" > "$gpu/max_freq"
        fi
        read -r current < "$gpu/max_freq"
        test "$current" = "$cap"
        if ! $ready; then
            test -z "${NOTIFY_SOCKET:-}" || systemd-notify --ready
            ready=true
        fi
        if test "$previous" != "$cap"; then
            printf 'GPU thermal ceiling: %s Hz; temperature: %s mC\n' "$cap" "${temperature:-unavailable}"
            previous=$cap
        fi
    elif $ready; then
        echo 'GPU frequency control disappeared' >&2
        exit 1
    fi
    sleep 0.5
done
