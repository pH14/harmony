// SPDX-License-Identifier: AGPL-3.0-or-later

package main

import (
	"errors"
	"fmt"
	"math"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
	"syscall"

	"harmony.local/harness/kit/sdk"
)

const (
	bridge   = "hbr0"
	subnet   = "10.77.0"
	gateway  = subnet + ".254"
	chain    = "HARNESS"
	stateDir = "/run/harness-net"
	maxNodes = 16
)

func usage() {
	fmt.Fprintln(os.Stderr, "usage: harness-net setup N | exec I -- ARGV... | partition | isolate | delay | heal")
	os.Exit(2)
}

func main() {
	if len(os.Args) < 2 {
		usage()
	}
	var err error
	switch os.Args[1] {
	case "setup":
		if len(os.Args) != 3 {
			usage()
		}
		var n int
		n, err = strconv.Atoi(os.Args[2])
		if err == nil && (n < 2 || n > maxNodes) {
			err = fmt.Errorf("node count %d is outside 2..%d", n, maxNodes)
		}
		if err == nil {
			err = setup(n)
		}
	case "exec":
		if len(os.Args) < 5 || os.Args[3] != "--" {
			usage()
		}
		err = execIn(os.Args[2], os.Args[4:])
	case "partition":
		err = partition()
	case "isolate":
		err = isolate()
	case "delay":
		err = delay()
	case "heal":
		err = heal()
	default:
		usage()
	}
	if err != nil {
		fmt.Fprintln(os.Stderr, "harness-net:", err)
		os.Exit(1)
	}
}

func namespace(i int) string { return fmt.Sprintf("hn%d", i) }

func address(i int) string { return fmt.Sprintf("%s.%d", subnet, i) }

func run(argv ...string) error {
	out, err := exec.Command(argv[0], argv[1:]...).CombinedOutput()
	if err != nil {
		return fmt.Errorf("%s: %w: %s", strings.Join(argv, " "), err, strings.TrimSpace(string(out)))
	}
	return nil
}

func exists(argv ...string) bool {
	return exec.Command(argv[0], argv[1:]...).Run() == nil
}

func inNode(i int, argv ...string) error {
	return run(append([]string{"ip", "netns", "exec", namespace(i)}, argv...)...)
}

func setup(n int) error {
	if !exists("ip", "link", "show", bridge) {
		if err := run("ip", "link", "add", bridge, "type", "bridge"); err != nil {
			return err
		}
	}
	if err := run("ip", "addr", "replace", gateway+"/24", "dev", bridge); err != nil {
		return err
	}
	if err := run("ip", "link", "set", bridge, "up"); err != nil {
		return err
	}
	for i := 1; i <= n; i++ {
		ns, veth := namespace(i), fmt.Sprintf("hv%d", i)
		if !exists("ip", "netns", "exec", ns, "true") {
			if err := run("ip", "netns", "add", ns); err != nil {
				return err
			}
		}
		if !exists("ip", "link", "show", veth) {
			if err := run("ip", "link", "add", veth, "type", "veth", "peer", "name", "eth0", "netns", ns); err != nil {
				return err
			}
		}
		steps := [][]string{
			{"ip", "link", "set", veth, "master", bridge, "up"},
			{"ip", "-n", ns, "addr", "replace", address(i) + "/24", "dev", "eth0"},
			{"ip", "-n", ns, "link", "set", "eth0", "up"},
			{"ip", "-n", ns, "link", "set", "lo", "up"},
		}
		for _, step := range steps {
			if err := run(step...); err != nil {
				return err
			}
		}
		if !exists("ip", "netns", "exec", ns, "iptables", "-n", "-L", chain) {
			for _, step := range [][]string{
				{"iptables", "-N", chain},
				{"iptables", "-I", "INPUT", "-j", chain},
				{"iptables", "-I", "OUTPUT", "-j", chain},
			} {
				if err := inNode(i, step...); err != nil {
					return err
				}
			}
		}
	}
	if err := os.MkdirAll(stateDir, 0o755); err != nil {
		return err
	}
	return os.WriteFile(filepath.Join(stateDir, "nodes"), []byte(strconv.Itoa(n)+"\n"), 0o644)
}

func nodes() (int, error) {
	data, err := os.ReadFile(filepath.Join(stateDir, "nodes"))
	if err != nil {
		return 0, fmt.Errorf("run setup first: %w", err)
	}
	return strconv.Atoi(strings.TrimSpace(string(data)))
}

func execIn(index string, argv []string) error {
	i, err := strconv.Atoi(index)
	if err != nil || i < 1 || i > maxNodes {
		return fmt.Errorf("bad node index %q", index)
	}
	ip, err := exec.LookPath("ip")
	if err != nil {
		return err
	}
	return syscall.Exec(ip, append([]string{"ip", "netns", "exec", namespace(i)}, argv...), os.Environ())
}

func cut(n int, side func(int) bool) error {
	if err := clearRules(n); err != nil {
		return err
	}
	for i := 1; i <= n; i++ {
		for j := 1; j <= n; j++ {
			if side(i) == side(j) {
				continue
			}
			if err := inNode(i, "iptables", "-A", chain, "-s", address(j), "-j", "DROP"); err != nil {
				return err
			}
			if err := inNode(i, "iptables", "-A", chain, "-d", address(j), "-j", "DROP"); err != nil {
				return err
			}
		}
	}
	return nil
}

func partition() error {
	n, err := nodes()
	if err != nil {
		return err
	}
	mask := sdk.Below(uint64(1)<<n-2) + 1
	return cut(n, func(i int) bool { return mask&(1<<(i-1)) != 0 })
}

func isolate() error {
	n, err := nodes()
	if err != nil {
		return err
	}
	lone := int(sdk.Below(uint64(n))) + 1
	return cut(n, func(i int) bool { return i == lone })
}

func delay() error {
	n, err := nodes()
	if err != nil {
		return err
	}
	i := int(sdk.Below(uint64(n))) + 1
	unit := float64(sdk.Random()>>11) / float64(1<<53)
	ms := math.Exp(unit * math.Log(500))
	return inNode(i, "tc", "qdisc", "replace", "dev", "eth0", "root", "netem", "delay", fmt.Sprintf("%.1fms", ms))
}

func clearRules(n int) error {
	var errs []error
	for i := 1; i <= n; i++ {
		errs = append(errs, inNode(i, "iptables", "-F", chain))
	}
	return errors.Join(errs...)
}

func heal() error {
	n, err := nodes()
	if err != nil {
		return err
	}
	errs := []error{clearRules(n)}
	for i := 1; i <= n; i++ {
		_ = inNode(i, "tc", "qdisc", "del", "dev", "eth0", "root")
	}
	return errors.Join(errs...)
}
