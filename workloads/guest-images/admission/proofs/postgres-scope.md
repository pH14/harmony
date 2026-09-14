# postgres component scope proposal

Exact rootfs manifest digest: 35082b0b7f874961adde1c6db70522c2b48cd6f05e1a4f3a02a4675271c0e7f6.
Exact archive/view digest: a47c03046ff1f7a3f1d0fb0f7ed5fdc361fa00df51a834b2e4688ce476083513.
Required labels: startup LD_BIND_NOW=1 before every exec; generated code forbidden;
writable executable memory forbidden; workloads controlled/trusted; loader
overrides forbidden. These are reviewed trusted-execution obligations, not
runtime enforcement claimed by the scanner. The prepared composition binds the
actual default SessionConfig (including pre-PID1 binding flags), argv, environment,
mounts and fixed ROM/SQL/config inputs. No custom session/command/extra process,
module or writable code substitution is supported. The OCI root remains writable.
