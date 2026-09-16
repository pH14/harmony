// SPDX-License-Identifier: AGPL-3.0-or-later

&[
    ReviewedInputs {
        kernel: "7ce25244cf1d138db1286ce61fb1c880bd224b2866ffaebd1ec19d04b2ad69b6",
        initramfs: "b48c9f8eac32fc90f4681d997dbfe5217ca50a823db7627974518ecd3efc8fe8",
        ram_bytes: 256 << 20,
        cmdline: "console=ttyS0 panic=-1 reboot=t tsc=reliable no_timer_check lpj=4000000 random.trust_cpu=off nokaslr nosmp maxcpus=1 nox2apic hpet=disable harmony_pvclock noxsaveopt noxsaves LD_BIND_NOW=1",
    },
    ReviewedInputs {
        kernel: "7ce25244cf1d138db1286ce61fb1c880bd224b2866ffaebd1ec19d04b2ad69b6",
        initramfs: "075c8f52e17978602c7630fe807d39e643d7d2be590c4acb52093c87c50b2a5c",
        ram_bytes: 128 << 20,
        cmdline: "console=ttyS0 panic=-1 reboot=t tsc=reliable no_timer_check lpj=4000000 random.trust_cpu=off nokaslr nosmp maxcpus=1 nox2apic hpet=disable harmony_pvclock noxsaveopt noxsaves LD_BIND_NOW=1 rdinit=/init",
    },
    ReviewedInputs {
        kernel: "7ce25244cf1d138db1286ce61fb1c880bd224b2866ffaebd1ec19d04b2ad69b6",
        initramfs: "9ab0487109626b62c9d8588d62b831302d1a73cbe7c6858c7044404272e74d44",
        ram_bytes: 128 << 20,
        cmdline: "console=ttyS0 panic=-1 reboot=t tsc=reliable no_timer_check lpj=4000000 random.trust_cpu=off nokaslr nosmp maxcpus=1 nox2apic hpet=disable harmony_pvclock noxsaveopt noxsaves LD_BIND_NOW=1 rdinit=/init",
    },
]
