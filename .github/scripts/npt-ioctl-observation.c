// SPDX-License-Identifier: AGPL-3.0-or-later
/*
 * Raw Linux KVM observation fixture for the AMD PAE/NPT snapshot witness.
 *
 * This program deliberately uses the KVM ioctl ABI directly.  It is a
 * diagnostic companion to the Rust fixture, not a second implementation of
 * snapshot/restore and not a qualification oracle.  Each case gets a fresh
 * no-irqchip VM.  The only read ioctl issued between changing the guest PDPT
 * in RAM and the next KVM_RUN is the case's selected ioctl.
 */

#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <inttypes.h>
#include <linux/kvm.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sched.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <unistd.h>

#include <asm/kvm.h>

#ifndef KVM_MAX_CPUID_ENTRIES
#define KVM_MAX_CPUID_ENTRIES 256
#endif

#ifndef KVM_SREGS2_FLAGS_PDPTRS_VALID
#define KVM_SREGS2_FLAGS_PDPTRS_VALID 1
#endif

enum {
    RAM_LEN = 4 * 1024 * 1024,
    PAGE_SIZE_FIXTURE = 4096,
    CODE_GPA = 0x1000,
    REMAPPED_CODE_GPA = 0x201000,
    DATA_GPA = 0x8000,
    REMAPPED_DATA_GPA = 0x208000,
    GDT_GPA = 0x7000,
    PDPT_GPA = 0x4000,
    PD_A_GPA = 0x5000,
    PD_B_GPA = 0x6000,
    UART_PORT = 0x3f8,
    WARMUP_MARKER = 0xa5,
    MAX_KVM_RUNS = 8,
};

static const uint64_t PDPT_A_ENTRY = 0x5001;
static const uint64_t PDPT_B_ENTRY = 0x6001;
static const uint64_t PD_A_ENTRY = 0x83;
static const uint64_t PD_B_ENTRY = 0x200083;

_Static_assert(sizeof(struct kvm_xsave) == 4096,
               "the diagnostic must use the legacy 4096-byte XSAVE ABI");

enum read_case {
    READ_NONE,
    READ_REGS,
    READ_SREGS,
    READ_SREGS2,
    READ_XSAVE,
};

struct selected_case {
    enum read_case kind;
    const char *name;
};

static const struct selected_case SELECTED_CASES[] = {
    {READ_NONE, "none"},
    {READ_REGS, "GET_REGS"},
    {READ_SREGS, "GET_SREGS"},
    {READ_SREGS2, "GET_SREGS2"},
    {READ_XSAVE, "GET_XSAVE"},
};

struct case_vm {
    int kvm_fd;
    int vm_fd;
    int vcpu_fd;
    size_t run_size;
    struct kvm_run *run;
    uint8_t *ram;
};

struct endpoint {
    struct kvm_regs regs;
    struct kvm_sregs2 sregs2;
    uint8_t second_serial;
    uint64_t pd_a;
    uint64_t pd_b;
};

static void die_text(const char *message)
{
    fprintf(stderr, "npt-ioctl-observation: %s\n", message);
    exit(EXIT_FAILURE);
}

static void die_errno(const char *operation)
{
    int error = errno;
    fprintf(stderr, "npt-ioctl-observation: %s: %s\n", operation,
            strerror(error));
    exit(EXIT_FAILURE);
}

static void require_true(int condition, const char *message)
{
    if (!condition)
        die_text(message);
}

static int ioctl_result(int fd, unsigned long request, void *argument,
                        const char *operation)
{
    int result = ioctl(fd, request, argument);
    if (result < 0)
        die_errno(operation);
    return result;
}

static int ioctl_noarg(int fd, unsigned long request, const char *operation)
{
    int result = ioctl(fd, request, 0UL);

    if (result < 0)
        die_errno(operation);
    return result;
}

static int ioctl_ulong(int fd, unsigned long request, unsigned long argument,
                       const char *operation)
{
    int result = ioctl(fd, request, argument);
    if (result < 0)
        die_errno(operation);
    return result;
}

static void *checked_mmap(void *address, size_t length, int protection,
                          int flags, int fd, off_t offset, const char *operation)
{
    void *mapped = mmap(address, length, protection, flags, fd, offset);
    if (mapped == MAP_FAILED)
        die_errno(operation);
    return mapped;
}

static void checked_close(int fd, const char *operation)
{
    if (close(fd) < 0)
        die_errno(operation);
}

