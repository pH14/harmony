// SPDX-License-Identifier: AGPL-3.0-or-later

package main

import (
	"fmt"
	"slices"
	"strings"
	"testing"
)

// fakeNodes stands in for the node namespaces. It models the iptables
// commands harness-net may run against a HARNESS chain, treats one
// iptables-restore input as a single commit, and calls observe after every
// command so a test sees each state another process could see.
type fakeNodes struct {
	t        *testing.T
	chains   map[int][]string
	commands int
	observe  func()
}

func (f *fakeNodes) run(i int, input string, argv ...string) error {
	f.t.Helper()
	f.commands++
	switch {
	case slices.Equal(argv, []string{"iptables", "-F", chain}):
		f.chains[i] = nil
	case len(argv) > 2 && slices.Equal(argv[:3], []string{"iptables", "-A", chain}):
		f.chains[i] = append(slices.Clone(f.chains[i]), strings.Join(argv[3:], " "))
	case slices.Equal(argv, []string{"iptables-restore", "--noflush"}):
		f.chains[i] = parseRestore(f.t, input)
	default:
		return fmt.Errorf("unexpected command %v", argv)
	}
	f.observe()
	return nil
}

func parseRestore(t *testing.T, input string) []string {
	t.Helper()
	lines := strings.Split(strings.TrimSuffix(input, "\n"), "\n")
	if len(lines) < 3 || lines[0] != "*filter" || lines[len(lines)-1] != "COMMIT" {
		t.Fatalf("input is not one filter table commit: %q", input)
	}
	if lines[1] != ":"+chain+" - [0:0]" {
		t.Fatalf("input does not start by declaring %s: %q", chain, input)
	}
	rules := []string{}
	for _, line := range lines[2 : len(lines)-1] {
		rule, ok := strings.CutPrefix(line, "-A "+chain+" ")
		if !ok {
			t.Fatalf("unexpected line %q", line)
		}
		rules = append(rules, rule)
	}
	return rules
}

func sideOf(first ...int) func(int) bool {
	return func(i int) bool { return slices.Contains(first, i) }
}

func linkDropped(rules []string, peer int) bool {
	return slices.Contains(rules, "-s "+address(peer)+" -j DROP") &&
		slices.Contains(rules, "-d "+address(peer)+" -j DROP")
}

func TestReplaceKeepsSharedLinkDropped(t *testing.T) {
	const n = 3
	f := &fakeNodes{t: t, chains: map[int][]string{}}
	saved := nodeCommand
	nodeCommand = f.run
	t.Cleanup(func() { nodeCommand = saved })

	f.observe = func() {}
	if err := cut(n, sideOf(1)); err != nil {
		t.Fatal(err)
	}
	if !linkDropped(f.chains[1], 3) || !linkDropped(f.chains[3], 1) {
		t.Fatalf("old partition does not cut n1-n3: %v", f.chains)
	}

	f.commands = 0
	f.observe = func() {
		if !linkDropped(f.chains[1], 3) || !linkDropped(f.chains[3], 1) {
			t.Fatalf("n1-n3 open after command %d: %v", f.commands, f.chains)
		}
	}
	if err := cut(n, sideOf(1, 2)); err != nil {
		t.Fatal(err)
	}
	if f.commands != n {
		t.Fatalf("replace ran %d commands, want one per node", f.commands)
	}
	if linkDropped(f.chains[1], 2) || linkDropped(f.chains[2], 1) {
		t.Fatalf("n1-n2 still cut after the new partition: %v", f.chains)
	}
	if !linkDropped(f.chains[2], 3) || !linkDropped(f.chains[3], 2) {
		t.Fatalf("n2-n3 not cut by the new partition: %v", f.chains)
	}
}

func TestRestoreInputRules(t *testing.T) {
	want := "*filter\n:HARNESS - [0:0]\n" +
		"-A HARNESS -s 10.77.0.2 -j DROP\n-A HARNESS -d 10.77.0.2 -j DROP\n" +
		"-A HARNESS -s 10.77.0.3 -j DROP\n-A HARNESS -d 10.77.0.3 -j DROP\n" +
		"COMMIT\n"
	if got := restoreInput(1, 3, sideOf(1)); got != want {
		t.Fatalf("got %q, want %q", got, want)
	}
	empty := "*filter\n:HARNESS - [0:0]\nCOMMIT\n"
	if got := restoreInput(1, 3, sideOf(1, 2, 3)); got != empty {
		t.Fatalf("got %q, want %q", got, empty)
	}
}
