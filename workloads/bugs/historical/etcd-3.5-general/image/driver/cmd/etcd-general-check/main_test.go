// SPDX-License-Identifier: AGPL-3.0-or-later

package main

import (
	"encoding/json"
	"errors"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"harmony-etcd-general/history"
)

type record struct {
	Assert struct {
		ID        string `json:"id"`
		Hit       bool   `json:"hit"`
		Condition bool   `json:"condition"`
	} `json:"antithesis_assert"`
}

func journalWithAcknowledgedPut(t *testing.T) string {
	dir := t.TempDir()
	lines := history.IntentLine(1, history.Put, "gen/0/0", "v-1") + history.AckLine(1, 10, true)
	if err := os.WriteFile(filepath.Join(dir, "general-0"), []byte(lines), 0o644); err != nil {
		t.Fatal(err)
	}
	return dir
}

func hits(t *testing.T, out string) map[string]bool {
	data, err := os.ReadFile(filepath.Join(out, "sdk.jsonl"))
	if err != nil {
		t.Fatal(err)
	}
	conditions := map[string]bool{}
	for _, line := range strings.Split(strings.TrimSpace(string(data)), "\n") {
		var r record
		if err := json.Unmarshal([]byte(line), &r); err != nil {
			t.Fatal(err)
		}
		if r.Assert.Hit {
			conditions[r.Assert.ID] = r.Assert.Condition
		}
	}
	return conditions
}

func noHashes([]string, []member) ([]hash, error) { return nil, errors.New("unused") }

func TestAMemberThatAnswersIsJudgedWhileAnotherIsDown(t *testing.T) {
	out := t.TempDir()
	t.Setenv("ANTITHESIS_OUTPUT_DIR", out)
	read := func(endpoint string) (member, error) {
		if endpoint == "down" {
			return member{}, errors.New("context deadline exceeded")
		}
		return member{endpoint: endpoint, revision: 20, values: map[string]history.Value{}}, nil
	}
	if err := run(journalWithAcknowledgedPut(t), []string{"down", "up"}, read, noHashes); err != nil {
		t.Fatal(err)
	}
	got := hits(t, out)
	if condition, ok := got[historyID]; !ok || condition {
		t.Fatalf("the lost acknowledged put was not reported: %v", got)
	}
	if !got[comparedID] {
		t.Fatalf("the partial comparison was not marked: %v", got)
	}
	if _, ok := got[everyID]; ok {
		t.Fatalf("a check with a member down claimed every member: %v", got)
	}
}

func TestNoAnsweringMemberIsInconclusive(t *testing.T) {
	out := t.TempDir()
	t.Setenv("ANTITHESIS_OUTPUT_DIR", out)
	read := func(string) (member, error) { return member{}, errors.New("refused") }
	if err := run(journalWithAcknowledgedPut(t), []string{"a", "b"}, read, noHashes); err == nil {
		t.Fatal("a check with no member was conclusive")
	}
	if _, err := os.Stat(filepath.Join(out, "sdk.jsonl")); !errors.Is(err, os.ErrNotExist) {
		t.Fatal("a check with no member evaluated an assertion")
	}
}

func TestEveryMemberAnsweringIsMarked(t *testing.T) {
	out := t.TempDir()
	t.Setenv("ANTITHESIS_OUTPUT_DIR", out)
	read := func(endpoint string) (member, error) {
		return member{endpoint: endpoint, revision: 20, values: map[string]history.Value{
			"gen/0/0": {Present: true, Data: "v-1", ModRevision: 10},
		}}, nil
	}
	if err := run(journalWithAcknowledgedPut(t), []string{"a"}, read, noHashes); err != nil {
		t.Fatal(err)
	}
	got := hits(t, out)
	if !got[comparedID] || !got[everyID] || !got[historyID] {
		t.Fatalf("a clean check of every member was not marked: %v", got)
	}
}
