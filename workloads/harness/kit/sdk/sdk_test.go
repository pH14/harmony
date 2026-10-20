// SPDX-License-Identifier: AGPL-3.0-or-later

package sdk

import (
	"bufio"
	"encoding/json"
	"os"
	"path/filepath"
	"testing"
)

func resetRandom(t *testing.T, seed string) {
	t.Helper()
	t.Setenv("HARNESS_SEED", seed)
	random.Lock()
	random.device = filepath.Join(t.TempDir(), "missing")
	random.native = nil
	random.Unlock()
}

func TestNativeRandomFollowsTheSeed(t *testing.T) {
	resetRandom(t, "42")
	first := []uint64{Random(), Random(), Random()}
	resetRandom(t, "42")
	second := []uint64{Random(), Random(), Random()}
	for index := range first {
		if first[index] != second[index] {
			t.Fatalf("draw %d differs: %d vs %d", index, first[index], second[index])
		}
	}
	resetRandom(t, "43")
	if Random() == first[0] {
		t.Fatal("a different seed repeated the first draw")
	}
	if Below(0) != 0 || Below(1) != 0 {
		t.Fatal("Below must stay inside its range")
	}
}

type decoded struct {
	Assert struct {
		Hit         bool            `json:"hit"`
		MustHit     bool            `json:"must_hit"`
		AssertType  string          `json:"assert_type"`
		DisplayType string          `json:"display_type"`
		Message     string          `json:"message"`
		Condition   bool            `json:"condition"`
		ID          string          `json:"id"`
		Details     json.RawMessage `json:"details"`
		Location    struct {
			File      string `json:"file"`
			Function  string `json:"function"`
			BeginLine int    `json:"begin_line"`
		} `json:"location"`
	} `json:"antithesis_assert"`
}

func TestAssertionsWriteOneRecordPerLine(t *testing.T) {
	dir := t.TempDir()
	t.Setenv("ANTITHESIS_OUTPUT_DIR", dir)
	Declare(KindAlways, "declared")
	Always(false, "always broken", map[string]any{"value": "a-1-2"})
	Sometimes(true, "sometimes seen", nil)
	Reachable("reached", nil)
	Unreachable("never", nil)

	file, err := os.Open(filepath.Join(dir, "sdk.jsonl"))
	if err != nil {
		t.Fatal(err)
	}
	defer file.Close()
	var records []decoded
	scanner := bufio.NewScanner(file)
	for scanner.Scan() {
		var record decoded
		if err := json.Unmarshal(scanner.Bytes(), &record); err != nil {
			t.Fatalf("line %q: %v", scanner.Text(), err)
		}
		records = append(records, record)
	}
	if len(records) != 5 {
		t.Fatalf("got %d records", len(records))
	}
	type want struct {
		assertType      string
		hit, must, cond bool
	}
	expected := []want{
		{"always", false, true, false},
		{"always", true, true, false},
		{"sometimes", true, true, true},
		{"reachability", true, true, true},
		{"reachability", true, false, true},
	}
	for index, record := range records {
		got := want{record.Assert.AssertType, record.Assert.Hit, record.Assert.MustHit, record.Assert.Condition}
		if got != expected[index] {
			t.Errorf("record %d = %+v, want %+v", index, got, expected[index])
		}
		if record.Assert.ID != record.Assert.Message || record.Assert.ID == "" {
			t.Errorf("record %d id %q message %q", index, record.Assert.ID, record.Assert.Message)
		}
		if record.Assert.Location.Function == "" || record.Assert.Location.BeginLine == 0 {
			t.Errorf("record %d has no caller location", index)
		}
	}
	if string(records[1].Assert.Details) != `{"value":"a-1-2"}` {
		t.Errorf("details = %s", records[1].Assert.Details)
	}
	if string(records[2].Assert.Details) != "null" {
		t.Errorf("empty details = %s", records[2].Assert.Details)
	}
}

func TestNoOutputDirectoryIsSilent(t *testing.T) {
	t.Setenv("ANTITHESIS_OUTPUT_DIR", "")
	Always(false, "nowhere", nil)
}