static int pin_to_first_allowed_cpu(void)
{
    cpu_set_t allowed;
    cpu_set_t selected;
    int cpu;

    CPU_ZERO(&allowed);
    if (sched_getaffinity(0, sizeof(allowed), &allowed) < 0)
        die_errno("sched_getaffinity");
    cpu = -1;
    for (int candidate = 0; candidate < CPU_SETSIZE; ++candidate) {
        if (CPU_ISSET(candidate, &allowed)) {
            cpu = candidate;
            break;
        }
    }
    require_true(cpu >= 0, "sched_getaffinity returned an empty CPU set");

    CPU_ZERO(&selected);
    CPU_SET(cpu, &selected);
    if (sched_setaffinity(0, sizeof(selected), &selected) < 0)
        die_errno("sched_setaffinity");
    return cpu;
}

static void store_u64(uint8_t *ram, size_t offset, uint64_t value)
{
    require_true(offset <= RAM_LEN - sizeof(value), "guest RAM store overflow");
    memcpy(ram + offset, &value, sizeof(value));
}

static uint64_t load_u64(const uint8_t *ram, size_t offset)
{
    uint64_t value;

    require_true(offset <= RAM_LEN - sizeof(value), "guest RAM load overflow");
    memcpy(&value, ram + offset, sizeof(value));
    return value;
}

static void store_bytes(uint8_t *ram, size_t offset, const uint8_t *bytes,
                        size_t length)
{
    require_true(offset <= RAM_LEN && length <= RAM_LEN - offset,
                 "guest RAM byte store overflow");
    memcpy(ram + offset, bytes, length);
}

static void build_guest_image(uint8_t *ram)
{
    /*
     * mov edx, 0x3f8; mov al, 0xa5; out dx, al; mov al, [0x8000];
     * mov edx, 0x3f8; out dx, al; inc ebx; hlt
     *
     * The same instruction bytes live at the remapped physical address.  A
     * PAE 2-MiB mapping through PD B therefore makes the second UART byte
     * observable without another host-side device.
     */
    static const uint8_t program[] = {
        0xba, 0xf8, 0x03, 0x00, 0x00, 0xb0, WARMUP_MARKER, 0xee,
        0xa0, 0x00, 0x80, 0x00, 0x00, 0xba, 0xf8, 0x03, 0x00,
        0x00, 0xee, 0x43, 0xf4,
    };

    memset(ram, 0, RAM_LEN);
    store_bytes(ram, CODE_GPA, program, sizeof(program));
    store_bytes(ram, REMAPPED_CODE_GPA, program, sizeof(program));
    ram[DATA_GPA] = 0x42;
    ram[REMAPPED_DATA_GPA] = 0x99;

    store_u64(ram, PDPT_GPA, PDPT_A_ENTRY);
    store_u64(ram, PD_A_GPA, PD_A_ENTRY);
    store_u64(ram, PD_B_GPA, PD_B_ENTRY);

    /* A flat code/data GDT matching the Rust PAE fixture. */
    store_u64(ram, GDT_GPA, 0);
    store_u64(ram, GDT_GPA + 8, UINT64_C(0x00cf9b000000ffff));
    store_u64(ram, GDT_GPA + 16, UINT64_C(0x00cf93000000ffff));
}

static struct kvm_segment flat_segment(uint16_t selector, uint8_t type)
{
    struct kvm_segment segment;

    memset(&segment, 0, sizeof(segment));
    segment.limit = UINT32_MAX;
    segment.selector = selector;
    segment.type = type;
    segment.present = 1;
    segment.db = 1;
    segment.s = 1;
    segment.g = 1;
    return segment;
}

static void install_supported_cpuid(int kvm_fd, int vcpu_fd)
{
    size_t capacity = (size_t)KVM_MAX_CPUID_ENTRIES;
    size_t header_size = sizeof(struct kvm_cpuid2);
    size_t entries_size;
    struct kvm_cpuid2 *cpuid;

    require_true(capacity <= (SIZE_MAX - header_size) /
                              sizeof(struct kvm_cpuid_entry2),
                 "supported CPUID allocation overflow");
    entries_size = capacity * sizeof(struct kvm_cpuid_entry2);
    cpuid = calloc(1, header_size + entries_size);
    if (cpuid == NULL)
        die_errno("allocate supported CPUID");

    require_true(capacity <= UINT32_MAX, "supported CPUID count overflows u32");
    cpuid->nent = (uint32_t)capacity;
    if (ioctl(kvm_fd, KVM_GET_SUPPORTED_CPUID, cpuid) < 0)
        die_errno("KVM_GET_SUPPORTED_CPUID");
    require_true(cpuid->nent != 0 && cpuid->nent <= capacity,
                 "KVM_GET_SUPPORTED_CPUID returned an invalid count");
    if (ioctl(vcpu_fd, KVM_SET_CPUID2, cpuid) < 0)
        die_errno("KVM_SET_CPUID2");
    free(cpuid);
}

