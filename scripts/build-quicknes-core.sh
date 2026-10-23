#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later

set -eu

script_dir=$(cd -P -- "$(dirname "$0")" && pwd)
revision=26bb785c9deddb66a17717b21bb4e328f03ade32
repository=https://github.com/libretro/QuickNES_Core.git
output=${1:-quicknes_libretro.so}
static_output=${HARMONY_QUICKNES_STATIC_OUTPUT:-}
build_root=$(mktemp -d "${TMPDIR:-/tmp}/harmony-quicknes.XXXXXX")
trap 'rm -rf "$build_root"' EXIT HUP INT TERM

git clone --quiet "$repository" "$build_root/core"
git -C "$build_root/core" checkout --quiet --detach "$revision"
test "$(git -C "$build_root/core" rev-parse HEAD)" = "$revision"

# NES games spend much of each frame spinning in a wait loop until the next
# interrupt. The patch skips whole iterations of a loop that provably repeats
# with no effect, so emulated state and timing match the unpatched core while
# search runs faster. Both the shared and static builds carry it. The patch is
# fail-closed: a source move at the pinned revision stops the build.
patch -d "$build_root/core" -f -p1 <<'PATCH'
--- a/nes_emu/Nes_Cpu.cpp
+++ b/nes_emu/Nes_Cpu.cpp
@@ -113,6 +113,45 @@
 	2,6,2,8,3,3,5,5,2,2,2,2,4,4,6,6,// E
 	3,5,2,8,4,4,6,6,2,4,2,7,4,4,7,7 // F
 };
+
+// Inside run(), every memory write, I/O-range read and interrupt-flag change
+// counts as an effect. A backward jump that reaches the same target with the
+// same registers and no effect since the previous visit has entered a loop
+// whose iterations repeat exactly until the next event, so whole iterations
+// are skipped up to clock_limit.
+
+#undef READ_LIKELY_PPU
+#undef READ
+#undef WRITE
+#undef WRITE_LOW
+
+#define NOTE_READ( addr )       (effects += (unsigned) ((addr) - 0x2000) < 0x6000)
+#define READ_LIKELY_PPU( addr ) (NOTE_READ( addr ), NES_CPU_READ_PPU( this, (addr), (clock_count) ))
+#define READ( addr )            (NOTE_READ( addr ), NES_CPU_READ( this, (addr), (clock_count) ))
+#define WRITE( addr, data )     {effects++; NES_CPU_WRITE( this, (addr), (data), (clock_count) );}
+#define WRITE_LOW( addr, data ) (void) (effects++, READ_LOW( addr ) = (data))
+
+#define IDLE_CHECK()                                                    \
+{                                                                       \
+	if ( pc == idle_pc && effects == idle_effects && a == idle_a &&     \
+			x == idle_x && y == idle_y && sp == idle_sp &&              \
+			status == idle_status && c == idle_c && nz == idle_nz )     \
+	{                                                                   \
+		nes_time_t period = clock_count - idle_clock;                   \
+		if ( period > 0 && clock_count < clock_limit )                  \
+			clock_count += (clock_limit - 1 - clock_count) / period * period; \
+	}                                                                   \
+	idle_pc = pc;                                                       \
+	idle_clock = clock_count;                                           \
+	idle_effects = effects;                                             \
+	idle_a = a;                                                         \
+	idle_x = x;                                                         \
+	idle_y = y;                                                         \
+	idle_sp = sp;                                                       \
+	idle_status = status;                                               \
+	idle_c = c;                                                         \
+	idle_nz = nz;                                                       \
+}
 
 Nes_Cpu::result_t Nes_Cpu::run( nes_time_t end )
 {
@@ -168,6 +207,13 @@
 		int temp = r.status;
 		SET_STATUS( temp );
 	}
+	
+	unsigned long effects = 0;
+	unsigned long idle_effects = 0;
+	unsigned idle_pc = ~0u;
+	nes_time_t idle_clock = 0;
+	int idle_a = 0, idle_x = 0, idle_y = 0, idle_sp = 0;
+	int idle_status = 0, idle_c = 0, idle_nz = 0;
 	
 	goto loop;
 dec_clock_loop:
@@ -280,6 +326,8 @@
 	pc += offset;       \
 	pc = uint16_t( pc ); \
 	clock_count += (extra_clock >> 8) & 1;  \
+	if ( offset < 0 )   \
+		IDLE_CHECK()    \
 	goto loop;          \
 }
 
@@ -304,9 +352,13 @@
 		goto loop;
 	}
 	
-	case 0x4C: // JMP abs
+	case 0x4C: { // JMP abs
+		unsigned from = pc;
 		pc = GET_OPERAND16( pc );
+		if ( pc < from )
+			IDLE_CHECK()
 		goto loop;
+	}
 	
 	case 0xE8: INC_DEC_XY( x, 1 )  // INX
 	
@@ -813,6 +865,7 @@
 		if ( !((data ^ status) & st_i) )
 			goto loop; // I flag didn't change
 	i_flag_changed:
+		effects++;
 		this->r.status = status; // update externally-visible I flag
 		// update clock_limit based on modified I flag
 		clock_limit = end_time_;
@@ -888,6 +941,7 @@
 			goto loop;
 		status &= ~st_i;
 	handle_cli:
