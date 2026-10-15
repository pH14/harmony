#include <cstddef>
#include <cstdint>

#ifndef VALUE
#error VALUE is required
#endif

struct retro_system_info {
    const char *library_name;
    const char *library_version;
    const char *valid_extensions;
    bool need_fullpath;
    bool block_extract;
};

#ifdef QUICKNES_GNU_UNIQUE
inline int quicknes_unique_global = VALUE;
extern "C" int quicknes_unique_reference() { return quicknes_unique_global; }
#endif

static uint8_t ram[2048];

extern "C" {
int quicknes_image_marker() { return VALUE; }
void *quicknes_image_global() { return ram; }
void retro_set_environment(void *) {}
void retro_set_video_refresh(void *) {}
void retro_set_audio_sample(void *) {}
void retro_set_audio_sample_batch(void *) {}
void retro_set_input_poll(void *) {}
void retro_set_input_state(void *) {}
void retro_init() {}
void retro_deinit() {}
void retro_get_system_info(retro_system_info *info) {
    info->library_name = "QuickNES";
    info->library_version = "1.0-WIP26bb785c9deddb66a17717b21bb4e328f03ade32";
    info->valid_extensions = "nes";
    info->need_fullpath = false;
    info->block_extract = false;
}
bool retro_load_game(const void *) {
    ram[0] = VALUE;
    return true;
}
void retro_unload_game() {}
void retro_run() { ++ram[0]; }
size_t retro_serialize_size() { return 1; }
bool retro_serialize(void *data, size_t size) {
    if (data == nullptr || size != 1) return false;
    *static_cast<uint8_t *>(data) = ram[0];
    return true;
}
bool retro_unserialize(const void *data, size_t size) {
    if (data == nullptr || size != 1) return false;
    ram[0] = *static_cast<const uint8_t *>(data);
    return true;
}
void *retro_get_memory_data(unsigned id) { return id == 2 ? ram : nullptr; }
size_t retro_get_memory_size(unsigned id) { return id == 2 ? sizeof(ram) : 0; }
}
