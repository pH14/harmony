/* SPDX-License-Identifier: AGPL-3.0-or-later */
#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include "libretro.h"

static uint32_t buttons;
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
static void poll(void) {}
static int16_t input(unsigned port, unsigned device, unsigned index, unsigned id) {
    (void)device; (void)index;
    static const uint8_t map[16] = {1, 255, 2, 3, 4, 5, 6, 7, 0, 255, 255, 255, 255, 255, 255, 255};
    return port == 0 && id < 16 && map[id] < 8 && (buttons & (1u << map[id])) ? 1 : 0;
}
extern void _initialize(void);
uint32_t quicknes_initialize(const uint8_t *rom, uint32_t length) {
    if (!rom || !length || length > 1024 * 1024) return 1;
    _initialize();
    retro_set_environment(environment);
    retro_set_video_refresh(video);
    retro_set_audio_sample(audio);
    retro_set_audio_sample_batch(audio_batch);
    retro_set_input_poll(poll);
    retro_set_input_state(input);
    retro_init();
    struct retro_game_info game = {NULL, rom, length, NULL};
    if (!retro_load_game(&game)) return 2;
    retro_set_controller_port_device(0, RETRO_DEVICE_JOYPAD);
    ram = retro_get_memory_data(RETRO_MEMORY_SYSTEM_RAM);
    save_ram = retro_get_memory_data(RETRO_MEMORY_SAVE_RAM);
    if (!ram || !save_ram || retro_get_memory_size(RETRO_MEMORY_SYSTEM_RAM) != 2048 ||
        retro_get_memory_size(RETRO_MEMORY_SAVE_RAM) != 8192) return 3;
    memset(save_ram, 0xff, 8192);
    return 0;
}
void quicknes_frame(uint32_t joypad) {
    buttons = joypad;
    retro_run();
}
uint32_t quicknes_work_ram(uint8_t *output, uint32_t length) {
    if (!ram || length != 2048) return 0;
    memcpy(output, ram, length);
    return 1;
}
uint32_t quicknes_save_ram(uint8_t *output, uint32_t length) {
    if (!save_ram || length != 8192) return 0;
    memcpy(output, save_ram, length);
    return length;
}