+		effects++;
 		this->r.status = status; // update externally-visible I flag
 		if ( clock_count < end_time_ )
 		{
@@ -907,6 +961,7 @@
 			goto loop;
 		status |= st_i;
 	handle_sei:
+		effects++;
 		this->r.status = status; // update externally-visible I flag
 		clock_limit = end_time_;
 		if ( clock_count < irq_time_ )
@@ -1164,6 +1219,7 @@
 			len = 3;
 		pc += len - 1;
 		error_count_++;
+		effects++;
 		goto loop;
 		
 		//result = result_badop; // intentionally unreachable; see comment above
PATCH

# A four-screen cartridge saves a 4 KiB nametable block. The pinned reader
# reads it in two calls, and its read path subtracts the bytes read from the
# block's remaining length twice, so the second call fails and the state cannot
# be restored. The patch reads the whole block in one call. Two-screen states
# read exactly as before.
patch -d "$build_root/core" -f -p1 <<'PATCH'
--- a/nes_emu/Nes_State.cpp
+++ b/nes_emu/Nes_State.cpp
@@ -256,11 +256,15 @@ const char * Nes_State_::read_blocks( Nes_File_Reader& in )
 			break;
 			
 		case FOUR_CHAR('NTAB'):
+		{
+			uint8_t both [0x1000];
 			nametable_size = in.remain();
-			RETURN_ERR( in.read( nametable, 0x800 ) );
+			RETURN_ERR( in.read_block_data( both, sizeof both ) );
+			memcpy( nametable, both, 0x800 );
 			if ( nametable_size > 0x800 )
-				RETURN_ERR( in.read( chr, 0x800 ) );
+				memcpy( chr, both + 0x800, 0x800 );
 			break;
+		}
 			
 		case FOUR_CHAR('LRAM'):
 			ram_valid = true;
PATCH

# A switch dispatches every instruction through one indirect jump, which the
# branch predictor misses about once every four instructions. The patch gives
# each handler its own copy of the dispatch jump and reads RAM and ROM operands
# without a call. Emulated state and timing are unchanged.
patch -d "$build_root/core" -f -p1 <<'PATCH'
diff --git a/nes_emu/Nes_Cpu.cpp b/nes_emu/Nes_Cpu.cpp
index 1ebddd5..7e066eb 100644
--- a/nes_emu/Nes_Cpu.cpp
+++ b/nes_emu/Nes_Cpu.cpp
@@ -3,6 +3,13 @@
 
 #include "Nes_Cpu.h"
 
+// Each handler ends in its own copy of the dispatch jump, so the branch
+// predictor sees one indirect jump per handler. Cross-jumping and GCSE would
+// merge those copies back into one.
+#if defined (__GNUC__) && !defined (__clang__)
+	#pragma GCC optimize ("no-crossjumping", "no-gcse")
+#endif
+
 #include <string.h>
 #include <limits.h>
 #include "blargg_endian.h"
@@ -127,7 +134,8 @@ static const unsigned char clock_table [256] = {
 
 #define NOTE_READ( addr )       (effects += (unsigned) ((addr) - 0x2000) < 0x6000)
 #define READ_LIKELY_PPU( addr ) (NOTE_READ( addr ), NES_CPU_READ_PPU( this, (addr), (clock_count) ))
-#define READ( addr )            (NOTE_READ( addr ), NES_CPU_READ( this, (addr), (clock_count) ))
+#define READ( addr )            (NOTE_READ( addr ), !((addr) & 0xE000) ? (int) low_mem [(addr) & 0x7FF] : \
+		(addr) > 0x7FFF ? (int) *get_code( addr ) : NES_CPU_READ( this, (addr), (clock_count) ))
 #define WRITE( addr, data )     {effects++; NES_CPU_WRITE( this, (addr), (data), (clock_count) );}
 #define WRITE_LOW( addr, data ) (void) (effects++, READ_LOW( addr ) = (data))
 
@@ -215,23 +223,38 @@ Nes_Cpu::result_t Nes_Cpu::run( nes_time_t end )
 	int idle_a = 0, idle_x = 0, idle_y = 0, idle_sp = 0;
 	int idle_status = 0, idle_c = 0, idle_nz = 0;
 	
+	static void* const dispatch_table [256] = {&&op_0x00,&&op_0x01,&&op_default,&&op_0x03,&&op_0x04,&&op_0x05,&&op_0x06,&&op_0x07,&&op_0x08,&&op_0x09,&&op_0x0A,&&op_0x0B,&&op_0x0C,&&op_0x0D,&&op_0x0E,&&op_0x0F,&&op_0x10,&&op_0x11,&&op_default,&&op_0x13,&&op_0x14,&&op_0x15,&&op_0x16,&&op_0x17,&&op_0x18,&&op_0x19,&&op_0x1A,&&op_0x1B,&&op_0x1C,&&op_0x1D,&&op_0x1E,&&op_0x1F,&&op_0x20,&&op_0x21,&&op_default,&&op_0x23,&&op_0x24,&&op_0x25,&&op_0x26,&&op_0x27,&&op_0x28,&&op_0x29,&&op_0x2A,&&op_0x2B,&&op_0x2C,&&op_0x2D,&&op_0x2E,&&op_0x2F,&&op_0x30,&&op_0x31,&&op_default,&&op_0x33,&&op_0x34,&&op_0x35,&&op_0x36,&&op_0x37,&&op_0x38,&&op_0x39,&&op_0x3A,&&op_0x3B,&&op_0x3C,&&op_0x3D,&&op_0x3E,&&op_0x3F,&&op_0x40,&&op_0x41,&&op_default,&&op_0x43,&&op_0x44,&&op_0x45,&&op_0x46,&&op_0x47,&&op_0x48,&&op_0x49,&&op_0x4A,&&op_0x4B,&&op_0x4C,&&op_0x4D,&&op_0x4E,&&op_0x4F,&&op_0x50,&&op_0x51,&&op_default,&&op_0x53,&&op_0x54,&&op_0x55,&&op_0x56,&&op_0x57,&&op_0x58,&&op_0x59,&&op_0x5A,&&op_0x5B,&&op_0x5C,&&op_0x5D,&&op_0x5E,&&op_0x5F,&&op_0x60,&&op_0x61,&&op_default,&&op_0x63,&&op_0x64,&&op_0x65,&&op_0x66,&&op_0x67,&&op_0x68,&&op_0x69,&&op_0x6A,&&op_0x6B,&&op_0x6C,&&op_0x6D,&&op_0x6E,&&op_0x6F,&&op_0x70,&&op_0x71,&&op_default,&&op_0x73,&&op_0x74,&&op_0x75,&&op_0x76,&&op_0x77,&&op_0x78,&&op_0x79,&&op_0x7A,&&op_0x7B,&&op_0x7C,&&op_0x7D,&&op_0x7E,&&op_0x7F,&&op_0x80,&&op_0x81,&&op_0x82,&&op_0x83,&&op_0x84,&&op_0x85,&&op_0x86,&&op_0x87,&&op_0x88,&&op_0x89,&&op_0x8A,&&op_default,&&op_0x8C,&&op_0x8D,&&op_0x8E,&&op_0x8F,&&op_0x90,&&op_0x91,&&op_default,&&op_0x93,&&op_0x94,&&op_0x95,&&op_0x96,&&op_0x97,&&op_0x98,&&op_0x99,&&op_0x9A,&&op_0x9B,&&op_0x9C,&&op_0x9D,&&op_0x9E,&&op_0x9F,&&op_0xA0,&&op_0xA1,&&op_0xA2,&&op_0xA3,&&op_0xA4,&&op_0xA5,&&op_0xA6,&&op_0xA7,&&op_0xA8,&&op_0xA9,&&op_0xAA,&&op_0xAB,&&op_0xAC,&&op_0xAD,&&op_0xAE,&&op_0xAF,&&op_0xB0,&&op_0xB1,&&op_default,&&op_0xB3,&&op_0xB4,&&op_0xB5,&&op_0xB6,&&op_0xB7,&&op_0xB8,&&op_0xB9,&&op_0xBA,&&op_0xBB,&&op_0xBC,&&op_0xBD,&&op_0xBE,&&op_0xBF,&&op_0xC0,&&op_0xC1,&&op_0xC2,&&op_0xC3,&&op_0xC4,&&op_0xC5,&&op_0xC6,&&op_0xC7,&&op_0xC8,&&op_0xC9,&&op_0xCA,&&op_0xCB,&&op_0xCC,&&op_0xCD,&&op_0xCE,&&op_0xCF,&&op_0xD0,&&op_0xD1,&&op_default,&&op_0xD3,&&op_0xD4,&&op_0xD5,&&op_0xD6,&&op_0xD7,&&op_0xD8,&&op_0xD9,&&op_0xDA,&&op_0xDB,&&op_0xDC,&&op_0xDD,&&op_0xDE,&&op_0xDF,&&op_0xE0,&&op_0xE1,&&op_0xE2,&&op_0xE3,&&op_0xE4,&&op_0xE5,&&op_0xE6,&&op_0xE7,&&op_0xE8,&&op_0xE9,&&op_0xEA,&&op_0xEB,&&op_0xEC,&&op_0xED,&&op_0xEE,&&op_0xEF,&&op_0xF0,&&op_0xF1,&&op_0xF2,&&op_0xF3,&&op_0xF4,&&op_0xF5,&&op_0xF6,&&op_0xF7,&&op_0xF8,&&op_0xF9,&&op_0xFA,&&op_0xFB,&&op_0xFC,&&op_0xFD,&&op_0xFE,&&op_0xFF};
+	uint8_t const* page;
+	unsigned opcode;
+	unsigned data;
+	
+#define DISPATCH {                                  \
+	page = code_map [pc >> page_bits];              \
+	opcode = page [pc];                             \
+	pc++;                                           \
+	if ( clock_count >= clock_limit )               \
+		goto stop;                                  \
+	clock_count += clock_table [opcode];            \
+	data = page [pc];                               \
+	goto *dispatch_table [opcode];                  \
+}
+	
 	goto loop;
 dec_clock_loop:
 	clock_count--;
 loop:
 	
