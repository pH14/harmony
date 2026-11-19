// SPDX-License-Identifier: AGPL-3.0-or-later

package main

import (
	"slices"
	"strings"
	"testing"
)

// applyRestore runs one iptables-restore --noflush input against a chain the
// way iptables does: a declared chain is emptied and the appended rules are
// visible only after COMMIT.
func applyRestore(t *testing.T, input string) []string {
	t.Helper()
	lines := strings.Split(strings.TrimSuffix(input, "\n"), "\n")
	if lines[0] != "*filter" || lines[len(lines)-1] != "COMMIT" {
		t.Fatalf("input is not one filter table commit: %q", input)
	}
	if lines[1] != ":"+chain+" - [0:0]" {
		t.Fatalf("input does not start by declaring %s: %q", chain, input)
	}
	next := []string{}
	for _, line := range lines[2 : len(lines)-1] {
		rule, ok := strings.CutPrefix(line, "-A "+chain+" ")
		if !ok {
			t.Fatalf("unexpected line %q", line)
		}
		next = append(next, rule)
	}
	return next
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
	oldSide, newSide := sideOf(1), sideOf(1, 2)
	chains := map[int][]string{}
	for i := 1; i <= n; i++ {
		chains[i] = applyRestore(t, restoreInput(i, n, oldSide))
	}
	if !linkDropped(chains[1], 3) || !linkDropped(chains[3], 1) {
		t.Fatalf("old partition does not cut n1-n3: %v", chains)
	}
	for i := 1; i <= n; i++ {
		chains[i] = applyRestore(t, restoreInput(i, n, newSide))
		if !linkDropped(chains[1], 3) || !linkDropped(chains[3], 1) {
			t.Fatalf("n1-n3 open after replacing node %d: %v", i, chains)
		}
	}
	if linkDropped(chains[1], 2) || linkDropped(chains[2], 1) {
		t.Fatalf("n1-n2 still cut after the new partition: %v", chains)
	}
	if !linkDropped(chains[2], 3) || !linkDropped(chains[3], 2) {
		t.Fatalf("n2-n3 not cut by the new partition: %v", chains)
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