static void configure_vcpu(struct case_vm *vm)
{
    struct kvm_sregs2 sregs2;
    struct kvm_regs regs;
    struct kvm_mp_state mp_state;
    struct kvm_segment data;

    /*
     * This is the only initial-state read of SREGS2.  It supplies fields such
     * as the host's APIC base and reserved template values; configuration
     * below overwrites exactly the guest-visible PAE fields used by the Rust
     * fixture.
     */
    memset(&sregs2, 0, sizeof(sregs2));
    ioctl_result(vm->vcpu_fd, KVM_GET_SREGS2, &sregs2, "KVM_GET_SREGS2 template");

    data = flat_segment(0x10, 0x3);
    sregs2.cs = flat_segment(0x8, 0xb);
    sregs2.ds = data;
    sregs2.es = data;
    sregs2.fs = data;
    sregs2.gs = data;
    sregs2.ss = data;
    sregs2.gdt.base = GDT_GPA;
    sregs2.gdt.limit = 0x17;
    sregs2.cr0 = UINT64_C(0x80000011);
    sregs2.cr2 = 0;
    sregs2.cr3 = PDPT_GPA;
    sregs2.cr4 = UINT64_C(0x30);
    sregs2.efer = 0;
    sregs2.flags = KVM_SREGS2_FLAGS_PDPTRS_VALID;
    sregs2.pdptrs[0] = PDPT_A_ENTRY;
    sregs2.pdptrs[1] = 0;
    sregs2.pdptrs[2] = 0;
    sregs2.pdptrs[3] = 0;

    memset(&regs, 0, sizeof(regs));
    regs.rip = CODE_GPA;
    regs.rsp = RAM_LEN - PAGE_SIZE_FIXTURE;
    regs.rflags = 2;
    if (ioctl(vm->vcpu_fd, KVM_SET_REGS, &regs) < 0)
        die_errno("KVM_SET_REGS");
    if (ioctl(vm->vcpu_fd, KVM_SET_SREGS2, &sregs2) < 0)
        die_errno("KVM_SET_SREGS2");

    memset(&mp_state, 0, sizeof(mp_state));
    mp_state.mp_state = KVM_MP_STATE_RUNNABLE;
    if (ioctl(vm->vcpu_fd, KVM_SET_MP_STATE, &mp_state) < 0)
        die_errno("KVM_SET_MP_STATE");
}

static struct case_vm create_case_vm(void)
{
    struct case_vm vm = {
        .kvm_fd = -1,
        .vm_fd = -1,
        .vcpu_fd = -1,
        .run_size = 0,
        .run = MAP_FAILED,
        .ram = MAP_FAILED,
    };
    struct kvm_userspace_memory_region region;
    int api_version;
    int immediate_exit;
    int mmap_size;

    vm.kvm_fd = open("/dev/kvm", O_RDWR | O_CLOEXEC);
    if (vm.kvm_fd < 0)
        die_errno("open /dev/kvm");
    api_version = ioctl_noarg(vm.kvm_fd, KVM_GET_API_VERSION,
                              "KVM_GET_API_VERSION");
    require_true(api_version == KVM_API_VERSION,
                 "KVM API version does not match the userspace ABI");
    immediate_exit =
        ioctl_ulong(vm.kvm_fd, KVM_CHECK_EXTENSION, KVM_CAP_IMMEDIATE_EXIT,
                    "KVM_CHECK_EXTENSION(KVM_CAP_IMMEDIATE_EXIT)");
    require_true(immediate_exit > 0, "KVM_CAP_IMMEDIATE_EXIT is required");

    vm.vm_fd = ioctl_ulong(vm.kvm_fd, KVM_CREATE_VM, 0UL, "KVM_CREATE_VM");
    vm.vcpu_fd =
        ioctl_ulong(vm.vm_fd, KVM_CREATE_VCPU, 0UL, "KVM_CREATE_VCPU");
    mmap_size = ioctl_noarg(vm.kvm_fd, KVM_GET_VCPU_MMAP_SIZE,
                            "KVM_GET_VCPU_MMAP_SIZE");
    require_true(mmap_size > 0 && (size_t)mmap_size >= sizeof(struct kvm_run),
                 "KVM vCPU run mapping is smaller than struct kvm_run");
    vm.run_size = (size_t)mmap_size;
    vm.run = checked_mmap(NULL, vm.run_size, PROT_READ | PROT_WRITE, MAP_SHARED,
                          vm.vcpu_fd, 0, "mmap KVM run page");
    vm.ram = checked_mmap(NULL, RAM_LEN, PROT_READ | PROT_WRITE,
                          MAP_SHARED | MAP_ANONYMOUS, -1, 0, "mmap guest RAM");
    build_guest_image(vm.ram);

