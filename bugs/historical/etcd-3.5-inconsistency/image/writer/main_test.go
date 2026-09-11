// SPDX-License-Identifier: AGPL-3.0-or-later

package main

import (
	"os"
	"sync"
	"testing"
)

func TestEntriesAreUniqueAcrossTheFourWriters(t *testing.T) {
	seen := make(map[string]string)
	for worker := 1; worker <= 4; worker++ {
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

func TestAppendJournalWritesAcknowledgedEntryFormat(t *testing.T) {
	journal, err := os.CreateTemp(t.TempDir(), "acked-")
	if err != nil {
		t.Fatal(err)
	}
	defer journal.Close()

	var journalMu sync.Mutex
	if err := appendJournal(journal, &journalMu, "museum/1/key-1", "value-1-1"); err != nil {
		t.Fatal(err)
	}
	if err := journal.Close(); err != nil {
		t.Fatal(err)
	}
	contents, err := os.ReadFile(journal.Name())
	if err != nil {
		t.Fatal(err)
	}
	if got, want := string(contents), "museum/1/key-1\tvalue-1-1\n"; got != want {
		t.Fatalf("journal = %q, want %q", got, want)
	}
}
