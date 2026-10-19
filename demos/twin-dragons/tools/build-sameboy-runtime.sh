#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
set -euo pipefail
demo=$(cd -- "$(dirname -- "$0")/.." && pwd)
build=$demo/.build
mkdir -p "$build" "$demo/public/engine"
revision=98da08c3117a7bf39c7329fc662c0d18cb54707d
rgbds_version=v1.0.4
logo_sha256=ea7d99347f748b9d6a7ed675c3e214d9571619c504a1e5e671be4173ce839341
boot_sha256=a8d20344fd69752ef171cff8069396758946a53d6871dcf176c82cccf2157998

case "$(uname -s)-$(uname -m)" in
    Darwin-*)
        rgbds_asset=rgbds-macos.zip
        rgbds_sha256=62d1669bbe3b2bf2cfc2e72076d3c8c00fc023a9cc15d458f0c723bd9d0312c4
        ;;
    Linux-x86_64)
        rgbds_asset=rgbds-linux-x86_64.tar.xz
        rgbds_sha256=3e19e50ea602e316be709160acaf0a708b1fa6d32d219ec541f244442895dbb7
        ;;
    *)
        echo "No pinned RGBDS release for $(uname -s)-$(uname -m)" >&2
        exit 1
        ;;
esac

rgbds=$build/rgbds-$rgbds_version
if [ ! -x "$rgbds/bin/rgbasm" ]; then
    rm -rf "$rgbds"
    mkdir -p "$rgbds/bin"
    curl -sSfL -o "$rgbds/$rgbds_asset" \
        "https://github.com/gbdev/rgbds/releases/download/$rgbds_version/$rgbds_asset"
    echo "$rgbds_sha256  $rgbds/$rgbds_asset" | sha256sum -c --quiet
    case "$rgbds_asset" in
        *.zip) unzip -q -o "$rgbds/$rgbds_asset" -d "$rgbds/bin" ;;
        *) tar -xJf "$rgbds/$rgbds_asset" -C "$rgbds/bin" ;;
    esac
fi
for tool in rgbasm rgblink rgbgfx; do
    found=$(find "$rgbds/bin" -name "$tool" -type f | head -n 1)
    [ -n "$found" ] && [ "$found" = "$rgbds/bin/$tool" ] || ln -sf "$found" "$rgbds/bin/$tool"
done
"$rgbds/bin/rgbasm" --version | grep -qx "rgbasm $rgbds_version"

sameboy=$build/sameboy
if [ ! -d "$sameboy/.git" ]; then
    git clone --quiet https://github.com/LIJI32/SameBoy.git "$sameboy"
fi
git -C "$sameboy" checkout --quiet --force --detach "$revision"
git -C "$sameboy" clean --quiet -fdx
test "$(git -C "$sameboy" rev-parse HEAD)" = "$revision"
patch -d "$sameboy" -f -p1 >/dev/null <<'PATCH'
diff --git a/Core/sm83_cpu.c b/Core/sm83_cpu.c
index 9c14e18c..dd2e3136 100644
--- a/Core/sm83_cpu.c
+++ b/Core/sm83_cpu.c
@@ -1618,6 +1618,10 @@ void GB_cpu_run(GB_gameboy_t *gb)
         return;
     }
     
+    if (gb->halted && GB_skip_halted_steps(gb)) {
+        return;
+    }
+    
     if ((gb->interrupt_enable & 0x10) && (gb->ime || gb->halted)) {
         GB_timing_sync(gb);
     }
diff --git a/Core/timing.c b/Core/timing.c
index 9b0332bf..7080049e 100644
--- a/Core/timing.c
+++ b/Core/timing.c
@@ -257,6 +257,24 @@ void GB_set_internal_div_counter(GB_gameboy_t *gb, uint16_t value)
     gb->div_counter = value;
 }
 
