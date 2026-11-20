# Shared by the ISO builders and the installed ARM boot configuration.

claw_qcom_boards() {
    local boards="${1:-}/usr/lib/modules/${2:-}/dtb/qcom/claw-boards.list"
    if [ -f "$boards" ]; then
        cat "$boards"
        return
    fi
    cat <<'BOARDS'
x1e80100-asus-vivobook-s15.dtb|asus,vivobook-s15|ASUS Vivobook S 15 (X Elite)
x1p42100-asus-vivobook-s15.dtb|asus,vivobook-s15-x1p4|ASUS Vivobook S 15 (X1P-42-100)
x1e80100-asus-zenbook-a14.dtb|asus,zenbook-a14-ux3407ra|ASUS Zenbook A14 UX3407RA
x1p42100-asus-zenbook-a14-lcd.dtb|asus,zenbook-a14-ux3407qa-lcd|ASUS Zenbook A14 UX3407QA (LCD)
x1p42100-asus-zenbook-a14.dtb|asus,zenbook-a14-ux3407qa-oled|ASUS Zenbook A14 UX3407QA (OLED)
x1e80100-dell-inspiron-14-plus-7441.dtb|dell,inspiron-14-plus-7441|Dell Inspiron 14 Plus 7441
x1e80100-dell-latitude-7455.dtb|dell,latitude-7455|Dell Latitude 7455
x1e80100-dell-xps13-9345.dtb|dell,xps13-9345|Dell XPS 13 9345
x1e80100-hp-elitebook-ultra-g1q.dtb|hp,elitebook-ultra-g1q|HP EliteBook Ultra G1q
x1e80100-hp-omnibook-x14.dtb|hp,omnibook-x14|HP OmniBook X 14-fe0
x1p42100-hp-omnibook-x14.dtb|hp,omnibook-x14-fe1|HP OmniBook X 14-fe1
x1e78100-lenovo-thinkpad-t14s.dtb|lenovo,thinkpad-t14s-lcd|Lenovo ThinkPad T14s Gen 6 (LCD)
x1e78100-lenovo-thinkpad-t14s-oled.dtb|lenovo,thinkpad-t14s-oled|Lenovo ThinkPad T14s Gen 6 (OLED)
x1p42100-lenovo-thinkbook-16.dtb|lenovo,thinkbook-16|Lenovo ThinkBook 16 Gen 7 QOY
x1e80100-lenovo-yoga-slim7x.dtb|lenovo,yoga-slim7x|Lenovo Yoga Slim 7x
x1e80100-medion-sprchrgd-14-s1.dtb|medion,sprchrgd14s1|Medion SPRCHRGD 14 S1
x1e80100-microsoft-romulus13.dtb|microsoft,romulus13|Microsoft Surface Laptop 7 (13.8 inch)
x1e80100-microsoft-romulus15.dtb|microsoft,romulus15|Microsoft Surface Laptop 7 (15 inch)
x1p64100-microsoft-denali.dtb|microsoft,denali-lcd|Microsoft Surface Pro 11 (X Plus LCD)
x1e80100-microsoft-denali-oled.dtb|microsoft,denali-oled|Microsoft Surface Pro 11 (X Elite OLED)
BOARDS
}

claw_qcom_board() {
    claw_qcom_boards '' "$2" | awk -F '|' -v "tree=$1" '$1 == tree { print; found=1 } END { exit !found }'
}

claw_qcom_tree() {
    local directory
    for directory in "$1/usr/lib/modules/$2/dtb/qcom" "$1/usr/lib/linux-image-$2/qcom"; do
        if [ -f "$directory/$3" ]; then
            printf '%s\n' "$directory/$3"
            return
        fi
    done
    return 1
}

# Keep the DSP off by default: it can reset USB-C storage, and some boards
# still need a signed image not supplied by Debian.
claw_qcom_args='acpi=off console=tty0 clk_ignore_unused pd_ignore_unused iommu.passthrough=0'
claw_qcom_no_dsp='modprobe.blacklist=qcom_q6v5_pas'

claw_qcom_board_args() {
    case "$1" in
        x1e80100-samsung-galaxy-book4-edge-14.dtb)
            echo 'qcom_aoss.prevent_aoss_sleep=1'
            ;;
        glymur-asus-zenbook-a16-ux3607oa.dtb)
            echo 'plymouth.enable=0'
            ;;
        x1e80100-lenovo-yoga-slim7x.dtb)
            echo 'initcall_blacklist=simpledrm_platform_driver_init'
            ;;
    esac
}

claw_qcom_board_sleep() {
    case "$1" in
        glymur-asus-zenbook-a16-ux3607oa.dtb)
            echo 'AllowSuspend=no'
            ;;
        x1p42100-asus-zenbook-14-ux3480qa.dtb)
            echo 'MemorySleepMode=s2idle'
            ;;
        *)
            claw_qcom_laptop8 "$1" || return 0
            echo 'MemorySleepMode=s2idle'
            ;;
    esac
    printf '%s\n' 'AllowHibernation=no' 'AllowSuspendThenHibernate=no' 'AllowHybridSleep=no'
}

claw_qcom_romulus() {
    case "$1" in
        x1e80100-microsoft-romulus13.dtb|x1e80100-microsoft-romulus13-spi.dtb|x1e80100-microsoft-romulus15.dtb) return 0 ;;
        *) return 1 ;;
    esac
}

claw_qcom_laptop8() {
    case "$1" in
        glymur-microsoft-surface-laptop8.dtb|glymur-microsoft-surface-laptop8-15.dtb) return 0 ;;
        *) return 1 ;;
    esac
}

claw_qcom_nvme_boot() {
    local device
    for device do
        [ -b "$device" ] || return 1
        lsblk -snro TYPE,TRAN "$device" | awk '
            $1 == "disk" { found=1; if ($2 != "nvme") other=1 }
            END { exit !found || other }
        ' || return 1
    done
}

# Do not call the private selector *.dtb: the EFI stub also scans for dtb=.
claw_qcom_iso_menu() {
    local rootfs=$1 media=$2 version=$3 label=$4
    local entries tree _compatible model source board_args
    entries=$(
        claw_qcom_boards "$rootfs" "$version" | while IFS='|' read -r tree _compatible model; do
            source=$(claw_qcom_tree "$rootfs" "$version" "$tree") || continue
            install -Dm0644 "$source" "$media/live/dtb/qcom/$tree" || exit 1
            board_args=$(claw_qcom_board_args "$tree")
            cat <<MENU
    menuentry '$model' {
        if devicetree /live/dtb/qcom/$tree; then
            linux /live/vmlinuz boot=live components $claw_qcom_args $board_args $claw_qcom_no_dsp claw.board_tree=$tree
            initrd /live/initrd.img
        fi
    }
MENU
        done
    ) || return 1
    if [ -n "$entries" ]; then
        printf "\nsubmenu '%s - Snapdragon X/X2 (experimental)' {\n%s\n}\n" "$label" "$entries"
    fi
}
