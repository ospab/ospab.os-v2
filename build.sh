#!/usr/bin/env bash
# Build script for ospab.os v2 (AETERNA) — kernel + Limine + hybrid ISO (BIOS + UEFI)
#
# Usage:
#   ./build.sh           — build kernel + ISO
#   ./build.sh kernel    — build kernel only (no ISO)
#
# Env:
#   TARGET               — rust target (e.g. x86_64-unknown-none; empty = host)
#   LIMINE_BIN_DIR       — dir with BOOTX64.EFI, limine-bios.sys, limine-bios-cd.bin
#   LIMINE_CONF_SRC     — path to limine.conf (default: ./limine.conf)
#
# Requires: cargo, xorriso, mtools (mcopy, mmd, mkfs.vfat), dd
# Optional: limine CLI (enroll-config, bios-install), isohybrid
set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$SCRIPT_DIR"
ISOS_DIR="$PROJECT_ROOT/isos"
# Limine: put BOOTX64.EFI, limine-bios.sys, limine-bios-cd.bin in tools/limine/bin
# Or set LIMINE_BIN_DIR to your limine-10.8.2/bin (e.g. ../limine-10.8.2/bin)
if [ -z "$LIMINE_BIN_DIR" ]; then
    if [ -d "$PROJECT_ROOT/tools/limine/bin" ]; then
        LIMINE_BIN_DIR="$PROJECT_ROOT/tools/limine/bin"
    elif [ -d "$PROJECT_ROOT/../limine-10.8.2/bin" ]; then
        LIMINE_BIN_DIR="$PROJECT_ROOT/../limine-10.8.2/bin"
    else
        LIMINE_BIN_DIR="$PROJECT_ROOT/tools/limine/bin"
    fi
fi
LIMINE_CONF_SRC="${LIMINE_CONF_SRC:-$PROJECT_ROOT/limine.conf}"
TARGET="${TARGET:-}"
ISO_ROOT="${ISO_ROOT:-/tmp/ospab_os_v2_iso_root}"
ESP_IMG="$ISO_ROOT/efiboot.img"
BUILD_ISO=true
[ "$1" = "kernel" ] && BUILD_ISO=false

# --- Auto-increment ISO number ---
mkdir -p "$ISOS_DIR"
LAST_NUM=$(ls -1 "$ISOS_DIR"/ospab-os-v2-*.iso 2>/dev/null | sed 's/.*ospab-os-v2-\([0-9]*\)\.iso/\1/' | sort -n | tail -1)
if [ -z "$LAST_NUM" ]; then
    NEXT_NUM=1
else
    NEXT_NUM=$((LAST_NUM + 1))
fi
ISO_NAME="ospab-os-v2-${NEXT_NUM}.iso"
ISO_PATH="$ISOS_DIR/$ISO_NAME"

echo "=== Building ospab.os v2 (AETERNA) — ISO #$NEXT_NUM ==="
cd "$PROJECT_ROOT"

# --- Build kernel (custom target needs build-std; -A warnings avoids nightly ICE on bin crate) ---
export RUSTFLAGS="${RUSTFLAGS:--A warnings}"
echo "--- Building kernel ---"
if [ -n "$TARGET" ]; then
    cargo +nightly build --release --target "$TARGET" -Z build-std=core,alloc
    KERNEL_BIN="$PROJECT_ROOT/target/$TARGET/release/ospab_os"
else
    cargo +nightly build --release -Z build-std=core
    # .cargo/config sets target = x86_64-ospab.json
    KERNEL_BIN="$PROJECT_ROOT/target/x86_64-ospab/release/ospab_os"
fi

if [ ! -f "$KERNEL_BIN" ]; then
    echo "ERROR: Kernel binary not found: $KERNEL_BIN" >&2
    exit 1
fi

echo "Kernel built: $KERNEL_BIN"
[ "$BUILD_ISO" = false ] && exit 0

# --- Ensure Limine binaries (build from limine-10.8.2 if missing) ---
LIMINE_NEEDED=
[ ! -f "$LIMINE_BIN_DIR/BOOTX64.EFI" ] && LIMINE_NEEDED=1
[ ! -f "$LIMINE_BIN_DIR/limine-bios.sys" ] && LIMINE_NEEDED=1
[ ! -f "$LIMINE_BIN_DIR/limine-bios-cd.bin" ] && LIMINE_NEEDED=1

if [ -n "$LIMINE_NEEDED" ]; then
    LIMINE_SRC="${LIMINE_SRC:-$PROJECT_ROOT/limine-10.8.2}"
    if [ -f "$LIMINE_SRC/configure" ]; then
        echo "--- Building Limine from $LIMINE_SRC ---"
        mkdir -p "$PROJECT_ROOT/tools/limine/bin"
        ( cd "$LIMINE_SRC" && ./configure --enable-bios --enable-bios-cd --enable-uefi-x86-64 && make -j"$(nproc 2>/dev/null || echo 2)" )
        LIMINE_BUILT_BIN="$LIMINE_SRC/bin"
        if [ -d "$LIMINE_BUILT_BIN" ] && [ -f "$LIMINE_BUILT_BIN/BOOTX64.EFI" ]; then
            cp -f "$LIMINE_BUILT_BIN/BOOTX64.EFI" "$LIMINE_BUILT_BIN/limine-bios.sys" "$LIMINE_BUILT_BIN/limine-bios-cd.bin" "$PROJECT_ROOT/tools/limine/bin/"
            [ -x "$LIMINE_BUILT_BIN/limine" ] && cp -f "$LIMINE_BUILT_BIN/limine" "$PROJECT_ROOT/tools/limine/bin/"
            LIMINE_BIN_DIR="$PROJECT_ROOT/tools/limine/bin"
            echo "--- Limine binaries copied to $LIMINE_BIN_DIR ---"
        fi
    fi
