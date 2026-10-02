/* SPDX-License-Identifier: AGPL-3.0-or-later */
#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include "libretro.h"

extern uint32_t harmony_chord(uint32_t frame);
__attribute__((import_module("harmony_v1"), import_name("request")))
extern int32_t harmony_request(uint32_t operation, const void *input,
    uint32_t input_len, void *output, uint32_t output_capacity);

static uint64_t decision_sequence;
static uint32_t harmony_decision(uint32_t suggested) {
    uint8_t input[14] = {0x45, 0x4e};
    uint8_t output[5];
    uint64_t request_id = decision_sequence++;
    for (uint32_t i = 0; i < 8; ++i)
        input[2 + i] = request_id >> (8 * i);
    for (uint32_t i = 0; i < 4; ++i)
        input[10 + i] = suggested >> (8 * i);
    int32_t length = harmony_request((6u << 16) | 3u, input, sizeof(input), output, sizeof(output));
    if (length == 1 && output[0] == 0)
        return suggested;
    if (length != 5 || output[0] != 1)
        abort();
    return (uint32_t)output[1] | ((uint32_t)output[2] << 8) |
        ((uint32_t)output[3] << 16) | ((uint32_t)output[4] << 24);
}

static uint32_t frame_number;
static uint32_t buttons;
static uint8_t rom[1024 * 1024];
static uint8_t *ram;
static uint8_t *save_ram;

static bool environment(unsigned command, void *data) {
    switch (command) {
    case RETRO_ENVIRONMENT_SET_PIXEL_FORMAT:
    case RETRO_ENVIRONMENT_SET_INPUT_DESCRIPTORS:
    case RETRO_ENVIRONMENT_SET_MEMORY_MAPS:
    case RETRO_ENVIRONMENT_SET_VARIABLES:
        return true;
    case RETRO_ENVIRONMENT_GET_VARIABLE_UPDATE:
        *(bool *)data = false;
        return true;
    case RETRO_ENVIRONMENT_GET_VARIABLE:
        ((struct retro_variable *)data)->value = NULL;
        return false;
    case RETRO_ENVIRONMENT_GET_AUDIO_VIDEO_ENABLE:
        *(int *)data = 0;
        return true;
    default:
        return false;
    }
}
static void video(const void *data, unsigned width, unsigned height, size_t pitch) {
    (void)data; (void)width; (void)height; (void)pitch;
}
static void audio(int16_t left, int16_t right) { (void)left; (void)right; }
static size_t audio_batch(const int16_t *data, size_t frames) { (void)data; return frames; }
static void poll(void) { buttons = harmony_decision(harmony_chord(frame_number)); }
static int16_t input(unsigned port, unsigned device, unsigned index, unsigned id) {
    (void)device; (void)index;
    static const uint8_t map[16] = {1, 255, 2, 3, 4, 5, 6, 7, 0, 255, 255, 255, 255, 255, 255, 255};
    return port == 0 && id < 16 && map[id] < 8 && (buttons & (1u << map[id])) ? 1 : 0;
}
__attribute__((export_name("rom_address")))
uint32_t rom_address(void) { return (uint32_t)(uintptr_t)rom; }
__attribute__((export_name("initialize")))
uint32_t initialize(uint32_t length) {
    if (length > sizeof(rom)) return 1;
    retro_set_environment(environment);
    retro_set_video_refresh(video);
    retro_set_audio_sample(audio);
    retro_set_audio_sample_batch(audio_batch);
    retro_set_input_poll(poll);
    retro_set_input_state(input);
    retro_init();
    struct retro_game_info game = {NULL, rom, length, NULL};
    if (!retro_load_game(&game)) return 2;
    ram = retro_get_memory_data(RETRO_MEMORY_SYSTEM_RAM);
    save_ram = retro_get_memory_data(RETRO_MEMORY_SAVE_RAM);
    if (!ram || !save_ram || retro_get_memory_size(RETRO_MEMORY_SYSTEM_RAM) != 2048 ||
        retro_get_memory_size(RETRO_MEMORY_SAVE_RAM) != 8192) return 3;
    memset(save_ram, 0xff, 8192);
    return 0;
}
__attribute__((export_name("run")))
uint32_t run(uint32_t frames) {
    for (uint32_t i = 0; i < frames; ++i) {
        retro_run();
        ++frame_number;
    }
    return frame_number;
}
__attribute__((export_name("ram_address")))
uint32_t ram_address(void) { return (uint32_t)(uintptr_t)ram; }
__attribute__((export_name("save_ram_address")))
uint32_t save_ram_address(void) { return (uint32_t)(uintptr_t)save_ram; }
