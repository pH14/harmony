// SPDX-License-Identifier: AGPL-3.0-or-later

package main

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestOnlyWorkloadShapedRecordsAreCompared(t *testing.T) {
	journal := strings.Join([]string{
		"museum/1/key-000000000007\tvalue-1-7",
		"museum/1/key-000000000007\tvalue-1-7",
		"museum/2/key-000000000009\tvalue-2-9",
		"museum/0/key-000000000001\tvalue-0-1",
		"museum/1/key-000000000008\tnot-a-value",
		"museum/1/key-000000000008",
		"",
	}, "\n")

	entries := selectEntries(journal)

	want := []string{
		"museum/1/key-000000000007\tvalue-1-7",
		"museum/2/key-000000000009\tvalue-2-9",
	}
	if fmt.Sprint(entries) != fmt.Sprint(want) {
		t.Fatalf("selectEntries = %q, want %q", entries, want)
	}
}

func TestOneSpanPerWorkerCoversThatWorkersWindow(t *testing.T) {
	bounds := windowBounds([]string{
		"museum/1/key-000000000004\tvalue-1-4",
		"museum/1/key-000000000009\tvalue-1-9",
		"museum/2/key-000000000003\tvalue-2-3",
	})

	want := []span{{worker: "1", low: 4, high: 9}, {worker: "2", low: 3, high: 3}}
	if fmt.Sprint(bounds) != fmt.Sprint(want) {
		t.Fatalf("windowBounds = %v, want %v", bounds, want)
	}
}

func TestAKeyIsZeroPaddedToItsNumericOrder(t *testing.T) {
	if got := key("3", 42); got != "museum/3/key-000000000042" {
		t.Fatalf("key = %q", got)
	}
	if key("3", 9) >= key("3", 10) {
		t.Fatal("byte order disagrees with numeric order")
	}
}

func TestMissingCountsAKeyHeldWithAnotherValue(t *testing.T) {
	expected := []string{
		"museum/1/key-000000000001\tvalue-1-1",
		"museum/1/key-000000000002\tvalue-1-2",
	}
	held := map[string]struct{}{
		"museum/1/key-000000000001\tvalue-1-1": {},
		"museum/1/key-000000000002\tvalue-1-9": {},
	}

	if got := countMissing(expected, held); got != 1 {
		t.Fatalf("countMissing = %d, want 1", got)
	}
}

func TestAnAbsentWatermarkStartsAtTheJournalHead(t *testing.T) {
	start, confirmed, err := readWatermark(filepath.Join(t.TempDir(), "absent"))
	if err != nil {
		t.Fatal(err)
	}
	if start != 0 || confirmed != 0 {
		t.Fatalf("readWatermark = %d, %d, want 0, 0", start, confirmed)
	}
}

func TestAMalformedWatermarkLineReadsAsZero(t *testing.T) {
	path := filepath.Join(t.TempDir(), "verified")
	if err := os.WriteFile(path, []byte("512\nnot-a-count\n"), 0o644); err != nil {
		t.Fatal(err)
	}

	start, confirmed, err := readWatermark(path)
	if err != nil {
		t.Fatal(err)
	}
	if start != 512 || confirmed != 0 {
		t.Fatalf("readWatermark = %d, %d, want 512, 0", start, confirmed)
	}
}

// A window that ends mid-record must stop at the last newline, so the next
// window starts on a boundary and the torn record is counted exactly once.
func TestATornFinalRecordIsLeftForTheNextWindow(t *testing.T) {
	whole := "museum/1/key-000000000001\tvalue-1-1\n"

	window, consumed := windowOf([]byte(whole + "museum/1/key-0000000"))

	if window != whole {
		t.Fatalf("window = %q, want %q", window, whole)
	}
	if consumed != int64(len(whole)) {
		t.Fatalf("consumed = %d, want %d", consumed, len(whole))
	}
}