fi

if [ ! -f "$LIMINE_BIN_DIR/BOOTX64.EFI" ] || [ ! -f "$LIMINE_BIN_DIR/limine-bios.sys" ] || [ ! -f "$LIMINE_BIN_DIR/limine-bios-cd.bin" ]; then
    echo "ERROR: Limine binaries not found in $LIMINE_BIN_DIR" >&2
    echo "  Required: BOOTX64.EFI, limine-bios.sys, limine-bios-cd.bin" >&2
    echo "  Put limine-10.8.2 source in project root and run ./build.sh again, or copy from release." >&2
    exit 1
fi

# --- Prepare ISO root ---
echo "--- Preparing ISO root ---"
rm -rf "$ISO_ROOT"
mkdir -p "$ISO_ROOT/boot/limine"
mkdir -p "$ISO_ROOT/EFI/BOOT"

# Kernel: copy as expected by limine.conf (AETERNA Live = /boot/ospab-live.elf) and legacy name
cp "$KERNEL_BIN" "$ISO_ROOT/boot/ospab-live.elf"
cp "$KERNEL_BIN" "$ISO_ROOT/boot/KERNEL"

# Limine config
if [ -f "$LIMINE_CONF_SRC" ]; then
    cp "$LIMINE_CONF_SRC" "$ISO_ROOT/limine.conf"
    cp "$ISO_ROOT/limine.conf" "$ISO_ROOT/boot/limine/limine.conf"
else
    echo "WARN: $LIMINE_CONF_SRC not found; using minimal limine.conf"
    cat > "$ISO_ROOT/limine.conf" << 'EOF'
TIMEOUT=5
[entry]
label: AETERNA
path: /boot/KERNEL
protocol: limine
EOF
    cp "$ISO_ROOT/limine.conf" "$ISO_ROOT/boot/limine/limine.conf"
fi

# Limine BIOS stages
cp "$LIMINE_BIN_DIR/limine-bios-cd.bin" "$ISO_ROOT/boot/limine/"
cp "$LIMINE_BIN_DIR/limine-bios.sys"   "$ISO_ROOT/boot/limine/"
# UEFI
cp "$LIMINE_BIN_DIR/BOOTX64.EFI"       "$ISO_ROOT/EFI/BOOT/"

# --- Create FAT32 ESP image for UEFI ---
echo "--- Creating ESP image (FAT32) for UEFI ---"
rm -f "$ESP_IMG"
dd if=/dev/zero of="$ESP_IMG" bs=1M count=64 status=none
mkfs.vfat -F 32 "$ESP_IMG"
mmd -i "$ESP_IMG" ::/EFI ::/EFI/BOOT
mcopy -i "$ESP_IMG" "$LIMINE_BIN_DIR/BOOTX64.EFI" ::/EFI/BOOT/BOOTX64.EFI
mcopy -i "$ESP_IMG" "$ISO_ROOT/limine.conf"      ::/EFI/BOOT/limine.conf
mmd -i "$ESP_IMG" ::/boot 2>/dev/null || true
mcopy -i "$ESP_IMG" "$KERNEL_BIN" ::/boot/KERNEL

# --- Build hybrid ISO with xorriso (BIOS + UEFI) ---
echo "--- Creating hybrid ISO (xorriso) ---"
xorriso -as mkisofs \
    -iso-level 3 \
    -R -J \
    -b boot/limine/limine-bios-cd.bin \
    -no-emul-boot \
    -boot-load-size 4 \
    -boot-info-table \
    -eltorito-alt-boot \
    -eltorito-platform efi \
    -e efiboot.img \
    -no-emul-boot \
    -isohybrid-gpt-basdat \
    -append_partition 2 0xef "$ESP_IMG" \
    -appended_part_as_gpt \
    -partition_cyl_align all \
    "$ISO_ROOT" -o "$ISO_PATH"

# --- Post-process with Limine (enroll config, BIOS install) ---
echo "--- Post-processing ---"
LIMINE_CLI="$LIMINE_BIN_DIR/limine"
if [ -x "$LIMINE_CLI" ]; then
    "$LIMINE_CLI" enroll-config "$ISO_PATH" "$ISO_ROOT/limine.conf" 2>/dev/null || true
    "$LIMINE_CLI" bios-install "$ISO_PATH" 2>/dev/null || true
else
    echo "Note: limine CLI not found; skip enroll-config / bios-install"
fi
if command -v isohybrid >/dev/null 2>&1; then
    isohybrid --uefi "$ISO_PATH" 2>/dev/null || echo "Note: isohybrid finished with warnings"
fi

echo ""
echo "DONE. ISO: $ISO_PATH"
echo "Run: qemu-system-x86_64 -cdrom \"$ISO_PATH\" -m 256M -serial stdio"