    memset(&region, 0, sizeof(region));
    region.slot = 0;
    region.guest_phys_addr = 0;
    region.memory_size = RAM_LEN;
    region.userspace_addr = (uint64_t)(uintptr_t)vm.ram;
    if (ioctl(vm.vm_fd, KVM_SET_USER_MEMORY_REGION, &region) < 0)
        die_errno("KVM_SET_USER_MEMORY_REGION");

    install_supported_cpuid(vm.kvm_fd, vm.vcpu_fd);
    configure_vcpu(&vm);
    return vm;
}

static void destroy_case_vm(struct case_vm *vm)
{
    if (vm->run != MAP_FAILED) {
        if (munmap(vm->run, vm->run_size) < 0)
            die_errno("munmap KVM run page");
        vm->run = MAP_FAILED;
    }
    if (vm->vcpu_fd >= 0) {
        checked_close(vm->vcpu_fd, "close vCPU");
        vm->vcpu_fd = -1;
    }
    if (vm->vm_fd >= 0) {
        checked_close(vm->vm_fd, "close VM");
        vm->vm_fd = -1;
    }
    if (vm->kvm_fd >= 0) {
        checked_close(vm->kvm_fd, "close KVM");
        vm->kvm_fd = -1;
    }
    if (vm->ram != MAP_FAILED) {
        if (munmap(vm->ram, RAM_LEN) < 0)
            die_errno("munmap guest RAM");
        vm->ram = MAP_FAILED;
    }
}

static void run_once(const struct case_vm *vm, const char *phase)
{
    int result = ioctl(vm->vcpu_fd, KVM_RUN, 0UL);

    if (result < 0) {
        char message[128];
        int written = snprintf(message, sizeof(message), "KVM_RUN (%s)", phase);

        if (written < 0 || (size_t)written >= sizeof(message))
            die_text("KVM_RUN phase message overflow");
        die_errno(message);
    }
    require_true(result == 0, "KVM_RUN returned an unexpected positive result");
}

static uint8_t pio_byte(const struct case_vm *vm, uint8_t direction,
                        const char *phase)
{
    uint64_t offset;
    uint64_t bytes;

    require_true(vm->run->exit_reason == KVM_EXIT_IO, "expected a KVM_EXIT_IO");
    require_true(vm->run->io.direction == direction, "unexpected PIO direction");
    require_true(vm->run->io.size == 1, "PIO width must be one byte");
    require_true(vm->run->io.count == 1, "PIO count must be one");
    require_true(vm->run->io.port == UART_PORT, "unexpected PIO port");

    offset = vm->run->io.data_offset;
    bytes = (uint64_t)vm->run->io.size * vm->run->io.count;
    require_true(offset <= (uint64_t)vm->run_size &&
                     bytes <= (uint64_t)vm->run_size - offset,
                 "PIO data range exceeds the KVM run mapping");
    if (phase == NULL)
        die_text("PIO phase label is required");
    return *((const uint8_t *)vm->run + (size_t)offset);
}

static void retire_first_pio(const struct case_vm *vm)
{
    int result;
    int saved_errno;

    /*
     * KVM_CAP_IMMEDIATE_EXIT makes this completion-only entry return EINTR
     * before executing the instruction following the OUT.  It is deliberately
     * one ioctl: a retry could cross the first post-warmup instruction.
     */
    vm->run->immediate_exit = 1;
    errno = 0;
    result = ioctl(vm->vcpu_fd, KVM_RUN, 0UL);
    saved_errno = errno;
    vm->run->immediate_exit = 0;
    require_true(result == -1 && saved_errno == EINTR,
                 "completion-only KVM_RUN must return EINTR");
}

