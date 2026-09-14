# nes component scope proposal

Exact rootfs manifest digest: 8c966242c6ae3b7679159538d0e4bb311ad78a4059cdff423db8ca82c1e334da.
Exact archive/view digest: d9e9d38da1d1a16be2b16276519432c5b7e2101953e820a62e23f85afb8099c5.
Required labels: startup LD_BIND_NOW=1 before every exec; generated code forbidden;
writable executable memory forbidden; workloads controlled/trusted; loader
overrides forbidden. These are reviewed trusted-execution obligations, not
runtime enforcement claimed by the scanner. The prepared composition binds the
actual default SessionConfig (including pre-PID1 binding flags), argv, environment,
mounts and fixed ROM/SQL/config inputs. No custom session/command/extra process,
module or writable code substitution is supported. The OCI root remains writable.
