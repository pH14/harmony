# Coffee Lake baseline

These files contain the raw host observations used to derive the
`det-cfl-v1` CPU model in `../../intel.toml`.

| File | Contents |
|---|---|
| `cpuid-raw.txt` | Raw CPUID leaves and register values |
| `cpuid-decoded.txt` | Decoded CPUID output used to check feature names |
| `sysinfo.txt` | Kernel, processor, topology, and microcode information |
| `lscpu-microcode.txt` | Focused processor and microcode output |
| `msr-dump.txt` | Selected model-specific register values from CPU 0 |
| `msrs.txt` | Selected model-specific register values across the host |
| `mxcsr.txt` | The observed `MXCSR_MASK` value |

The active contract and its canonical-hash tests determine which captured
values remain part of the guest-visible model.