-	uint8_t const* page = code_map [pc >> page_bits];
-	unsigned opcode = page [pc];
+	page = code_map [pc >> page_bits];
+	opcode = page [pc];
 	pc++;
 	
 	if ( clock_count >= clock_limit )
 		goto stop;
 	
 	clock_count += clock_table [opcode];
-	unsigned data;
 	data = page [pc];
 	
-	switch ( opcode )
+	goto *dispatch_table [opcode];
 	{
 
 // Macros
@@ -244,7 +267,7 @@ loop:
 
 #define HANDLE_PAGE_CROSSING( lsb ) clock_count += (lsb) >> 8;
 
-#define INC_DEC_XY( reg, n ) reg = uint8_t (nz = reg + n); goto loop;
+#define INC_DEC_XY( reg, n ) reg = uint8_t (nz = reg + n); DISPATCH
 
 #define IND_Y(r,c) {                                            \
 		int temp = READ_LOW( data ) + y;                        \
@@ -259,22 +282,22 @@ loop:
 		data = 0x100 * READ_LOW( uint8_t (temp + 1) ) + READ_LOW( uint8_t (temp) ); \
 	}
 	
-#define ARITH_ADDR_MODES( op )          \
-case op - 0x04: /* (ind,x) */           \
+#define ARITH_ADDR_MODES( op, ix, iy, zx, ay, ax, ab, im )          \
+op_##ix: /* (ind,x) */           \
 	IND_X                               \
 	goto ptr##op;                       \
-case op + 0x0C: /* (ind),y */           \
+op_##iy: /* (ind),y */           \
 	IND_Y(true,true)                    \
 	goto ptr##op;                       \
-case op + 0x10: /* zp,X */              \
+op_##zx: /* zp,X */              \
 	data = uint8_t (data + x);          \
-case op + 0x00: /* zp */                \
+op_##op: /* zp */                \
 	data = READ_LOW( data );            \
 	goto imm##op;                       \
-case op + 0x14: /* abs,Y */             \
+op_##ay: /* abs,Y */             \
 	data += y;                          \
 	goto ind##op;                       \
-case op + 0x18: /* abs,X */             \
+op_##ax: /* abs,X */             \
 	data += x;                          \
 ind##op: {                              \
 	HANDLE_PAGE_CROSSING( data );       \
@@ -284,27 +307,27 @@ ind##op: {                              \
 		 READ( data - 0x100 );          \
 	goto ptr##op;                       \
 }                                       \
-case op + 0x08: /* abs */               \
+op_##ab: /* abs */               \
 	ADD_PAGE                            \
 ptr##op:                                \
 	data = READ( data );                \
-case op + 0x04: /* imm */               \
+op_##im: /* imm */               \
 imm##op:                                \
 
-#define ARITH_ADDR_MODES_PTR( op )      \
-case op - 0x04: /* (ind,x) */           \
+#define ARITH_ADDR_MODES_PTR( op, ix, iy, zx, ay, ax, ab, im )      \
+op_##ix: /* (ind,x) */           \
 	IND_X                               \
 	goto imm##op;                       \
-case op + 0x0C:                         \
+op_##iy:                         \
 	IND_Y(false,false)                  \
 	goto imm##op;                       \
-case op + 0x10: /* zp,X */              \
+op_##zx: /* zp,X */              \
 	data = uint8_t (data + x);          \
 	goto imm##op;                       \
-case op + 0x14: /* abs,Y */             \
+op_##ay: /* abs,Y */             \
 	data += y;                          \
 	goto ind##op;                       \
-case op + 0x18: /* abs,X */             \
+op_##ax: /* abs,X */             \
 	data += x;                          \
 ind##op: {                              \
 	int temp = data;                    \
@@ -312,9 +335,9 @@ ind##op: {                              \
 	READ( data - ( temp & 0x100 ) );    \
 	goto imm##op;                       \
 }                                       \
-case op + 0x08: /* abs */               \
+op_##ab: /* abs */               \
 	ADD_PAGE                            \
-case op + 0x00: /* zp */                \
+op_##op: /* zp */                \
 imm##op:                                \
 
 #define BRANCH( cond )      \
@@ -328,89 +351,89 @@ imm##op:                                \
 	clock_count += (extra_clock >> 8) & 1;  \
 	if ( offset < 0 )   \
 		IDLE_CHECK()    \
-	goto loop;          \
+	DISPATCH          \
 }
 
 // Often-Used
 
-	case 0xB5: // LDA zp,x
+	op_0xB5: // LDA zp,x
 		data = uint8_t (data + x);
-	case 0xA5: // LDA zp
+	op_0xA5: // LDA zp
 		a = nz = READ_LOW( data );
 		pc++;
-		goto loop;
+		DISPATCH
 	
-	case 0xD0: // BNE
+	op_0xD0: // BNE
 		BRANCH( (uint8_t) nz );
 	