+static inline uint16_t div_watched_bits(GB_gameboy_t *gb)
+{
+    return TAC_TRIGGER_BITS[gb->io_registers[GB_IO_TAC] & 3] | gb->serial_mask |
+           (gb->cgb_double_speed? 0x2000 : 0x1000);
+}
+
+/* Most DIV increments change none of the bits that clock TIMA, the serial
+   port or the APU frame sequencer. */
+static inline void increase_div(GB_gameboy_t *gb)
+{
+    uint16_t value = gb->div_counter + 4;
+    if (likely(!((gb->div_counter ^ value) & div_watched_bits(gb)))) {
+        gb->div_counter = value;
+        return;
+    }
+    GB_set_internal_div_counter(gb, value);
+}
+
 static void timers_run(GB_gameboy_t *gb, uint8_t cycles)
 {
     if (gb->stopped) {
@@ -275,7 +293,7 @@ static void timers_run(GB_gameboy_t *gb, uint8_t cycles)
     GB_SLEEP(gb, div, 1, 3);
     while (true) {
         advance_tima_state_machine(gb);
-        GB_set_internal_div_counter(gb, gb->div_counter + 4);
+        increase_div(gb);
         gb->apu.apu_cycles += 1 << !gb->cgb_double_speed;
         gb->apu_output.sample_cycles += (gb->apu_output.sample_rate << !gb->cgb_double_speed) << 1;
         GB_SLEEP(gb, div, 2, 4);
@@ -432,6 +450,108 @@ static void camera_run(GB_gameboy_t *gb, uint8_t cycles)
 }
 
 
+static inline bool apu_forced(GB_gameboy_t *gb)
+{
+    return gb->apu.square_sweep_calculate_countdown || gb->apu.channel_1_restart_hold ||
+           gb->apu.square_sweep_calculate_countdown_reload_timer ||
+           (gb->model <= GB_MODEL_CGB_E && (gb->apu.wave_channel.bugged_read_countdown ||
+                                            (gb->apu.wave_channel.enable && gb->apu.wave_channel.pulsed)));
+}
+
+/* Mirrors the condition under which GB_apu_run(gb, false) does any work. */
+static inline bool apu_run_due(GB_gameboy_t *gb)
+{
+    return gb->apu.apu_cycles + gb->apu_output.cycles_since_render >= gb->apu_output.max_cycles_per_sample ||
+           gb->apu_output.sample_cycles >= GB_get_clock_rate(gb) || apu_forced(gb);
+}
+
+/* The PPU wakes once its counter turns positive, except at its two batching
+   points, where it waits until a whole mode's worth of cycles has
+   accumulated. Until it wakes, GB_display_run only adds to counters. */
+static inline int32_t display_wake(GB_gameboy_t *gb)
+{
+    int32_t wake = 1;
+    if (gb->display_state == 5) {
+        wake = 80 * 2;
+    }
+    else if (gb->display_state == 3) {
+        wake = gb->mode3_batching_length * 2;
+    }
+    return wake > 1? wake : 1;
+}
+
+static inline int32_t display_idle_cycles(GB_gameboy_t *gb)
+{
+    if (gb->wy_check_scheduled && !gb->wy_triggered) return 0;
+    if (gb->delayed_glitch_hblank_interrupt || (gb->stopped && !GB_is_cgb(gb))) return 0;
+    int32_t idle = display_wake(gb) - 1 - gb->display_cycles;
+    if (gb->io_registers[GB_IO_LCDC] & GB_LCDC_ENABLE) {
+        int32_t line_left = 456 * 2 - gb->cycles_for_line * 2 - gb->display_cycles;
+        if (line_left < idle) {
+            idle = line_left;
+        }
+    }
+    return idle > 0? idle : 0;
+}
+
+static inline void display_idle(GB_gameboy_t *gb, uint32_t cycles)
+{
+    if (gb->wy_triggered) {
+        gb->wy_check_scheduled = false;
+    }
+    gb->cycles_since_vblank_callback += cycles / 2;
+    gb->frame_parity_ticks += cycles;
+    gb->wy_check_modulo += cycles;
+    gb->frame_repeat_countdown = gb->frame_repeat_countdown > cycles? gb->frame_repeat_countdown - cycles : 0;
+    gb->display_cycles += cycles;
+}
+
+/* Conditions under which a 4-cycle step leaves every unit other than DIV,
+   the APU and the PPU untouched. */
+static inline bool units_can_idle(GB_gameboy_t *gb)
+{
+    if (gb->stopped || gb->speed_switch_countdown || gb->speed_switch_halt_countdown || gb->speed_switch_freeze) return false;
+    if (!gb->joypad_is_stable || gb->printer_callback || gb->apu_output.output_file) return false;
+    if (gb->div_state != 2 || gb->div_cycles > 0 || gb->div_cycles <= -4) return false;
+    if (gb->tima_reload_state != GB_TIMA_RUNNING || gb->apu.pending_envelope_tick) return false;
+    if (gb->dma_current_dest != 0xA1 && !gb->halted) return false;
+    if (gb->cartridge_type->mbc_type == GB_CAMERA || gb->cartridge_type->mbc_type == GB_HUC1 ||
+        gb->cartridge_type->mbc_type == GB_HUC3 || gb->cartridge_type->has_rtc) return false;
+    if (gb->model <= GB_MODEL_CGB_E && gb->cgb_mode &&
+        ((gb->io_registers[GB_IO_RP] & 0xC0) == 0xC0 || gb->ir_sensor || gb->effective_ir_input)) return false;
+    if ((gb->io_registers[GB_IO_SC] & 0x81) == 0x81 || gb->serial_mask < 4 ||
+        (gb->serial_mask & (gb->serial_mask - 1))) return false;
+    return true;
+}
+
+/* Everything GB_advance_cycles does over 4-cycle steps in which no unit wakes,
+   except the DIV, TIMA and serial clock updates. */
+static inline void advance_idle_units(GB_gameboy_t *gb, uint32_t steps)
+{
+    uint32_t total = steps * (gb->cgb_double_speed? 4 : 8);
+    gb->apu.apu_cycles += steps << !gb->cgb_double_speed;
+    gb->apu_output.sample_cycles += steps * ((gb->apu_output.sample_rate << !gb->cgb_double_speed) << 1);
+    gb->apu.pcm_mask[0] = gb->apu.pcm_mask[1] = 0xFF;
+    gb->dma_cycles = 4;
+    if (gb->io_registers[GB_IO_LCDC] & GB_LCDC_ENABLE) {
+        gb->double_speed_alignment += total;
+    }
+    gb->cycles_since_last_sync += total;
+    gb->cycles_since_run += total;
+    gb->rumble_on_cycles += (gb->rumble_strength & 3) * steps;
+    gb->rumble_off_cycles += ((gb->rumble_strength & 3) ^ 3) * steps;
+    if (gb->data_bus_decay_countdown) {
+        if (gb->data_bus_decay_countdown <= total) {
+            gb->data_bus_decay_countdown = 0;
+            gb->data_bus = 0xFF;
+        }
+        else {
+            gb->data_bus_decay_countdown -= total;
+        }
+    }
+    display_idle(gb, total);
+}
+
 void GB_advance_cycles(GB_gameboy_t *gb, uint8_t cycles)
 {
     if (unlikely(gb->speed_switch_countdown)) {
@@ -510,8 +630,15 @@ void GB_advance_cycles(GB_gameboy_t *gb, uint8_t cycles)
     }
     
     GB_joypad_run(gb, cycles);
-    GB_apu_run(gb, false);
-    GB_display_run(gb, cycles, false);
+    if (apu_run_due(gb)) {
+        GB_apu_run(gb, false);
+    }
+    if (cycles <= display_idle_cycles(gb)) {
+        display_idle(gb, cycles);
+    }
+    else {
+        GB_display_run(gb, cycles, false);
+    }
     if (unlikely(!gb->stopped)) { // TODO: Verify what happens in STOP mode
         GB_dma_run(gb);
     }
@@ -519,6 +646,66 @@ void GB_advance_cycles(GB_gameboy_t *gb, uint8_t cycles)
     rtc_run(gb, cycles);
 }
 
+static uint32_t steps_before_crossing(uint32_t value, uint32_t period, uint32_t crossings)
+{
+    uint32_t limit = (value / period + crossings + 1) * period;
+    return (limit - value - 1) / 4;
+}
+
+static uint32_t crossings(uint32_t from, uint32_t to, uint32_t period)
+{
+    return to / period - from / period;
+}
+
+/* Takes as many of the 4-cycle steps of a halted CGB as possible at once,
+   stopping before the first step in which a unit does more than add to
+   counters: the PPU or APU waking, DIV reaching an APU frame sequencer edge,
+   TIMA overflowing, or a serial bit shifting. No interrupt can become pending
+   before that step, so the CPU stays halted throughout. */
+bool GB_skip_halted_steps(GB_gameboy_t *gb)
+{
+    unsigned cycles = gb->cgb_double_speed? 4 : 8;
+    uint32_t steps = display_idle_cycles(gb) / cycles;
+    if (steps == 0) return false;
+    if (!GB_is_cgb(gb) || gb->just_halted || gb->ime_toggle) return false;
+    if (gb->interrupt_enable & gb->io_registers[GB_IO_IF] & 0x1F) return false;
+    if (!units_can_idle(gb) || apu_run_due(gb)) return false;
+    
+    uint32_t div = gb->div_counter;
+    uint32_t limit = steps_before_crossing(div, gb->cgb_double_speed? 0x2000 : 0x1000, 0);
+    if (limit < steps) steps = limit;
+    
+    uint32_t apu_used = gb->apu.apu_cycles + gb->apu_output.cycles_since_render;
+    limit = (gb->apu_output.max_cycles_per_sample - 1 - apu_used) >> !gb->cgb_double_speed;
+    if (limit < steps) steps = limit;
+    
+    unsigned sample_step = (gb->apu_output.sample_rate << !gb->cgb_double_speed) << 1;
+    if (sample_step) {
+        limit = (GB_get_clock_rate(gb) - 1 - gb->apu_output.sample_cycles) / sample_step;
+        if (limit < steps) steps = limit;
+    }
+    
+    bool timer_enabled = gb->io_registers[GB_IO_TAC] & 4;
+    uint32_t timer_period = TAC_TRIGGER_BITS[gb->io_registers[GB_IO_TAC] & 3] * 2;
+    if (timer_enabled) {
+        limit = steps_before_crossing(div, timer_period, 0xFF - gb->io_registers[GB_IO_TIMA]);
+        if (limit < steps) steps = limit;
+    }
+    
+    if (steps == 0) return false;
+    
+    uint32_t new_div = div + steps * 4;
+    if (timer_enabled) {
+        gb->io_registers[GB_IO_TIMA] += crossings(div, new_div, timer_period);
+    }
+    if (crossings(div, new_div, gb->serial_mask * 2) & 1) {
+        gb->serial_master_clock ^= true;
+    }
+    gb->div_counter = new_div;
+    advance_idle_units(gb, steps);
+    return true;
+}
+
 /* 
    This glitch is based on the expected results of mooneye-gb rapid_toggle test.
    This glitch happens because how TIMA is increased, see GB_set_internal_div_counter.
diff --git a/Core/timing.h b/Core/timing.h
index b89bf161..33ef8ca3 100644
--- a/Core/timing.h
+++ b/Core/timing.h
@@ -15,6 +15,7 @@ void GB_set_rtc_multiplier(GB_gameboy_t *gb, double multiplier);
 
 #ifdef GB_INTERNAL
 internal void GB_advance_cycles(GB_gameboy_t *gb, uint8_t cycles);
+internal bool GB_skip_halted_steps(GB_gameboy_t *gb);
 internal void GB_emulate_timer_glitch(GB_gameboy_t *gb, uint8_t old_tac, uint8_t new_tac);
 internal bool GB_timing_sync_turbo(GB_gameboy_t *gb); /* Returns true if should skip frame */
 internal void GB_timing_sync(GB_gameboy_t *gb);
PATCH

boot=$sameboy/build/boot
mkdir -p "$boot"
"$rgbds/bin/rgbgfx" -Z -u -c embedded -o "$boot/SameBoyLogo.2bpp" "$sameboy/BootROMs/SameBoyLogo.png"
echo "$logo_sha256  $boot/SameBoyLogo.2bpp" | sha256sum -c --quiet
cc -std=c99 -O2 "$sameboy/BootROMs/pb12.c" -o "$boot/pb12"
"$boot/pb12" < "$boot/SameBoyLogo.2bpp" > "$boot/SameBoyLogo.pb12"
printf 'DEF AGB = 1\nDEF FAST = 1\ninclude "cgb_boot.asm"\n' > "$boot/agb_boot_fast.asm"
"$rgbds/bin/rgbasm" --include "$boot/" --include "$sameboy/BootROMs/" -o "$boot/agb_boot_fast.o" "$boot/agb_boot_fast.asm"
"$rgbds/bin/rgblink" -x -o "$boot/agb_boot_fast.bin" "$boot/agb_boot_fast.o"
echo "$boot_sha256  $boot/agb_boot_fast.bin" | sha256sum -c --quiet
{
    printf 'static const unsigned char sameboy_boot_rom[] = {'
    od -An -v -tu1 "$boot/agb_boot_fast.bin" | tr -s ' \n' ',,' | sed 's/^,//; s/,$//'
    printf '};\n'
} > "$boot/sameboy-boot.h"

core=$sameboy/Core
sources=()
for name in apu camera display gb joypad mbc memory printer random rumble save_state sgb sm83_cpu timing workboy; do
    sources+=("$core/$name.c")
done
flags=(-Os -flto -std=gnu11 -D_GNU_SOURCE -DNDEBUG -DGB_INTERNAL -DGB_VERSION='"1.0.4"'
    -DGB_DISABLE_TIMEKEEPING -DGB_DISABLE_REWIND -DGB_DISABLE_DEBUGGER -DGB_DISABLE_CHEATS
    -DGB_DISABLE_CHEAT_SEARCH -I"$core" -I"$boot")
emcc "${flags[@]}" "$demo/tools/sameboy-frontend.c" "${sources[@]}" \
    -sMODULARIZE=1 -sEXPORT_ES6=1 -sENVIRONMENT=web,worker,node \
    '-sEXPORTED_RUNTIME_METHODS=["HEAPU8"]' -sALLOW_MEMORY_GROWTH=1 -sFILESYSTEM=0 -sINITIAL_MEMORY=16777216 \
    -sSTACK_SIZE=262144 \
    '-sEXPORTED_FUNCTIONS=["_malloc","_free","_gb_load","_gb_run","_gb_state_size","_gb_save","_gb_restore","_gb_pixels","_gb_width","_gb_height","_gb_wram","_gb_wram_size","_gb_hram","_gb_hram_size","_gb_audio_enable","_gb_audio_samples","_gb_audio_count","_gb_audio_clear"]' \
    -o "$demo/public/engine/sameboy.js"
