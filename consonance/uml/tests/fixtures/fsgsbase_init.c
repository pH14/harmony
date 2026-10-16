// SPDX-License-Identifier: AGPL-3.0-or-later

#include <stdint.h>

#define SYS_WRITE 1
#define SYS_OPEN 2
#define SYS_MOUNT 165
#define SYS_ARCH_PRCTL 158
#define SYS_REBOOT 169
#define SYS_EXIT_GROUP 231

#define ARCH_SET_GS 0x1001
#define ARCH_SET_FS 0x1002
#define ARCH_GET_FS 0x1003
#define ARCH_GET_GS 0x1004

#define LINUX_REBOOT_MAGIC1 0xfee1dead
#define LINUX_REBOOT_MAGIC2 672274793
#define LINUX_REBOOT_CMD_POWER_OFF 0x4321fedc

#define GS_MODELED 0x1000UL
#define GS_NATIVE 0x2000UL
#define FS_MODELED 0x3000UL
#define FS_NATIVE 0x4000UL

static long syscall2(long number, long first, long second)
{
	long result;
	__asm__ volatile("syscall" : "=a"(result)
			 : "a"(number), "D"(first), "S"(second)
			 : "rcx", "r11", "memory");
	return result;
}

static long syscall3(long number, long first, long second, long third)
{
	long result;
	__asm__ volatile("syscall" : "=a"(result)
			 : "a"(number), "D"(first), "S"(second), "d"(third)
			 : "rcx", "r11", "memory");
	return result;
}

static long syscall4(long number, long first, long second, long third,
		     long fourth)
{
	register long r10 __asm__("r10") = fourth;
	long result;
	__asm__ volatile("syscall" : "=a"(result)
			 : "a"(number), "D"(first), "S"(second), "d"(third),
			   "r"(r10)
			 : "rcx", "r11", "memory");
	return result;
}

static long syscall5(long number, long first, long second, long third,
		     long fourth, long fifth)
{
	register long r10 __asm__("r10") = fourth;
	register long r8 __asm__("r8") = fifth;
	long result;
	__asm__ volatile("syscall" : "=a"(result)
			 : "a"(number), "D"(first), "S"(second), "d"(third),
			   "r"(r10), "r"(r8)
			 : "rcx", "r11", "memory");
	return result;
}

static uint64_t read_gs_base(void)
{
	uint64_t value;
	__asm__ volatile(".byte 0xf3,0x48,0x0f,0xae,0xc8" : "=a"(value));
	return value;
}

static uint64_t read_fs_base(void)
{
	uint64_t value;
	__asm__ volatile(".byte 0xf3,0x48,0x0f,0xae,0xc0" : "=a"(value));
	return value;
}

static void write_gs_base(uint64_t value)
{
	__asm__ volatile(".byte 0xf3,0x48,0x0f,0xae,0xd8" : : "a"(value));
}

static void write_fs_base(uint64_t value)
{
	__asm__ volatile(".byte 0xf3,0x48,0x0f,0xae,0xd0" : : "a"(value));
}

static void power_off(void)
{
	syscall4(SYS_REBOOT, LINUX_REBOOT_MAGIC1, LINUX_REBOOT_MAGIC2,
		 LINUX_REBOOT_CMD_POWER_OFF, 0);
}

static int fail(const char *message, long length)
{
	syscall3(SYS_WRITE, 1, (long)message, length);
	power_off();
	return 1;
}

static unsigned long append(char *output, unsigned long at, const char *text)
{
	while (*text)
		output[at++] = *text++;
	return at;
}

static unsigned long append_hex(char *output, unsigned long at, uint64_t value)
{
	static const char digits[] = "0123456789abcdef";

	for (int index = 15; index >= 0; index--)
		output[at++] = digits[(value >> (index * 4)) & 15];
	return at;
}

static int emit(long fd, const char tag[8], uint64_t fs_direct,
		uint64_t fs_modeled, uint64_t gs_direct, uint64_t gs_modeled)
{
	char output[192];
	unsigned long at = 0;

	at = append(output, at, "{\"uml\":\"");
	for (int index = 0; index < 8; index++)
		output[at++] = tag[index];
	at = append(output, at, "\",\"direct_fs\":\"");
	at = append_hex(output, at, fs_direct);
	at = append(output, at, "\",\"modeled_fs\":\"");
	at = append_hex(output, at, fs_modeled);
	at = append(output, at, "\",\"direct_gs\":\"");
	at = append_hex(output, at, gs_direct);
	at = append(output, at, "\",\"modeled_gs\":\"");
	at = append_hex(output, at, gs_modeled);
	at = append(output, at, "\"}");
	return syscall3(SYS_WRITE, fd, (long)output, at) == (long)at ? 0 : -1;
}

