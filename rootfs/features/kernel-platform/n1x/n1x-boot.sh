claw_n1x_no_gpu='module_blacklist=nvidia,nvidia_modeset,nvidia_drm,nvidia_uvm'

claw_n1x_present() {
    for controller in /sys/bus/acpi/devices/NVDA0200:*; do
        [ ! -d "$controller" ] || return 0
    done
    for device in /sys/bus/pci/devices/*; do
        [ -r "$device/vendor" ] && [ -r "$device/device" ] || continue
        read -r vendor <"$device/vendor"
        read -r id <"$device/device"
        [ "$vendor:$id" != "0x10de:0x2e06" ] || return 0
    done
    return 1
}
