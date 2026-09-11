// SPDX-License-Identifier: AGPL-3.0-or-later

package main

import (
	"os"
	"path/filepath"
	"testing"
)

func writeBounds(t *testing.T, body string) string {
	t.Helper()
	path := filepath.Join(t.TempDir(), "bounds")
	if err := os.WriteFile(path, []byte(body), 0o644); err != nil {
		t.Fatal(err)
	}
	return path
}

func TestReadBoundsKeepsOneSpanPerWorker(t *testing.T) {
	bounds, err := readBounds(writeBounds(t, "1 5 9\n3 100 100\n\n"))
	if err != nil {
		t.Fatal(err)
	}
	want := []span{{worker: "1", low: 5, high: 9}, {worker: "3", low: 100, high: 100}}
	if len(bounds) != len(want) {
		t.Fatalf("got %d spans, want %d", len(bounds), len(want))
	}
	for index, got := range bounds {
		if got != want[index] {
			t.Errorf("span %d is %+v, want %+v", index, got, want[index])
		}
	}
}

func TestReadBoundsRefusesAMalformedLine(t *testing.T) {
	for _, body := range []string{"1 5\n", "1 5 9 13\n", "1 low 9\n", "1 5 high\n"} {
		if _, err := readBounds(writeBounds(t, body)); err == nil {
			t.Errorf("%q was accepted", body)
		}
	}
}

func TestAKeyIsZeroPaddedToItsNumericOrder(t *testing.T) {
	if got := key("2", 7); got != "museum/2/key-000000000007" {
		t.Errorf("got %q", got)
	}
	if key("2", 9) >= key("2", 10) {
		t.Error("byte order does not follow numeric order")
	}
}