-	case 0x20: { // JSR
+	op_0x20: { // JSR
 		int temp = pc + 1;
 		pc = GET_OPERAND16( pc );
 		WRITE_LOW( 0x100 | (sp - 1), temp >> 8 );
 		sp = (sp - 2) | 0x100;
 		WRITE_LOW( sp, temp );
-		goto loop;
+		DISPATCH
 	}
 	
-	case 0x4C: { // JMP abs
+	op_0x4C: { // JMP abs
 		unsigned from = pc;
 		pc = GET_OPERAND16( pc );
 		if ( pc < from )
 			IDLE_CHECK()
-		goto loop;
+		DISPATCH
 	}
 	
-	case 0xE8: INC_DEC_XY( x, 1 )  // INX
+	op_0xE8: INC_DEC_XY( x, 1 )  // INX
 	
-	case 0x10: // BPL
+	op_0x10: // BPL
 		BRANCH( !IS_NEG )
 	
-	ARITH_ADDR_MODES( 0xC5 ) // CMP
+	ARITH_ADDR_MODES( 0xC5, 0xC1, 0xD1, 0xD5, 0xD9, 0xDD, 0xCD, 0xC9 ) // CMP
 		nz = a - data;
 		pc++;
 		c = ~nz;
 		nz &= 0xFF;
-		goto loop;
+		DISPATCH
 	
-	case 0x30: // BMI
+	op_0x30: // BMI
 		BRANCH( IS_NEG )
 	
-	case 0xF0: // BEQ
+	op_0xF0: // BEQ
 		BRANCH( !(uint8_t) nz );
 	
-	case 0x95: // STA zp,x
+	op_0x95: // STA zp,x
 		data = uint8_t (data + x);
-	case 0x85: // STA zp
+	op_0x85: // STA zp
 		pc++;
 		WRITE_LOW( data, a );
-		goto loop;
+		DISPATCH
 	
-	case 0xC8: INC_DEC_XY( y, 1 )  // INY
+	op_0xC8: INC_DEC_XY( y, 1 )  // INY
 
-	case 0xA8: // TAY
+	op_0xA8: // TAY
 		y = a;
-	case 0x98: // TYA
+	op_0x98: // TYA
 		a = nz = y;
-		goto loop;
+		DISPATCH
 	
-	case 0xAD:{// LDA abs
+	op_0xAD:{// LDA abs
 		unsigned addr = GET_ADDR();
 		pc += 2;
 		a = nz = READ_LIKELY_PPU( addr );
-		goto loop;
+		DISPATCH
 	}
 	
-	case 0x60: // RTS
+	op_0x60: // RTS
 		pc = 1 + READ_LOW( sp );
 		pc += READ_LOW( 0x100 | (sp - 0xFF) ) * 0x100;
 		sp = (sp - 0xFE) | 0x100;
-		goto loop;
+		DISPATCH
 
-	case 0x99: // STA abs,Y
+	op_0x99: // STA abs,Y
 		data += y;
 		goto sta_ind_common;
 	
-	case 0x9D: // STA abs,X
+	op_0x9D: // STA abs,X
 		data += x;
 	sta_ind_common: {
 		int temp = data;
@@ -418,25 +441,25 @@ imm##op:                                \
 		READ( data - ( temp & 0x100 ) );
 		goto sta_ptr;
 	}
-	case 0x8D: // STA abs
+	op_0x8D: // STA abs
 		ADD_PAGE
 	sta_ptr:
 		pc++;
 		WRITE( data, a );
-		goto loop;
+		DISPATCH
 	
-	case 0xA9: // LDA #imm
+	op_0xA9: // LDA #imm
 		pc++;
 		a = data;
 		nz = data;
-		goto loop;
+		DISPATCH
 	
 	// optimization of most commonly used memory read instructions
 	
-	case 0xB9:// LDA abs,Y
+	op_0xB9:// LDA abs,Y
 		data += y;
 		data -= x;
-	case 0xBD:{// LDA abs,X
+	op_0xBD:{// LDA abs,X
 		pc++;
 		unsigned msb = GET_OPERAND( pc );
 		data += x;
@@ -447,14 +470,14 @@ imm##op:                                \
 		data += msb * 0x100;
 		a = nz = READ_PROG( uint16_t( data ) );
 		if ( (unsigned) (data - 0x2000) >= 0x6000 )
-			goto loop;
+			DISPATCH
 		if ( temp & 0x100 )
 			READ( data - 0x100 );
 		a = nz = READ( data );
-		goto loop;
+		DISPATCH
 	}
 	
-	case 0xB1:{// LDA (ind),Y
+	op_0xB1:{// LDA (ind),Y
 		unsigned msb = READ_LOW( (uint8_t) (data + 1) );
 		data = READ_LOW( data ) + y;
 		// indexed common
@@ -464,203 +487,203 @@ imm##op:                                \
 		data += msb * 0x100;
 		a = nz = READ_PROG( uint16_t( data ) );
 		if ( (unsigned) (data - 0x2000) >= 0x6000 )
-			goto loop;
+			DISPATCH
 		if ( temp & 0x100 )
 			READ( data - 0x100 );
 		a = nz = READ( data );
-		goto loop;
+		DISPATCH
 	}
 	
-	case 0xA1: // LDA (ind,X)
+	op_0xA1: // LDA (ind,X)
 		IND_X
 		a = nz = READ( data );
 		pc++;
-		goto loop;
+		DISPATCH
 	
 // Branch
 
-	case 0x50: // BVC
+	op_0x50: // BVC
 		BRANCH( !(status & st_v) )
 	
-	case 0x70: // BVS
+	op_0x70: // BVS
 		BRANCH( status & st_v )
 	
-	case 0xB0: // BCS
+	op_0xB0: // BCS
 		BRANCH( c & 0x100 )
 	
-	case 0x90: // BCC
+	op_0x90: // BCC
 		BRANCH( !(c & 0x100) )
 	
 // Load/store
 	
-	case 0x94: // STY zp,x
+	op_0x94: // STY zp,x
 		data = uint8_t (data + x);
-	case 0x84: // STY zp
+	op_0x84: // STY zp
 		pc++;
 		WRITE_LOW( data, y );
-		goto loop;
+		DISPATCH
 	
-	case 0x96: // STX zp,y
+	op_0x96: // STX zp,y
 		data = uint8_t (data + y);
-	case 0x86: // STX zp
+	op_0x86: // STX zp
 		pc++;
 		WRITE_LOW( data, x );
-		goto loop;
+		DISPATCH
 	
-	case 0xB6: // LDX zp,y
+	op_0xB6: // LDX zp,y
 		data = uint8_t (data + y);
-	case 0xA6: // LDX zp
+	op_0xA6: // LDX zp
 		data = READ_LOW( data );
-	case 0xA2: // LDX #imm
+	op_0xA2: // LDX #imm
 		pc++;
 		x = data;
 		nz = data;
-		goto loop;
+		DISPATCH
 	
-	case 0xB4: // LDY zp,x
+	op_0xB4: // LDY zp,x
 		data = uint8_t (data + x);
-	case 0xA4: // LDY zp
+	op_0xA4: // LDY zp
 		data = READ_LOW( data );
-	case 0xA0: // LDY #imm
+	op_0xA0: // LDY #imm
 		pc++;
 		y = data;
 		nz = data;
-		goto loop;
+		DISPATCH
 	
-	case 0x91: // STA (ind),Y
+	op_0x91: // STA (ind),Y
 		IND_Y(false,false)
 		goto sta_ptr;
 	
-	case 0x81: // STA (ind,X)
+	op_0x81: // STA (ind,X)
 		IND_X
 		goto sta_ptr;
 	
-	case 0xBC: // LDY abs,X
+	op_0xBC: // LDY abs,X
 		data += x;
 		HANDLE_PAGE_CROSSING( data );
-	case 0xAC:{// LDY abs
+	op_0xAC:{// LDY abs
 		pc++;
 		unsigned addr = data + 0x100 * GET_OPERAND( pc );
 		if ( data & 0x100 )
 			READ( addr - 0x100 );
 		pc++;
 		y = nz = READ( addr );
-		goto loop;
+		DISPATCH
 	}
 	
-	case 0xBE: // LDX abs,y
+	op_0xBE: // LDX abs,y
 		data += y;
 		HANDLE_PAGE_CROSSING( data );
-	case 0xAE:{// LDX abs
+	op_0xAE:{// LDX abs
 		pc++;
 		unsigned addr = data + 0x100 * GET_OPERAND( pc );
 		pc++;
 		if ( data & 0x100 )
 			READ( addr - 0x100 );
 		x = nz = READ( addr );
-		goto loop;
+		DISPATCH
 	}
 	
 	{
 		int temp;
-	case 0x8C: // STY abs
+	op_0x8C: // STY abs
 		temp = y;
 		goto store_abs;
 	
-	case 0x8E: // STX abs
+	op_0x8E: // STX abs
 		temp = x;
 	store_abs:
 		unsigned addr = GET_ADDR();
 		WRITE( addr, temp );
 		pc += 2;
-		goto loop;
+		DISPATCH
 	}
 
 // Compare
 
-	case 0xEC:{// CPX abs
+	op_0xEC:{// CPX abs
 		unsigned addr = GET_ADDR();
 		pc++;
 		data = READ( addr );
 		goto cpx_data;
 	}
 	
-	case 0xE4: // CPX zp
+	op_0xE4: // CPX zp
 		data = READ_LOW( data );
-	case 0xE0: // CPX #imm
+	op_0xE0: // CPX #imm
 	cpx_data:
 		nz = x - data;
 		pc++;
 		c = ~nz;
 		nz &= 0xFF;
-		goto loop;
+		DISPATCH
 	
-	case 0xCC:{// CPY abs
+	op_0xCC:{// CPY abs
 		unsigned addr = GET_ADDR();
 		pc++;
 		data = READ( addr );
 		goto cpy_data;
 	}
 	
-	case 0xC4: // CPY zp
+	op_0xC4: // CPY zp
 		data = READ_LOW( data );
-	case 0xC0: // CPY #imm
+	op_0xC0: // CPY #imm
 	cpy_data:
 		nz = y - data;
 		pc++;
 		c = ~nz;
 		nz &= 0xFF;
-		goto loop;
+		DISPATCH
 	
 // Logical
 
-	ARITH_ADDR_MODES( 0x25 ) // AND
+	ARITH_ADDR_MODES( 0x25, 0x21, 0x31, 0x35, 0x39, 0x3D, 0x2D, 0x29 ) // AND
 		nz = (a &= data);
 		pc++;
-		goto loop;
+		DISPATCH
 	
-	ARITH_ADDR_MODES( 0x45 ) // EOR
+	ARITH_ADDR_MODES( 0x45, 0x41, 0x51, 0x55, 0x59, 0x5D, 0x4D, 0x49 ) // EOR
 		nz = (a ^= data);
 		pc++;
-		goto loop;
+		DISPATCH
 	
-	ARITH_ADDR_MODES( 0x05 ) // ORA
+	ARITH_ADDR_MODES( 0x05, 0x01, 0x11, 0x15, 0x19, 0x1D, 0x0D, 0x09 ) // ORA
 		nz = (a |= data);
 		pc++;
-		goto loop;
+		DISPATCH
 	
-	case 0x2C:{// BIT abs
+	op_0x2C:{// BIT abs
 		unsigned addr = GET_ADDR();
 		pc += 2;
 		status &= ~st_v;
 		nz = READ_LIKELY_PPU( addr );
 		status |= nz & st_v;
 		if ( a & nz )
-			goto loop;
+			DISPATCH
 		// result must be zero, even if N bit is set
 		nz = nz << 4 & 0x800;
-		goto loop;
+		DISPATCH
 	}
 	
-	case 0x24: // BIT zp
+	op_0x24: // BIT zp
 		nz = READ_LOW( data );
 		pc++;
 		status &= ~st_v;
 		status |= nz & st_v;
 		if ( a & nz )
-			goto loop;
+			DISPATCH
 		// result must be zero, even if N bit is set
 		nz = nz << 4 & 0x800;
-		goto loop;
+		DISPATCH
 		
 // Add/subtract
 
-	ARITH_ADDR_MODES( 0xE5 ) // SBC
-	case 0xEB: // unofficial equivalent
+	ARITH_ADDR_MODES( 0xE5, 0xE1, 0xF1, 0xF5, 0xF9, 0xFD, 0xED, 0xE9 ) // SBC
+	op_0xEB: // unofficial equivalent
 		data ^= 0xFF;
 		goto adc_imm;
 	
-	ARITH_ADDR_MODES( 0x65 ) // ADC
+	ARITH_ADDR_MODES( 0x65, 0x61, 0x71, 0x75, 0x79, 0x7D, 0x6D, 0x69 ) // ADC
 	adc_imm: {
 		int carry = (c >> 8) & 1;
 		int ov = (a ^ 0x80) + carry + (int8_t) data; // sign-extend
@@ -669,45 +692,45 @@ imm##op:                                \
 		c = nz = a + data + carry;
 		pc++;
 		a = (uint8_t) nz;
-		goto loop;
+		DISPATCH
 	}
 	
 // Shift/rotate
 
-	case 0x4A: // LSR A
+	op_0x4A: // LSR A
 	lsr_a:
 		c = 0;
-	case 0x6A: // ROR A
+	op_0x6A: // ROR A
 		nz = (c >> 1) & 0x80; // could use bit insert macro here
 		c = a << 8;
 		nz |= a >> 1;
 		a = nz;
-		goto loop;
+		DISPATCH
 
-	case 0x0A: // ASL A
+	op_0x0A: // ASL A
 		nz = a << 1;
 		c = nz;
 		a = (uint8_t) nz;
-		goto loop;
+		DISPATCH
 
-	case 0x2A: { // ROL A
+	op_0x2A: { // ROL A
 		nz = a << 1;
 		int temp = (c >> 8) & 1;
 		c = nz;
 		nz |= temp;
 		a = (uint8_t) nz;
-		goto loop;
+		DISPATCH
 	}
 	
-	case 0x3E: // ROL abs,X
+	op_0x3E: // ROL abs,X
 		data += x;
 		goto rol_abs;
 	
-	case 0x1E: // ASL abs,X
+	op_0x1E: // ASL abs,X
 		data += x;
-	case 0x0E: // ASL abs
+	op_0x0E: // ASL abs
 		c = 0;
-	case 0x2E: // ROL abs
+	op_0x2E: // ROL abs
 	rol_abs: {
 		int temp = data;
 		ADD_PAGE
@@ -719,17 +742,17 @@ imm##op:                                \
     rotate_common:
 		pc++;
 		WRITE( data, (uint8_t) nz );
-		goto loop;
+		DISPATCH
 
-	case 0x7E: // ROR abs,X
+	op_0x7E: // ROR abs,X
 		data += x;
 		goto ror_abs;
 	
-	case 0x5E: // LSR abs,X
+	op_0x5E: // LSR abs,X
 		data += x;
-	case 0x4E: // LSR abs
+	op_0x4E: // LSR abs
 		c = 0;
-	case 0x6E: // ROR abs
+	op_0x6E: // ROR abs
 	ror_abs: {
 		int temp = data;
 		ADD_PAGE
@@ -740,15 +763,15 @@ imm##op:                                \
 		goto rotate_common;
 	}
 	
-	case 0x76: // ROR zp,x
+	op_0x76: // ROR zp,x
 		data = uint8_t (data + x);
 		goto ror_zp;
 	
-	case 0x56: // LSR zp,x
+	op_0x56: // LSR zp,x
 		data = uint8_t (data + x);
-	case 0x46: // LSR zp
+	op_0x46: // LSR zp
 		c = 0;
-	case 0x66: // ROR zp
+	op_0x66: // ROR zp
 	ror_zp: {
 		int temp = READ_LOW( data );
 		nz = ((c >> 1) & 0x80) | (temp >> 1);
@@ -756,15 +779,15 @@ imm##op:                                \
 		goto write_nz_zp;
 	}
 	
-	case 0x36: // ROL zp,x
+	op_0x36: // ROL zp,x
 		data = uint8_t (data + x);
 		goto rol_zp;
 	
-	case 0x16: // ASL zp,x
+	op_0x16: // ASL zp,x
 		data = uint8_t (data + x);
-	case 0x06: // ASL zp
+	op_0x06: // ASL zp
 		c = 0;
-	case 0x26: // ROL zp
+	op_0x26: // ROL zp
 	rol_zp:
 		nz = (c >> 8) & 1;
 		nz |= (c = READ_LOW( data ) << 1);
@@ -772,48 +795,48 @@ imm##op:                                \
 	
 // Increment/decrement
 
-	case 0xCA: INC_DEC_XY( x, -1 ) // DEX
+	op_0xCA: INC_DEC_XY( x, -1 ) // DEX
 	
-	case 0x88: INC_DEC_XY( y, -1 ) // DEY
+	op_0x88: INC_DEC_XY( y, -1 ) // DEY
 	
-	case 0xF6: // INC zp,x
+	op_0xF6: // INC zp,x
 		data = uint8_t (data + x);
-	case 0xE6: // INC zp
+	op_0xE6: // INC zp
 		nz = 1;
 		goto add_nz_zp;
 	
-	case 0xD6: // DEC zp,x
+	op_0xD6: // DEC zp,x
 		data = uint8_t (data + x);
-	case 0xC6: // DEC zp
+	op_0xC6: // DEC zp
 		nz = -1;
 	add_nz_zp:
 		nz += READ_LOW( data );
 	write_nz_zp:
 		pc++;
 		WRITE_LOW( data, nz );
-		goto loop;
+		DISPATCH
 	
-	case 0xFE: { // INC abs,x
+	op_0xFE: { // INC abs,x
 		int temp = data + x;
 		data = x + GET_ADDR();
 		READ( data - ( temp & 0x100 ) );
 		goto inc_ptr;
 	}
 	
-	case 0xEE: // INC abs
+	op_0xEE: // INC abs
 		data = GET_ADDR();
 	inc_ptr:
 		nz = 1;
 		goto inc_common;
 	
-	case 0xDE: { // DEC abs,x
+	op_0xDE: { // DEC abs,x
 		int temp = data + x;
 		data = x + GET_ADDR();
 		READ( data - ( temp & 0x100 ) );
 		goto dec_ptr;
 	}
 	
-	case 0xCE: // DEC abs
+	op_0xCE: // DEC abs
 		data = GET_ADDR();
 	dec_ptr:
 		nz = -1;
@@ -823,37 +846,37 @@ imm##op:                                \
 		nz += temp;
 		pc += 2;
 		WRITE( data, (uint8_t) nz );
-		goto loop;
+		DISPATCH
 	}
 		
 // Transfer
 
-	case 0xAA: // TAX
+	op_0xAA: // TAX
 		x = a;
-	case 0x8A: // TXA
+	op_0x8A: // TXA
 		a = nz = x;
-		goto loop;
+		DISPATCH
 
-	case 0x9A: // TXS
+	op_0x9A: // TXS
 		SET_SP( x ); // verified (no flag change)
-		goto loop;
+		DISPATCH
 	
-	case 0xBA: // TSX
+	op_0xBA: // TSX
 		x = nz = GET_SP();
-		goto loop;
+		DISPATCH
 	
 // Stack
 	
-	case 0x48: // PHA
+	op_0x48: // PHA
 		PUSH( a ); // verified
-		goto loop;
+		DISPATCH
 		
-	case 0x68: // PLA
+	op_0x68: // PLA
 		a = nz = READ_LOW( sp );
 		sp = (sp - 0xFF) | 0x100;
-		goto loop;
+		DISPATCH
 		
-	case 0x40: // RTI
+	op_0x40: // RTI
 		{
 			int temp = READ_LOW( sp );
 			pc   = READ_LOW( 0x100 | (sp - 0xFF) );
@@ -863,45 +886,45 @@ imm##op:                                \
 			SET_STATUS( temp );
 		}
 		if ( !((data ^ status) & st_i) )
-			goto loop; // I flag didn't change
+			DISPATCH // I flag didn't change
 	i_flag_changed:
 		effects++;
 		this->r.status = status; // update externally-visible I flag
 		// update clock_limit based on modified I flag
 		clock_limit = end_time_;
 		if ( end_time_ <= irq_time_ )
-			goto loop;
+			DISPATCH
 		if ( status & st_i )
-			goto loop;
+			DISPATCH
 		clock_limit = irq_time_;
-		goto loop;
+		DISPATCH
 	
-	case 0x28:{// PLP
+	op_0x28:{// PLP
 		int temp = READ_LOW( sp );
 		sp = (sp - 0xFF) | 0x100;
 		data = status;
 		SET_STATUS( temp );
 		if ( !((data ^ status) & st_i) )
-			goto loop; // I flag didn't change
+			DISPATCH // I flag didn't change
 		if ( !(status & st_i) )
 			goto handle_cli;
 		goto handle_sei;
 	}
 	
-	case 0x08: { // PHP
+	op_0x08: { // PHP
 		int temp;
 		CALC_STATUS( temp );
 		PUSH( temp | st_b | st_r );
-		goto loop;
+		DISPATCH
 	}
 	
-	case 0x6C: // JMP (ind)
+	op_0x6C: // JMP (ind)
 		data = GET_ADDR();
 		pc = READ( data );
 		pc |= READ( (data & 0xFF00) | ((data + 1) & 0xFF) ) << 8;
-		goto loop;
+		DISPATCH
 	
-	case 0x00: { // BRK
+	op_0x00: { // BRK
 		pc++;
 		WRITE_LOW( 0x100 | (sp - 1), pc >> 8 );
 		WRITE_LOW( 0x100 | (sp - 2), pc );
@@ -916,29 +939,29 @@ imm##op:                                \
 	
 // Flags
 
-	case 0x38: // SEC
+	op_0x38: // SEC
 		c = ~0;
-		goto loop;
+		DISPATCH
 	
-	case 0x18: // CLC
+	op_0x18: // CLC
 		c = 0;
-		goto loop;
+		DISPATCH
 		
-	case 0xB8: // CLV
+	op_0xB8: // CLV
 		status &= ~st_v;
-		goto loop;
+		DISPATCH
 	
-	case 0xD8: // CLD
+	op_0xD8: // CLD
 		status &= ~st_d;
-		goto loop;
+		DISPATCH
 	
-	case 0xF8: // SED
+	op_0xF8: // SED
 		status |= st_d;
-		goto loop;
+		DISPATCH
 	
-	case 0x58: // CLI
+	op_0x58: // CLI
 		if ( !(status & st_i) )
-			goto loop;
+			DISPATCH
 		status &= ~st_i;
 	handle_cli:
 		effects++;
@@ -946,31 +969,31 @@ imm##op:                                \
 		if ( clock_count < end_time_ )
 		{
 			if ( end_time_ <= irq_time_ )
-				goto loop; // irq is later
+				DISPATCH // irq is later
 			if ( clock_count >= irq_time_ )
 				irq_time_ = clock_count + 1; // delay IRQ until after next instruction
 			clock_limit = irq_time_;
-			goto loop;
+			DISPATCH
 		}
 		// execution is stopping now, so delayed CLI must be handled by caller
 		result = result_cli;
 		goto end;
 		
-	case 0x78: // SEI
+	op_0x78: // SEI
 		if ( status & st_i )
-			goto loop;
+			DISPATCH
 		status |= st_i;
 	handle_sei:
 		effects++;
 		this->r.status = status; // update externally-visible I flag
 		clock_limit = end_time_;
 		if ( clock_count < irq_time_ )
-			goto loop;
+			DISPATCH
 		result = result_sei; // IRQ will occur now, even though I flag is set
 		goto end;
 
 // Unofficial
-	case 0x1C: case 0x3C: case 0x5C: case 0x7C: case 0xDC: case 0xFC: { // SKW
+	op_0x1C: op_0x3C: op_0x5C: op_0x7C: op_0xDC: op_0xFC: { // SKW
 		data += x;
 		HANDLE_PAGE_CROSSING( data );
 		int addr = GET_ADDR() + x;
@@ -978,15 +1001,15 @@ imm##op:                                \
 			READ( addr - 0x100 );
 		READ( addr );
 	}
-	case 0x0C: // SKW
+	op_0x0C: // SKW
 		pc++;
-	case 0x74: case 0x04: case 0x14: case 0x34: case 0x44: case 0x54: case 0x64: // SKB
-	case 0x80: case 0x82: case 0x89: case 0xC2: case 0xD4: case 0xE2: case 0xF4:
+	op_0x74: op_0x04: op_0x14: op_0x34: op_0x44: op_0x54: op_0x64: // SKB
+	op_0x80: op_0x82: op_0x89: op_0xC2: op_0xD4: op_0xE2: op_0xF4:
 		pc++;
-	case 0xEA: case 0x1A: case 0x3A: case 0x5A: case 0x7A: case 0xDA: case 0xFA: // NOP
-		goto loop;
+	op_0xEA: op_0x1A: op_0x3A: op_0x5A: op_0x7A: op_0xDA: op_0xFA: // NOP
+		DISPATCH
 
-	ARITH_ADDR_MODES_PTR( 0xC7 ) // DCP
+	ARITH_ADDR_MODES_PTR( 0xC7, 0xC3, 0xD3, 0xD7, 0xDB, 0xDF, 0xCF, 0xCB ) // DCP
 		WRITE( data, nz = READ( data ) );
 		nz = uint8_t( nz - 1 );
 		WRITE( data, nz );
@@ -994,16 +1017,16 @@ imm##op:                                \
 		nz = a - nz;
 		c = ~nz;
 		nz &= 0xFF;
-		goto loop;
+		DISPATCH
 
-	ARITH_ADDR_MODES_PTR( 0xE7 ) // ISC
+	ARITH_ADDR_MODES_PTR( 0xE7, 0xE3, 0xF3, 0xF7, 0xFB, 0xFF, 0xEF, 0xEB ) // ISC
 		WRITE( data, nz = READ( data ) );
 		nz = uint8_t( nz + 1 );
 		WRITE( data, nz );
 		data = nz ^ 0xFF;
 		goto adc_imm;
 
-	ARITH_ADDR_MODES_PTR( 0x27 ) { // RLA
+	ARITH_ADDR_MODES_PTR( 0x27, 0x23, 0x33, 0x37, 0x3B, 0x3F, 0x2F, 0x2B ) { // RLA
 		WRITE( data, nz = READ( data ) );
 		int temp = c;
 		c = nz << 1;
@@ -1011,10 +1034,10 @@ imm##op:                                \
 		WRITE( data, nz );
 		pc++;
 		nz = a &= nz;
-		goto loop;
+		DISPATCH
 	}
 
-	ARITH_ADDR_MODES_PTR( 0x67 ) { // RRA
+	ARITH_ADDR_MODES_PTR( 0x67, 0x63, 0x73, 0x77, 0x7B, 0x7F, 0x6F, 0x6B ) { // RRA
 		int temp;
 		WRITE( data, temp = READ( data ) );
 		nz = ((c >> 1) & 0x80) | (temp >> 1);
@@ -1024,66 +1047,66 @@ imm##op:                                \
 		goto adc_imm;
 	}
 
-	ARITH_ADDR_MODES_PTR( 0x07 ) // SLO
+	ARITH_ADDR_MODES_PTR( 0x07, 0x03, 0x13, 0x17, 0x1B, 0x1F, 0x0F, 0x0B ) // SLO
 		WRITE( data, nz = READ( data ) );
 		c = nz << 1;
 		nz = uint8_t( c );
 		WRITE( data, nz );
 		nz = (a |= nz);
 		pc++;
-		goto loop;
+		DISPATCH
 
-	ARITH_ADDR_MODES_PTR( 0x47 ) // SRE
+	ARITH_ADDR_MODES_PTR( 0x47, 0x43, 0x53, 0x57, 0x5B, 0x5F, 0x4F, 0x4B ) // SRE
 		WRITE( data, nz = READ( data ) );
 		c = nz << 8;
 		nz >>= 1;
 		WRITE( data, nz );
 		nz = a ^= nz;
 		pc++;
-		goto loop;
+		DISPATCH
 
-	case 0x4B: // ALR
+	op_0x4B: // ALR
 		nz = (a &= data);
 		pc++;
 		goto lsr_a;
 
-	case 0x0B: // ANC
-	case 0x2B:
+	op_0x0B: // ANC
+	op_0x2B:
 		nz = a &= data;
 		c = a << 1;
 		pc++;
-		goto loop;
+		DISPATCH
 
-	case 0x6B: // ARR
+	op_0x6B: // ARR
 		nz = a = uint8_t( ( ( data & a ) >> 1 ) | ( ( c >> 1 ) & 0x80 ) );
 		c = a << 2;
 		status = ( status & ~st_v ) | ( ( a ^ a << 1 ) & st_v );
 		pc++;
-		goto loop;
+		DISPATCH
 
-	case 0xAB: // LXA
+	op_0xAB: // LXA
 		a = data;
 		x = data;
 		nz = data;
 		pc++;
-		goto loop;
+		DISPATCH
 
-	case 0xA3: // LAX
+	op_0xA3: // LAX
 		IND_X
 		goto lax_ptr;
 
-	case 0xB3:
+	op_0xB3:
 		IND_Y(true,true)
 		goto lax_ptr;
 
-	case 0xB7:
+	op_0xB7:
 		data = uint8_t (data + y);
 
-	case 0xA7:
+	op_0xA7:
 		data = READ_LOW( data );
 		goto lax_imm;
 
-	case 0xBF: {
+	op_0xBF: {
 		data += y;
 		HANDLE_PAGE_CROSSING( data );
 		int temp = data;
@@ -1093,7 +1116,7 @@ imm##op:                                \
 		goto lax_ptr;
 	}
 
-	case 0xAF:
+	op_0xAF:
 		ADD_PAGE
 
 	lax_ptr:
@@ -1101,49 +1124,49 @@ imm##op:                                \
 	lax_imm:
 		nz = x = a = data;
 		pc++;
-		goto loop;
+		DISPATCH
 
-	case 0x83: // SAX
+	op_0x83: // SAX
 		IND_X
 		goto sax_imm;
 
-	case 0x97:
+	op_0x97:
 		data = uint8_t (data + y);
 		goto sax_imm;
 
-	case 0x8F:
+	op_0x8F:
 		ADD_PAGE
 
-	case 0x87:
+	op_0x87:
 	sax_imm:
 		WRITE( data, a & x );
 		pc++;
-		goto loop;
+		DISPATCH
 
-	case 0xCB: // SBX
+	op_0xCB: // SBX
 		data = ( a & x ) - data;
 		c = ( data <= 0xFF ) ? 0x100 : 0;
 		nz = x = uint8_t( data );
 		pc++;
-		goto loop;
+		DISPATCH
 
-	case 0x93: // SHA (ind),Y
+	op_0x93: // SHA (ind),Y
 		IND_Y(false,false)
 		pc++;
 		WRITE( data, uint8_t( a & x & ( ( data >> 8 ) + 1 ) ) );
-		goto loop;
+		DISPATCH
 
-	case 0x9F: { // SHA abs,Y
+	op_0x9F: { // SHA abs,Y
 		data += y;
 		int temp = data;
 		ADD_PAGE
 		READ( data - ( temp & 0x100 ) );
 		pc++;
 		WRITE( data, uint8_t( a & x & ( ( data >> 8 ) + 1 ) ) );
-		goto loop;
+		DISPATCH
 	}
 
-	case 0x9E: { // SHX abs,Y
+	op_0x9E: { // SHX abs,Y
 		data += y;
 		int temp = data;
 		ADD_PAGE
@@ -1151,10 +1174,10 @@ imm##op:                                \
 		pc++;
 		if ( !( temp & 0x100 ) )
 			WRITE( data, uint8_t( x & ( ( data >> 8 ) + 1 ) ) );
-		goto loop;
+		DISPATCH
 	}
 
-	case 0x9C: { // SHY abs,X
+	op_0x9C: { // SHY abs,X
 		data += x;
 		int temp = data;
 		ADD_PAGE
@@ -1162,10 +1185,10 @@ imm##op:                                \
 		pc++;
 		if ( !( temp & 0x100) )
 			WRITE( data, uint8_t( y & ( ( data >> 8 ) + 1 ) ) );
-		goto loop;
+		DISPATCH
 	}
 
-	case 0x9B: { // SHS abs,Y
+	op_0x9B: { // SHS abs,Y
 		data += y;
 		int temp = data;
 		ADD_PAGE
@@ -1173,10 +1196,10 @@ imm##op:                                \
 		pc++;
 		SET_SP( a & x );
 		WRITE( data, uint8_t( a & x & ( ( data >> 8 ) + 1 ) ) );
-		goto loop;
+		DISPATCH
 	}
 
-	case 0xBB: { // LAS abs,Y
+	op_0xBB: { // LAS abs,Y
 		data += y;
 		HANDLE_PAGE_CROSSING( data );
 		int temp = data;
@@ -1187,21 +1210,21 @@ imm##op:                                \
 		a = GET_SP();
 		x = a &= READ( data );
 		SET_SP( a );
-		goto loop;
+		DISPATCH
 	}
 
 // Unimplemented
 	
-	case page_wrap_opcode: // HLT
+	op_0xF2: // HLT
 		if ( pc > 0x10000 )
 		{
 			// handle wrap-around (assumes caller has put page of HLT at 0x10000)
 			pc = (pc - 1) & 0xFFFF;
 			clock_count -= 2;
-			goto loop;
+			DISPATCH
 		}
 		// fall through
-	default:
+	op_default:
 		// Unimplemented / illegal opcode. The original code intended to halt
 		// the CPU here (see the commented-out result_badop branch below), but
 		// that was disabled because several games rely on opcodes that fall
@@ -1220,7 +1243,7 @@ imm##op:                                \
 		pc += len - 1;
 		error_count_++;
 		effects++;
-		goto loop;
+		DISPATCH
 		
 		//result = result_badop; // intentionally unreachable; see comment above
 		goto stop;
PATCH

make -C "$build_root/core" -j "${HARMONY_QUICKNES_BUILD_JOBS:-4}" \
    DEBUG=0 OPTIMIZE=-O2 GIT_VERSION="$revision"

case $(uname -s) in
    Darwin) artifact=$build_root/core/quicknes_libretro.dylib ;;
    Linux) artifact=$build_root/core/quicknes_libretro.so ;;
    *) echo "unsupported QuickNES build host" >&2; exit 1 ;;
esac

cp "$artifact" "$output"
if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$output"
else
    shasum -a 256 "$output"
fi

if [ -n "$static_output" ]; then
    # QuickNES uses only C library calls plus C++ allocation/vtable machinery.
    # Disabling exceptions and RTTI keeps the host C++ runtime out of the
    # archive. Explicit target tools prevent a native arm64 build from mixing
    # an archive from one target with a runtime from another.
    static_cxx=${HARMONY_QUICKNES_CXX:-${CXX:-c++}}
    static_cc=${HARMONY_QUICKNES_CC:-${CC:-cc}}
    static_ar=${HARMONY_QUICKNES_AR:-${AR:-ar}}
    static_target_flags=${HARMONY_QUICKNES_CXXFLAGS:-${CXXFLAGS:-}}
    static_cxxflags="$static_target_flags -fno-exceptions -fno-rtti -fno-use-cxa-atexit"
    for static_tool in "$static_cc" "$static_cxx" "$static_ar"; do
        command -v "$static_tool" >/dev/null 2>&1 || {
            echo "FAIL: QuickNES static tool is not executable: $static_tool" >&2
            exit 1
        }
    done
    make -C "$build_root/core" clean >/dev/null

    # The pinned core has three C++-only header uses. The patched-musl wrapper
    # intentionally has no libstdc++ headers, so rewrite these exact sites to
    # their libc equivalents. This patch is fail-closed: a source move at the
    # pinned revision stops the build instead of silently changing the core.
    patch -d "$build_root/core" -f -p1 <<'PATCH'
diff --git a/nes_emu/Nes_Mapper.cpp b/nes_emu/Nes_Mapper.cpp
--- a/nes_emu/Nes_Mapper.cpp
+++ b/nes_emu/Nes_Mapper.cpp
@@ -6 +6 @@
-#include <cstdio>
+#include <stdio.h>
diff --git a/nes_emu/mappers/mapper009.hpp b/nes_emu/mappers/mapper009.hpp
--- a/nes_emu/mappers/mapper009.hpp
+++ b/nes_emu/mappers/mapper009.hpp
@@ -3 +3 @@
-#include <cstring>
+#include <string.h>
@@ -30 +30 @@
-		std::memset(regs, 0, sizeof(regs));
+		memset(regs, 0, sizeof(regs));
diff --git a/nes_emu/mappers/mapper010.hpp b/nes_emu/mappers/mapper010.hpp
--- a/nes_emu/mappers/mapper010.hpp
+++ b/nes_emu/mappers/mapper010.hpp
@@ -2 +2 @@
-#include <cstring>
+#include <string.h>
@@ -29 +29 @@
-		std::memset(regs, 0, sizeof(regs));
+		memset(regs, 0, sizeof(regs));
PATCH
    CXXFLAGS="$static_cxxflags" CC="$static_cc" CXX="$static_cxx" AR="$static_ar" \
        make -C "$build_root/core" -j "${HARMONY_QUICKNES_BUILD_JOBS:-4}" \
        DEBUG=0 OPTIMIZE=-O2 GIT_VERSION="$revision" \
        STATIC_LINKING=1 TARGET=libquicknes_libretro.a

    # The pinned core needs only allocation and pure-virtual failure hooks from
    # the C++ ABI. The shim uses the target libc and carries no host libstdc++.
    static_runtime_source=$script_dir/quicknes-static-runtime.cpp
    static_runtime_object=$build_root/quicknes-static-runtime.o
    [ -f "$static_runtime_source" ] || {
        echo "FAIL: QuickNES static runtime source is missing: $static_runtime_source" >&2
        exit 1
    }
    # Build variables intentionally contain a trusted whitespace-separated
    # compiler flag list, matching make's CXXFLAGS convention.
    # shellcheck disable=SC2086
    "$static_cxx" -x c++ $static_target_flags -O2 -fno-stack-protector \
        -fno-exceptions -fno-rtti -fno-use-cxa-atexit \
        -c "$static_runtime_source" -o "$static_runtime_object"
    "$static_ar" rcs "$build_root/core/libquicknes_libretro.a" "$static_runtime_object"
    cp "$build_root/core/libquicknes_libretro.a" "$static_output"
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$static_output"
    else
        shasum -a 256 "$static_output"
    fi
fi
