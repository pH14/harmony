// SPDX-License-Identifier: AGPL-3.0-or-later

package main

import (
	"fmt"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"testing"
)

var cuts = []string{"partition", "isolate", "delay"}

// The supervisor treats every hook other than the bundle's heal hook as a
// fault that stays in force until the heal hook succeeds. A bundle that runs a
// harness-net cut without a heal line would leave the cut in force for the
// rest of the execution while checks compare replicas.
func bundleHealError(text string) error {
	hooks := map[string]string{}
	var cutIDs []string
	heal := ""
	for _, line := range strings.Split(text, "\n") {
		words := strings.Fields(line)
		switch {
		case len(words) >= 4 && words[0] == "hook" && filepath.Base(words[2]) == "harness-net":
			hooks[words[1]] = words[3]
			if slices.Contains(cuts, words[3]) {
				cutIDs = append(cutIDs, words[1])
			}
		case len(words) == 2 && words[0] == "heal":
			heal = words[1]
		}
	}
	if len(cutIDs) == 0 {
		return nil
	}
	if hooks[heal] != "heal" {
		return fmt.Errorf("hooks %s cut the network, but no heal line names a harness-net heal hook", strings.Join(cutIDs, ", "))
	}
	return nil
}

func TestTargetBundlesHealTheirCuts(t *testing.T) {
	paths, err := filepath.Glob("../../../targets/*/bundle*")
	if err != nil {
		t.Fatal(err)
	}
	if len(paths) == 0 {
		t.Skip("image builds copy the kit without the targets")
	}
	for _, path := range paths {
		text, err := os.ReadFile(path)
		if err != nil {
			t.Fatal(err)
		}
		if err := bundleHealError(string(text)); err != nil {
			t.Errorf("%s: %v", path, err)
		}
	}
}

func TestABundleWithACutAndNoHealIsRefused(t *testing.T) {
	healed := "node n1 /n\nhook 2 /opt/harmony/harness-net isolate\nhook 4 /opt/harmony/harness-net heal\nheal 4\n"
	if err := bundleHealError(healed); err != nil {
		t.Fatal(err)
	}
	for _, text := range []string{
		"node n1 /n\nhook 2 /opt/harmony/harness-net isolate\nhook 4 /opt/harmony/harness-net heal\n",
		"node n1 /n\nhook 1 /opt/harmony/harness-net partition\nhook 4 /bin/true\nheal 4\n",
	} {
		if bundleHealError(text) == nil {
			t.Errorf("accepted %q", text)
		}
	}
	if err := bundleHealError("node n1 /n\nhook 1 /hooks/put-batch\n"); err != nil {
		t.Fatal(err)
	}
}
