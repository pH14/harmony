#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

struct retro_system_info {
    const char *library_name;
    const char *library_version;
    const char *valid_extensions;
    bool need_fullpath;
    bool block_extract;
};

struct retro_game_info {
    const char *path;
    const void *data;
    size_t size;
    const char *meta;
};

extern int probe_value(void);
static uint8_t ram[2048];

void retro_set_environment(void *callback) { (void)callback; }
void retro_set_video_refresh(void *callback) { (void)callback; }
void retro_set_audio_sample(void *callback) { (void)callback; }
void retro_set_audio_sample_batch(void *callback) { (void)callback; }
void retro_set_input_poll(void *callback) { (void)callback; }
void retro_set_input_state(void *callback) { (void)callback; }
void retro_init(void) {}
void retro_deinit(void) {}
void retro_get_system_info(struct retro_system_info *info) {
    info->library_name = "QuickNES";
    info->library_version = "1.0-WIP26bb785c9deddb66a17717b21bb4e328f03ade32";
    info->valid_extensions = "nes";
    info->need_fullpath = false;
    info->block_extract = false;
}
bool retro_load_game(const struct retro_game_info *game) {
    (void)game;
    ram[0] = (uint8_t)probe_value();
    return true;
}
void retro_unload_game(void) {}
void retro_run(void) { ram[0] = (uint8_t)(ram[0] + probe_value()); }
size_t retro_serialize_size(void) { return 1; }
bool retro_serialize(void *data, size_t size) {
    if (!data || size != 1) return false;
    *(uint8_t *)data = ram[0];
    return true;
}
bool retro_unserialize(const void *data, size_t size) {
    if (!data || size != 1) return false;
    ram[0] = *(const uint8_t *)data;
    return true;
}
void *retro_get_memory_data(unsigned id) { return id == 2 ? ram : NULL; }
size_t retro_get_memory_size(unsigned id) { return id == 2 ? sizeof(ram) : 0; }
