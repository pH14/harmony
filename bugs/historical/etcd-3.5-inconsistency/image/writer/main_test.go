// SPDX-License-Identifier: AGPL-3.0-or-later

package main

import (
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"sync"
	"testing"
)

func TestEntriesAreUniqueAcrossTheFourWriters(t *testing.T) {
	seen := make(map[string]string)
	for worker := 1; worker <= workers; worker++ {
		for sequence := 1; sequence <= 3; sequence++ {
			key, value := entry(worker, sequence)
			if previous, ok := seen[key]; ok {
				t.Fatalf("duplicate key %q already held %q", key, previous)
			}
			seen[key] = value
		}
	}
	if len(seen) != 12 {
		t.Fatalf("got %d unique entries, want 12", len(seen))
	}
}

// A windowed oracle reads back a span of recent keys with one range request,
// which only isolates that span when key order matches sequence order.
func TestKeyOrderFollowsSequenceOrder(t *testing.T) {
	var keys []string
	for _, sequence := range []int{1, 2, 9, 10, 11, 99, 100, 101, 1000} {
		key, _ := entry(1, sequence)
		keys = append(keys, key)
	}
	if !sort.StringsAreSorted(keys) {
		t.Fatalf("keys are not in byte order: %q", keys)
	}
}

func TestAppendJournalWritesAcknowledgedEntryFormat(t *testing.T) {
	journal, err := os.CreateTemp(t.TempDir(), "acked-")
	if err != nil {
		t.Fatal(err)
	}
	defer journal.Close()

	key, value := entry(1, 1)
	var journalMu sync.Mutex
	if err := appendJournal(journal, &journalMu, key, value); err != nil {
		t.Fatal(err)
	}
	if err := journal.Close(); err != nil {
		t.Fatal(err)
	}
	contents, err := os.ReadFile(journal.Name())
	if err != nil {
		t.Fatal(err)
	}
	if got, want := string(contents), key+"\t"+value+"\n"; got != want {
		t.Fatalf("journal = %q, want %q", got, want)
	}
}

func TestResumeSequencesContinuesPastEveryRecordedKey(t *testing.T) {
	path := filepath.Join(t.TempDir(), "acked")
	var contents string
	written := map[int]int{1: 7, 2: 1, 3: 42}
	for worker, highest := range written {
		for sequence := 1; sequence <= highest; sequence++ {
			key, value := entry(worker, sequence)
			contents += key + "\t" + value + "\n"
		}
	}
	if err := os.WriteFile(path, []byte(contents), 0o644); err != nil {
		t.Fatal(err)
	}

	resumed, err := resumeSequences(path)
	if err != nil {
		t.Fatal(err)
	}
	for worker, highest := range written {
		if resumed[worker] != highest {
			t.Fatalf("worker %d resumed at %d, want %d", worker, resumed[worker], highest)
		}
	}
	// A worker with no recorded key starts its sequence at 1.
	if resumed[4] != 0 {
		t.Fatalf("worker 4 resumed at %d, want 0", resumed[4])
	}
}

func TestResumeSequencesIgnoresATruncatedFinalRecord(t *testing.T) {
	path := filepath.Join(t.TempDir(), "acked")
	key, value := entry(1, 5)
	partial, _ := entry(1, 6)
	contents := key + "\t" + value + "\n" + partial + "\tvalu"
	if err := os.WriteFile(path, []byte(contents), 0o644); err != nil {
		t.Fatal(err)
	}

	resumed, err := resumeSequences(path)
	if err != nil {
		t.Fatal(err)
	}
	if resumed[1] != 5 {
		t.Fatalf("worker 1 resumed at %d, want 5", resumed[1])
	}
}

func TestResumeSequencesTreatsAMissingJournalAsEmpty(t *testing.T) {
	resumed, err := resumeSequences(filepath.Join(t.TempDir(), "absent"))
	if err != nil {
		t.Fatal(err)
	}
	if len(resumed) != 0 {
		t.Fatalf("resumed = %v, want empty", resumed)
	}
}

func TestParseEntryRejectsMalformedRecords(t *testing.T) {
	key, value := entry(2, 3)
	for _, line := range []string{
		"",
		"museum/2/key-000000000003",
		key + "\tvalue-2-000000000004",
		key + "\tvalue-3-000000000003",
		"museum/0/key-000000000001\tvalue-0-000000000001",
		fmt.Sprintf("museum/%d/key-000000000001\tvalue-%d-000000000001",
			workers+1, workers+1),
	} {
		if _, _, ok := parseEntry(line); ok {
			t.Fatalf("parseEntry accepted %q", line)
		}
	}
	worker, sequence, ok := parseEntry(key + "\t" + value)
	if !ok || worker != 2 || sequence != 3 {
		t.Fatalf("parseEntry = (%d, %d, %v), want (2, 3, true)", worker, sequence, ok)
	}
}
