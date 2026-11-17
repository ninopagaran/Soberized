# Shared by the installer and its unprivileged regression tests. The installer
# supplies fixed host paths; these parameters are not environment overrides.
ensure_uinput() {
    # Arguments: sysfs registration, device node, modules root, running kernel.
    # Sysfs registration also covers kernels with uinput built in. An existing
    # registration remains usable after an upgrade removes old module files.
    if [ ! -r "$1" ]; then
        if ! command -v modprobe >/dev/null 2>&1; then
            printf '%s\n' 'install: uinput is not registered and modprobe is unavailable; install kmod and retry.' >&2
            return 1
        fi
        if ! modprobe uinput; then
            if [ ! -d "$3/$4" ]; then
                printf '%s\n' "install: uinput is unavailable and the running kernel $4 has no module directory at $3/$4." \
                    'Reboot into the installed kernel after your kernel upgrade, then rerun the installer.' >&2
            else
                printf '%s\n' "install: could not load uinput for kernel $4; check the matching kernel modules and retry." >&2
            fi
            return 1
        fi
    fi
    if [ ! -r "$1" ] || [ ! -c "$2" ]; then
        printf '%s\n' "install: uinput requires a kernel registration and the character device $2. Run this installer on the host with /dev exposed." >&2
        return 1
    fi
    printf '%s\n' 'uinput: kernel driver and character device are available.'
}
