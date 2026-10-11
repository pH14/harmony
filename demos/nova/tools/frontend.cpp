// SPDX-License-Identifier: GPL-2.0-or-later
#include "libretro.h"
#include <cstdint>
#include <cstring>
#include <vector>
#include <algorithm>

static uint8_t buttons;
static unsigned pixel_format;
static bool render_enabled = true;
static bool audio_enabled = false;
static int16_t audio_samples[19200];
static size_t audio_count;
static unsigned width = 256, height = 240;
static uint8_t pixels[256 * 240 * 4];
static std::vector<uint8_t> cartridge;

static bool environment(unsigned command, void* data) {
    switch (command) {
    case RETRO_ENVIRONMENT_GET_CAN_DUPE: *static_cast<bool*>(data) = true; return true;
    case RETRO_ENVIRONMENT_SET_PIXEL_FORMAT: pixel_format = *static_cast<unsigned*>(data); return pixel_format <= 2;
    case RETRO_ENVIRONMENT_GET_VARIABLE: {
        auto* v = static_cast<retro_variable*>(data);
        struct Option { const char* key; const char* value; };
        static const Option options[] = {
            {"quicknes_aspect_ratio_par","PAR"}, {"quicknes_use_overscan_h","enabled"},
            {"quicknes_use_overscan_v","disabled"}, {"quicknes_palette","default"},
            {"quicknes_no_sprite_limit","disabled"}, {"quicknes_audio_samplerate","48000"},
            {"quicknes_audio_nonlinear","nonlinear"}, {"quicknes_audio_eq","default"},
            {"quicknes_turbo_enable","none"}, {"quicknes_turbo_pulse_width","3"},
            {"quicknes_up_down_allowed","disabled"}
        };
        for (auto& o: options) if (!strcmp(v->key,o.key)) {v->value=o.value;return true;}
        return false;
    }
    case RETRO_ENVIRONMENT_GET_VARIABLE_UPDATE: *static_cast<bool*>(data)=false; return true;
    case RETRO_ENVIRONMENT_GET_AUDIO_VIDEO_ENABLE: *static_cast<int*>(data)=(render_enabled ? 1 : 0) | (audio_enabled ? 2 : 8); return true;
    case RETRO_ENVIRONMENT_GET_INPUT_BITMASKS: return true;
    case RETRO_ENVIRONMENT_SET_VARIABLES:
    case RETRO_ENVIRONMENT_SET_INPUT_DESCRIPTORS:
    case RETRO_ENVIRONMENT_SET_CORE_OPTIONS:
    case RETRO_ENVIRONMENT_SET_CORE_OPTIONS_INTL:
    case RETRO_ENVIRONMENT_SET_CORE_OPTIONS_V2:
    case RETRO_ENVIRONMENT_SET_CORE_OPTIONS_V2_INTL: return true;
    default: return false;
    }
}
static void video(const void* data, unsigned w, unsigned h, size_t pitch) {
    if (!data || !render_enabled || w>256 || h>240) return;
    width=w;height=h;
    for (unsigned y=0;y<h;y++) for(unsigned x=0;x<w;x++) {
        const auto* row=static_cast<const uint8_t*>(data)+y*pitch;
        uint8_t r,g,b;
        if (pixel_format==1) {uint32_t color;memcpy(&color,row+x*4,4);r=color>>16;g=color>>8;b=color;}
        else {uint16_t color;memcpy(&color,row+x*2,2);unsigned green=pixel_format==2?6:5;unsigned rv=color>>(green+5),gv=(color>>5)&((1<<green)-1),bv=color&31;r=rv*255/31;g=gv*255/((1<<green)-1);b=bv*255/31;}
        auto* out=pixels+(y*w+x)*4;out[0]=r;out[1]=g;out[2]=b;out[3]=255;
    }
}
static size_t audio_batch(const int16_t* samples,size_t frames) {
    size_t count=std::min(frames*2, size_t(19200)-audio_count);
    if(audio_enabled && count) {memcpy(audio_samples+audio_count,samples,count*sizeof(int16_t));audio_count+=count;}
    return frames;
}
static void audio(int16_t l,int16_t r) {int16_t pair[]={l,r};audio_batch(pair,1);}
static void poll() {}
static int16_t input(unsigned port,unsigned device,unsigned,unsigned id) {
    if(port || device!=RETRO_DEVICE_JOYPAD) return 0;
    const unsigned bits=(buttons&0xfc)|((buttons&1)<<8)|((buttons&2)>>1);
    return id==RETRO_DEVICE_ID_JOYPAD_MASK ? bits : id<16 && (bits&(1<<id)) ? 1 : 0;
}
extern "C" {
int nova_load(const uint8_t* rom,int size) {
    if(size<=0 || size>1024*1024) return 0;
    cartridge.assign(rom,rom+size);
    retro_set_environment(environment);retro_set_video_refresh(video);
    retro_set_audio_sample(audio);retro_set_audio_sample_batch(audio_batch);
    retro_set_input_poll(poll);retro_set_input_state(input);retro_init();
    retro_game_info info={nullptr,cartridge.data(),cartridge.size(),nullptr};
    return retro_load_game(&info);
}
void nova_run(int b,int frames,int render) {
    buttons=b;
    for(int i=0;i<frames;i++) {render_enabled=render!=0 && i==frames-1;retro_run();}
}
void nova_audio_enable(int enabled) {audio_enabled=enabled!=0;audio_count=0;}
int16_t* nova_audio_samples() {return audio_samples;}
int nova_audio_count() {return audio_count;}
void nova_audio_clear() {audio_count=0;}
int nova_state_size() {return retro_serialize_size();}
int nova_save(uint8_t* bytes,int size) {return size==nova_state_size() && retro_serialize(bytes,size);}
int nova_restore(const uint8_t* bytes,int size) {return size==nova_state_size() && retro_unserialize(bytes,size);}
uint8_t* nova_pixels() {return pixels;}
int nova_width() {return width;}
int nova_height() {return height;}
uint8_t* nova_ram() {return static_cast<uint8_t*>(retro_get_memory_data(RETRO_MEMORY_SYSTEM_RAM));}
int nova_ram_size() {return retro_get_memory_size(RETRO_MEMORY_SYSTEM_RAM);}
uint8_t* nova_sram() {return static_cast<uint8_t*>(retro_get_memory_data(RETRO_MEMORY_SAVE_RAM));}
int nova_sram_size() {return retro_get_memory_size(RETRO_MEMORY_SAVE_RAM);}
}