__attribute__((used)) static int probe_main(void)
{
	uint64_t fs_modeled = 0, gs_modeled = 0;
	uint64_t fs_direct, gs_direct;
	long fd;

	if (syscall5(SYS_MOUNT, (long)"devtmpfs", (long)"/dev",
		     (long)"devtmpfs", 0, 0) != 0)
		return fail("FSGSBASE FAIL mount\n", sizeof("FSGSBASE FAIL mount\n") - 1);
	fd = syscall3(SYS_OPEN, (long)"/dev/harmony", 2, 0);
	if (fd < 0)
		return fail("FSGSBASE FAIL open\n", sizeof("FSGSBASE FAIL open\n") - 1);
	if (syscall2(SYS_ARCH_PRCTL, ARCH_SET_GS, GS_MODELED) != 0)
		return fail("FSGSBASE FAIL set-gs\n", sizeof("FSGSBASE FAIL set-gs\n") - 1);
	if (syscall2(SYS_ARCH_PRCTL, ARCH_SET_FS, FS_MODELED) != 0)
		return fail("FSGSBASE FAIL set-fs\n", sizeof("FSGSBASE FAIL set-fs\n") - 1);
	if (read_gs_base() != GS_MODELED)
		return fail("FSGSBASE FAIL initial-sync\n",
			    sizeof("FSGSBASE FAIL initial-sync\n") - 1);
	if (read_fs_base() != FS_MODELED)
		return fail("FSGSBASE FAIL initial-fs-sync\n",
			    sizeof("FSGSBASE FAIL initial-fs-sync\n") - 1);

	write_gs_base(GS_NATIVE);
	write_fs_base(FS_NATIVE);
	if (read_gs_base() != GS_NATIVE)
		return fail("FSGSBASE FAIL native-write\n",
			    sizeof("FSGSBASE FAIL native-write\n") - 1);
	if (read_fs_base() != FS_NATIVE)
		return fail("FSGSBASE FAIL native-fs-write\n",
			    sizeof("FSGSBASE FAIL native-fs-write\n") - 1);
	if (syscall2(SYS_ARCH_PRCTL, ARCH_GET_FS, (long)&fs_modeled) != 0)
		return fail("FSGSBASE FAIL get-fs\n", sizeof("FSGSBASE FAIL get-fs\n") - 1);
	if (syscall2(SYS_ARCH_PRCTL, ARCH_GET_GS, (long)&gs_modeled) != 0)
		return fail("FSGSBASE FAIL get-gs\n", sizeof("FSGSBASE FAIL get-gs\n") - 1);
	fs_direct = read_fs_base();
	gs_direct = read_gs_base();
	if (emit(fd, "FSGSPRE1", fs_direct, fs_modeled, gs_direct, gs_modeled) != 0)
		return fail("FSGSBASE FAIL pre-event\n",
			    sizeof("FSGSBASE FAIL pre-event\n") - 1);
	if (emit(fd, "FSGSBAR1", 0, 0, 0, 0) != 0)
		return fail("FSGSBASE FAIL capture-barrier\n",
			    sizeof("FSGSBASE FAIL capture-barrier\n") - 1);

	/* Read both native bases before any guest syscall after capture/restore. */
	fs_direct = read_fs_base();
	gs_direct = read_gs_base();
	fs_modeled = 0;
	gs_modeled = 0;
	if (syscall2(SYS_ARCH_PRCTL, ARCH_GET_FS, (long)&fs_modeled) != 0)
		return fail("FSGSBASE FAIL post-get-fs\n",
			    sizeof("FSGSBASE FAIL post-get-fs\n") - 1);
	if (syscall2(SYS_ARCH_PRCTL, ARCH_GET_GS, (long)&gs_modeled) != 0)
		return fail("FSGSBASE FAIL post-get-gs\n",
			    sizeof("FSGSBASE FAIL post-get-gs\n") - 1);
	if (emit(fd, "FSGSPST1", fs_direct, fs_modeled, gs_direct, gs_modeled) != 0)
		return fail("FSGSBASE FAIL post-event\n",
			    sizeof("FSGSBASE FAIL post-event\n") - 1);
	if (emit(fd, "FSGSEND1", 0, 0, 0, 0) != 0)
		return fail("FSGSBASE FAIL end-barrier\n",
			    sizeof("FSGSBASE FAIL end-barrier\n") - 1);

	syscall3(SYS_WRITE, 1, (long)"FSGSBASE DONE\n",
		 sizeof("FSGSBASE DONE\n") - 1);
	power_off();
	return 1;
}

__asm__(".global _start\n"
	"_start:\n"
	"xor %rbp, %rbp\n"
	"and $-16, %rsp\n"
	"call probe_main\n"
	"mov %eax, %edi\n"
	"mov $231, %eax\n"
	"syscall\n");
