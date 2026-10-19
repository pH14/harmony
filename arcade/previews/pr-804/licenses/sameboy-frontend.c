// SPDX-License-Identifier: AGPL-3.0-or-later
#include "gb.h"
#include "random.h"
#include "sameboy-boot.h"
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#ifndef SAMEBOY_MODEL
#define SAMEBOY_MODEL GB_MODEL_AGB_A
#endif

enum { width = 160, height = 144, ram_seed = 0x5eed, audio_rate = 48000 };

static GB_gameboy_t *gb;
static uint32_t screen[width * height];
static int16_t audio_samples[19200];
static size_t audio_count;
static int audio_enabled;
static size_t state_size;

static uint32_t encode_rgb(GB_gameboy_t *unused, uint8_t r, uint8_t g, uint8_t b)
{
    (void)unused;
    return (uint32_t)r | (uint32_t)g << 8 | (uint32_t)b << 16 | 0xffu << 24;
}

static void push_sample(GB_gameboy_t *unused, GB_sample_t *sample)
{
    (void)unused;
    if (audio_enabled && audio_count + 2 <= sizeof audio_samples / sizeof audio_samples[0]) {
        audio_samples[audio_count++] = sample->left;
        audio_samples[audio_count++] = sample->right;
    }
}

// The byte uses the QuickNES frontend's order: A, B, Select, Start, Up, Down,
// Left, Right from bit 0. SameBoy orders keys Right, Left, Up, Down, A, B,
// Select, Start.
static GB_key_mask_t key_mask(int b)
{
    return (GB_key_mask_t)(((b & 0x0f) << 4) | (b & 0x10 ? GB_KEY_UP_MASK : 0) |
        (b & 0x20 ? GB_KEY_DOWN_MASK : 0) | (b & 0x40 ? GB_KEY_LEFT_MASK : 0) |
        (b & 0x80 ? GB_KEY_RIGHT_MASK : 0));
}

// A frame ends at VBlank in the middle of the PPU's work. The fetcher keeps
// values from the last visible line that the next line overwrites before
// reading, and how far it got depends on whether rendering is enabled, so a
// frame boundary stores the values the next line starts from. The APU defers
// cycles until a sample is due; flushing them makes the snapshot independent
// of that schedule.
static void settle_frame(void)
{
    GB_apu_run(gb, true);
    gb->lcd_x = 0;
    gb->fetcher_state = 0;
    gb->fetcher_y = 0;
    gb->current_tile = 0;
    gb->current_tile_attributes = 0;
    gb->current_tile_data[0] = gb->current_tile_data[1] = 0;
    gb->data_for_sel_glitch = 0;
    gb->last_tile_data_address = 0;
    gb->last_tile_index_address = 0;
    memset(&gb->bg_fifo, 0, sizeof gb->bg_fifo);
    memset(&gb->oam_fifo, 0, sizeof gb->oam_fifo);
    gb->n_visible_objs = 0;
}

int gb_load(const uint8_t *rom, int size)
{
    if (size < 0x8000 || size > 8 * 1024 * 1024) return 0;
    if (gb) {
        GB_free(gb);
        free(gb);
    }
    gb = malloc(GB_allocation_size());
    if (!gb) return 0;
    GB_random_seed(ram_seed);
    GB_init(gb, SAMEBOY_MODEL);
    GB_load_boot_rom_from_buffer(gb, sameboy_boot_rom, sizeof sameboy_boot_rom);
    GB_load_rom_from_buffer(gb, rom, size);
    GB_set_rtc_mode(gb, GB_RTC_MODE_ACCURATE);
    GB_set_emulate_joypad_bouncing(gb, false);
    GB_set_color_correction_mode(gb, GB_COLOR_CORRECTION_DISABLED);
    GB_set_rgb_encode_callback(gb, encode_rgb);
    GB_set_pixels_output(gb, screen);
    GB_set_sample_rate(gb, audio_enabled? audio_rate : 0);
    GB_apu_set_sample_callback(gb, push_sample);
    GB_set_rendering_disabled(gb, true);
    GB_random_seed(ram_seed);
    GB_reset(gb);
    gb->last_rtc_second = 0;
    gb->rtc_cycles = 0;
    state_size = GB_get_save_state_size_no_bess(gb);
    audio_count = 0;
    return 1;
}

void gb_run(int b, int frames, int render)
{
    GB_set_key_mask(gb, key_mask(b));
    for (int i = 0; i < frames; i++) {
        GB_set_rendering_disabled(gb, !(render && i == frames - 1));
        GB_run_frame(gb);
        settle_frame();
    }
    GB_set_rendering_disabled(gb, true);
}

// Sampling only decides when the APU renders output; the emulated state is
// the same at any rate, and a rate of zero lets halted stretches run longer.
void gb_audio_enable(int enabled)
{
    audio_enabled = enabled != 0;
    audio_count = 0;
    if (gb) GB_set_sample_rate(gb, audio_enabled? audio_rate : 0);
}

int16_t *gb_audio_samples(void) { return audio_samples; }
int gb_audio_count(void) { return (int)audio_count; }
void gb_audio_clear(void) { audio_count = 0; }
int gb_state_size(void) { return (int)state_size; }

int gb_save(uint8_t *bytes, int size)
{
    if ((size_t)size != state_size) return 0;
    GB_save_state_to_buffer_no_bess(gb, bytes);
    return 1;
}

int gb_restore(const uint8_t *bytes, int size)
{
    if ((size_t)size != state_size || GB_load_state_from_buffer(gb, bytes, size)) return 0;
    gb->joypad_is_stable = false;
    return 1;
}

uint8_t *gb_pixels(void) { return (uint8_t *)screen; }
int gb_width(void) { return width; }
int gb_height(void) { return height; }
uint8_t *gb_wram(void) { return gb->ram; }
int gb_wram_size(void) { return (int)gb->ram_size; }
uint8_t *gb_hram(void) { return gb->hram; }
int gb_hram_size(void) { return 0x7f; }