static void selected_read(const struct case_vm *vm, enum read_case kind)
{
    switch (kind) {
    case READ_NONE:
        return;
    case READ_REGS: {
        struct kvm_regs regs;

        memset(&regs, 0, sizeof(regs));
        ioctl_result(vm->vcpu_fd, KVM_GET_REGS, &regs, "KVM_GET_REGS");
        return;
    }
    case READ_SREGS: {
        struct kvm_sregs sregs;

        memset(&sregs, 0, sizeof(sregs));
        ioctl_result(vm->vcpu_fd, KVM_GET_SREGS, &sregs, "KVM_GET_SREGS");
        return;
    }
    case READ_SREGS2: {
        struct kvm_sregs2 sregs2;

        memset(&sregs2, 0, sizeof(sregs2));
        ioctl_result(vm->vcpu_fd, KVM_GET_SREGS2, &sregs2, "KVM_GET_SREGS2");
        return;
    }
    case READ_XSAVE: {
        _Alignas(8) uint8_t xsave[4096];

        memset(xsave, 0, sizeof(xsave));
        ioctl_result(vm->vcpu_fd, KVM_GET_XSAVE, xsave, "KVM_GET_XSAVE");
        return;
    }
    }
    die_text("unknown selected ioctl case");
}

static struct endpoint run_case(struct case_vm *vm, enum read_case kind)
{
    struct endpoint endpoint;
    uint8_t first_serial;
    unsigned int run_count = 0;

    memset(&endpoint, 0, sizeof(endpoint));
    run_once(vm, "warmup PIO");
    ++run_count;
    first_serial = pio_byte(vm, KVM_EXIT_IO_OUT, "warmup");
    require_true(first_serial == WARMUP_MARKER,
                 "first PIO byte is not the fixed warmup marker");

    retire_first_pio(vm);
    store_u64(vm->ram, PDPT_GPA, PDPT_B_ENTRY);

    /* This call is the sole selected read between the mutation and next run. */
    selected_read(vm, kind);

    run_once(vm, "second PIO");
    ++run_count;
    endpoint.second_serial = pio_byte(vm, KVM_EXIT_IO_OUT, "second");
    run_once(vm, "HLT");
    ++run_count;
    require_true(vm->run->exit_reason == KVM_EXIT_HLT,
                 "the bounded guest must terminate at HLT");
    require_true(run_count <= MAX_KVM_RUNS, "guest exceeded bounded KVM_RUN count");

    ioctl_result(vm->vcpu_fd, KVM_GET_REGS, &endpoint.regs, "KVM_GET_REGS endpoint");
    ioctl_result(vm->vcpu_fd, KVM_GET_SREGS2, &endpoint.sregs2,
                 "KVM_GET_SREGS2 endpoint");
    endpoint.pd_a = load_u64(vm->ram, PD_A_GPA);
    endpoint.pd_b = load_u64(vm->ram, PD_B_GPA);
    require_true(endpoint.regs.rbx == 1,
                 "the endpoint must retire exactly one INC EBX");
    return endpoint;
}

static void print_endpoint(const struct selected_case *selected, int cpu,
                           const struct endpoint *endpoint)
{
    printf("{\"case\":\"%s\",\"cpu\":%d,\"serial_bytes\":[%u,%u],"
           "\"rip\":%" PRIu64 ",\"rbx\":%" PRIu64
           ",\"reported_pdptr_flags\":%" PRIu64
           ",\"reported_pdptrs\":[%" PRIu64 ",%" PRIu64 ",%" PRIu64
           ",%" PRIu64 "],\"pd_a\":%" PRIu64 ",\"pd_b\":%" PRIu64
           ",\"pio_count\":2,\"hlt\":true}\n",
           selected->name, cpu, (unsigned int)WARMUP_MARKER,
           (unsigned int)endpoint->second_serial,
           (uint64_t)endpoint->regs.rip, (uint64_t)endpoint->regs.rbx,
           (uint64_t)endpoint->sregs2.flags,
           (uint64_t)endpoint->sregs2.pdptrs[0],
           (uint64_t)endpoint->sregs2.pdptrs[1],
           (uint64_t)endpoint->sregs2.pdptrs[2],
           (uint64_t)endpoint->sregs2.pdptrs[3],
           endpoint->pd_a, endpoint->pd_b);
    if (fflush(stdout) == EOF)
        die_errno("flush observation JSON");
}

int main(void)
{
    int cpu = pin_to_first_allowed_cpu();
    size_t index;

    for (index = 0; index < sizeof(SELECTED_CASES) / sizeof(SELECTED_CASES[0]);
         ++index) {
        struct case_vm vm = create_case_vm();
        struct endpoint endpoint = run_case(&vm, SELECTED_CASES[index].kind);

        print_endpoint(&SELECTED_CASES[index], cpu, &endpoint);
        destroy_case_vm(&vm);
    }
    return EXIT_SUCCESS;
}
