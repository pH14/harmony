# platform component scope proposal

Exact rootfs manifest digest: 0d3282cfe336f2b385fc22979211260ccf3aefbc12e2686b39426e7e8121358d.
Exact archive/view digest: 44e520bf7db067310d49362a566da2cf36ead6df9867b1ee92cc948ed16849b0.
Required labels: startup LD_BIND_NOW=1 before every exec; generated code forbidden;
writable executable memory forbidden; workloads controlled/trusted; loader
overrides forbidden. These are reviewed trusted-execution obligations, not
runtime enforcement claimed by the scanner. The prepared composition binds the
actual default SessionConfig (including pre-PID1 binding flags), argv, environment,
mounts and fixed ROM/SQL/config inputs. No custom session/command/extra process,
module or writable code substitution is supported. The OCI root remains writable.
